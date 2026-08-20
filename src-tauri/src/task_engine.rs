use crate::{
    auth_artifact, catalog,
    chatgpt_client::{
        ChatGptPlan, ChatGptPlanLookup, ChatGptProbeResult, ChatGptProbeStatus, ChatGptProber,
    },
    cookie_artifact::{self, CookiePolicy, MAX_COOKIE_POLICY, TWITCH_COOKIE_POLICY},
    module_probe::{
        CookieModuleProber, ModulePlan, ModuleProbeResult, ModuleProbeStatus, ProbeControl,
    },
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};

const DEFAULT_MAX_FILES: usize = 10_000;
const DEFAULT_SCAN_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_FILES: usize = 100_000;
const MAX_RAW_ENTRIES: usize = 20_000;
const MAX_ENTRY_BYTES: usize = 32 * 1024;
const MAX_TOTAL_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_WORKERS: usize = 32;
const MAX_HISTORY: usize = 100;
const MAX_DELAY_MS: u64 = 60_000;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
const HISTORY_VERSION: u8 = 1;
const RESULTS_MARKER_FILE: &str = ".ayla-results";
const RESULTS_MARKER_CONTENT: &[u8] = b"AYLA_RESULTS_DIRECTORY_V1\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscoveryLimits {
    max_directories: Option<usize>,
    max_files: Option<usize>,
    scan_budget_bytes: Option<u64>,
}

impl DiscoveryLimits {
    pub(crate) fn new(
        max_directories: Option<u32>,
        max_files: Option<u32>,
        scan_budget_mib: Option<u32>,
    ) -> Self {
        Self {
            max_directories: max_directories.map(|limit| (limit as usize).max(1)),
            max_files: max_files.map(|limit| (limit as usize).clamp(1, HARD_MAX_FILES)),
            scan_budget_bytes: scan_budget_mib
                .map(|limit| u64::from(limit.max(1)).saturating_mul(1024 * 1024)),
        }
    }
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            max_directories: None,
            max_files: Some(DEFAULT_MAX_FILES),
            scan_budget_bytes: Some(DEFAULT_SCAN_BUDGET_BYTES),
        }
    }
}

fn deserialize_task_entries<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct EntriesVisitor;

    impl<'de> serde::de::Visitor<'de> for EntriesVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded list of local file paths")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut entries =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX_RAW_ENTRIES));
            let mut total_bytes = 0usize;
            while let Some(entry) = sequence.next_element::<String>()? {
                if entries.len() >= MAX_RAW_ENTRIES {
                    return Err(serde::de::Error::custom(format!(
                        "the limit is {MAX_RAW_ENTRIES} lines"
                    )));
                }
                if entry.len() > MAX_ENTRY_BYTES {
                    return Err(serde::de::Error::custom(format!(
                        "each line can contain at most {MAX_ENTRY_BYTES} bytes"
                    )));
                }
                total_bytes = total_bytes
                    .checked_add(entry.len())
                    .filter(|total| *total <= MAX_TOTAL_INPUT_BYTES)
                    .ok_or_else(|| {
                        serde::de::Error::custom(format!(
                            "the total input can contain at most {MAX_TOTAL_INPUT_BYTES} bytes"
                        ))
                    })?;
                entries.push(entry);
            }
            Ok(entries)
        }
    }

    deserializer.deserialize_seq(EntriesVisitor)
}

static TASK_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTaskRequest {
    pub module_id: String,
    #[serde(deserialize_with = "deserialize_task_entries")]
    pub entries: Vec<String>,
    pub concurrency: usize,
    pub delay_ms: u64,
    #[serde(default)]
    pub use_proxy: bool,
    #[serde(default)]
    pub output_directory: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}

impl TaskStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptTaskSummary {
    pub active: usize,
    pub authenticated_unknown: usize,
    pub plan_unavailable: usize,
    pub dead: usize,
    pub rate_limited: usize,
    pub errors: usize,
    pub invalid: usize,
    pub free: usize,
    pub go: usize,
    pub plus: usize,
    pub pro: usize,
    pub team: usize,
    pub enterprise: usize,
}

impl ChatGptTaskSummary {
    fn record_active(&mut self, plan: ChatGptPlan) {
        self.active = self.active.saturating_add(1);
        let counter = match plan {
            ChatGptPlan::Free => &mut self.free,
            ChatGptPlan::Go => &mut self.go,
            ChatGptPlan::Plus => &mut self.plus,
            ChatGptPlan::Pro => &mut self.pro,
            ChatGptPlan::Team => &mut self.team,
            ChatGptPlan::Enterprise => &mut self.enterprise,
        };
        *counter = counter.saturating_add(1);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleTaskSummary {
    pub active: usize,
    pub authenticated_unknown: usize,
    pub no_entitlement: usize,
    pub dead: usize,
    pub rate_limited: usize,
    pub errors: usize,
    pub invalid: usize,
    pub plans: BTreeMap<String, usize>,
}

impl ModuleTaskSummary {
    fn record_active(&mut self, plan: ModulePlan) {
        self.active = self.active.saturating_add(1);
        self.record_plan(plan);
    }

    fn record_no_entitlement(&mut self, plan: ModulePlan) {
        self.no_entitlement = self.no_entitlement.saturating_add(1);
        self.record_plan(plan);
    }

    fn record_authenticated(&mut self, plan: ModulePlan) {
        self.authenticated_unknown = self.authenticated_unknown.saturating_add(1);
        self.record_plan(plan);
    }

    fn record_plan(&mut self, plan: ModulePlan) {
        let counter = self.plans.entry(plan.label()).or_default();
        *counter = counter.saturating_add(1);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub run_id: String,
    pub module_id: String,
    pub status: TaskStatus,
    pub total: usize,
    pub discovered: usize,
    pub locally_filtered: usize,
    pub discovery_complete: bool,
    pub discovery_error: Option<String>,
    pub queued: usize,
    pub running: usize,
    pub processed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub retried: usize,
    pub percent: f64,
    pub concurrency: usize,
    pub requested_concurrency: usize,
    pub delay_ms: u64,
    pub use_proxy: bool,
    pub proxy_count: usize,
    pub sequence: u64,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub history_persisted: Option<bool>,
    pub results_export_enabled: bool,
    pub exported_active: usize,
    pub exported_failed: usize,
    pub export_errors: usize,
    pub chatgpt: Option<ChatGptTaskSummary>,
    pub module_summary: Option<ModuleTaskSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskHistoryEntry {
    pub run_id: String,
    pub module_id: String,
    pub status: TaskStatus,
    pub total: usize,
    #[serde(default)]
    pub discovered: usize,
    #[serde(default)]
    pub locally_filtered: usize,
    #[serde(default)]
    pub discovery_error: Option<String>,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub concurrency: usize,
    #[serde(default)]
    pub requested_concurrency: usize,
    pub delay_ms: u64,
    #[serde(default)]
    pub use_proxy: bool,
    #[serde(default)]
    pub proxy_count: usize,
    pub started_at: u64,
    pub finished_at: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub results_export_enabled: bool,
    #[serde(default)]
    pub exported_active: usize,
    #[serde(default)]
    pub exported_failed: usize,
    #[serde(default)]
    pub export_errors: usize,
}

enum PreparedArtifact {
    ChatGpt(auth_artifact::ChatGptArtifactPreparation),
    Cookie(Result<cookie_artifact::PreparedCookieArtifact, Option<Vec<u8>>>),
}

struct TaskInput {
    value: Arc<str>,
    ordinal: usize,
    artifact: Option<PreparedArtifact>,
}

impl TaskInput {
    #[cfg(test)]
    fn into_inner(self) -> Arc<str> {
        self.value
    }

    fn ordinal(&self) -> usize {
        self.ordinal
    }
}

#[derive(Clone)]
struct CancellationToken {
    state: Arc<CancellationState>,
}

struct CancellationState {
    cancelled: AtomicBool,
    wait_lock: Mutex<()>,
    wake: Condvar,
}

impl CancellationToken {
    fn new() -> Self {
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                wait_lock: Mutex::new(()),
                wake: Condvar::new(),
            }),
        }
    }

    fn cancel(&self) {
        // Hold the wait lock across the state change and notification so a waiter that has
        // evaluated the flag but not yet parked cannot miss the wake (a lost wakeup that
        // would otherwise leave it sleeping for the full inter-item delay).
        let _guard = lock_unpoison(&self.state.wait_lock);
        self.state.cancelled.store(true, Ordering::Release);
        self.state.wake.notify_all();
    }

    fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    fn wait_cancelled(&self, duration: Duration) -> bool {
        if duration.is_zero() || self.is_cancelled() {
            return self.is_cancelled();
        }

        let guard = lock_unpoison(&self.state.wait_lock);
        if self.is_cancelled() {
            return true;
        }

        let _ = self
            .state
            .wake
            .wait_timeout_while(guard, duration, |_| !self.is_cancelled())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.is_cancelled()
    }
}

#[derive(Clone)]
struct TaskContext {
    cancellation: CancellationToken,
    probe: Option<TaskProbe>,
}

impl TaskContext {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl ProbeControl for TaskContext {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    fn wait_cancelled(&self, duration: Duration) -> bool {
        self.cancellation.wait_cancelled(duration)
    }
}

#[derive(Clone)]
enum TaskProbe {
    ChatGpt(Arc<dyn ChatGptProber>),
    Cookie(Arc<dyn CookieModuleProber>),
}

#[derive(Clone, Copy)]
enum HandlerOutcome {
    Succeeded,
    Failed,
    Skipped,
    ChatGpt(ChatGptProbeResult),
    Module(ModuleProbeResult),
}

struct HandlerResult {
    outcome: HandlerOutcome,
    artifact_bytes: Option<Vec<u8>>,
}

impl HandlerResult {
    fn without_artifact(outcome: HandlerOutcome) -> Self {
        Self {
            outcome,
            artifact_bytes: None,
        }
    }
}

enum WorkerMessage {
    CandidateReady,
    CandidateFiltered,
    DiscoveryComplete,
    DiscoveryFailed(String),
    Started,
    Finished(HandlerOutcome, ExportRecord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportRecord {
    None,
    Active,
    Failed,
    Error,
}

struct ResultExporter {
    active_directory: PathBuf,
    failed_directory: PathBuf,
    run_tag: String,
}

impl ResultExporter {
    fn new(root: &Path, module_id: &str, run_id: &str) -> Result<Self, String> {
        let trusted_root = root.to_path_buf();
        let root_metadata = fs::symlink_metadata(&trusted_root)
            .map_err(|_| "unable to inspect the selected results directory".to_string())?;
        if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
            return Err("the selected results path must be a regular directory".to_string());
        }
        let module_directory = trusted_root.join(module_id);
        ensure_output_directory(&module_directory)?;
        let active_directory = module_directory.join("active");
        let failed_directory = module_directory.join("failed");
        ensure_output_directory(&active_directory)?;
        ensure_output_directory(&failed_directory)?;
        let active_directory = fs::canonicalize(active_directory)
            .map_err(|_| "unable to resolve the active results directory".to_string())?;
        let failed_directory = fs::canonicalize(failed_directory)
            .map_err(|_| "unable to resolve the failed results directory".to_string())?;
        if !resolved_path_is_within(&active_directory, &trusted_root)
            || !resolved_path_is_within(&failed_directory, &trusted_root)
        {
            return Err("the result directories must stay inside the selected folder".to_string());
        }
        ensure_results_marker(&trusted_root)?;

        let run_prefix: String = run_id
            .trim_start_matches("task_")
            .chars()
            .filter(|character| character.is_ascii_hexdigit())
            .take(12)
            .collect();
        let run_tag = format!("{run_prefix}-p{:x}", std::process::id());

        Ok(Self {
            active_directory,
            failed_directory,
            run_tag,
        })
    }

    fn export(
        &self,
        ordinal: usize,
        outcome: HandlerOutcome,
        artifact_bytes: Option<&[u8]>,
    ) -> ExportRecord {
        let Some((active, plan, reason)) = export_classification(outcome) else {
            return ExportRecord::None;
        };
        let Some(artifact_bytes) = artifact_bytes else {
            return ExportRecord::Error;
        };
        let directory = if active {
            &self.active_directory
        } else {
            &self.failed_directory
        };
        let file_name = match reason {
            Some(reason) => format!("{plan}__{reason}__{}__{:06}.txt", self.run_tag, ordinal),
            None => format!("{plan}__{}__{:06}.txt", self.run_tag, ordinal),
        };

        match copy_result_file(artifact_bytes, directory, &directory.join(file_name)) {
            Ok(()) if active => ExportRecord::Active,
            Ok(()) => ExportRecord::Failed,
            Err(()) => ExportRecord::Error,
        }
    }
}

fn export_classification(outcome: HandlerOutcome) -> Option<(bool, String, Option<&'static str>)> {
    match outcome {
        HandlerOutcome::Succeeded => Some((true, "unknown-plan".to_string(), None)),
        HandlerOutcome::Failed => Some((false, "unknown-plan".to_string(), Some("invalid"))),
        HandlerOutcome::Skipped => None,
        HandlerOutcome::ChatGpt(result) => match result.status {
            ChatGptProbeStatus::Active(plan) => {
                Some((true, ModulePlan::ChatGpt(plan).slug(), None))
            }
            ChatGptProbeStatus::Authenticated(ChatGptPlanLookup::Known(plan)) => {
                Some((true, ModulePlan::ChatGpt(plan).slug(), None))
            }
            ChatGptProbeStatus::Authenticated(ChatGptPlanLookup::Unknown) => {
                Some((true, "unknown-plan".to_string(), Some("plan-unknown")))
            }
            ChatGptProbeStatus::Authenticated(ChatGptPlanLookup::Unavailable) => {
                Some((true, "unknown-plan".to_string(), Some("plan-unavailable")))
            }
            ChatGptProbeStatus::Dead => Some((false, "unknown-plan".to_string(), Some("dead"))),
            ChatGptProbeStatus::RateLimited => {
                Some((false, "unknown-plan".to_string(), Some("rate-limited")))
            }
            ChatGptProbeStatus::Error => Some((false, "unknown-plan".to_string(), Some("error"))),
        },
        HandlerOutcome::Module(result) => match result.status {
            ModuleProbeStatus::Active(plan) => Some((true, plan.slug(), None)),
            ModuleProbeStatus::Authenticated(plan) => {
                Some((true, plan.slug(), Some("plan-unavailable")))
            }
            ModuleProbeStatus::NoEntitlement(plan) => {
                Some((false, plan.slug(), Some("no-entitlement")))
            }
            ModuleProbeStatus::Dead => Some((false, "unknown-plan".to_string(), Some("dead"))),
            ModuleProbeStatus::RateLimited => {
                Some((false, "unknown-plan".to_string(), Some("rate-limited")))
            }
            ModuleProbeStatus::Error => Some((false, "unknown-plan".to_string(), Some("error"))),
        },
    }
}

fn ensure_output_directory(path: &Path) -> Result<(), String> {
    if !path.exists() {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err("unable to create the selected results directory".to_string());
            }
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "unable to inspect the selected results directory".to_string())?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err("the selected results path must contain regular directories".to_string());
    }
    Ok(())
}

fn copy_result_file(
    contents: &[u8],
    destination_directory: &Path,
    destination_path: &Path,
) -> Result<(), ()> {
    if contents.len() as u64 > auth_artifact::MAX_ARTIFACT_BYTES || destination_path.exists() {
        return Err(());
    }
    let final_name = destination_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(())?;
    let partial_path = destination_directory.join(format!(".ayla-{final_name}.part"));
    let mut partial = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&partial_path)
        .map_err(|_| ())?;
    if !auth_artifact::opened_file_is_within_local_directory(&partial, destination_directory)
        || partial
            .write_all(contents)
            .and_then(|_| partial.sync_all())
            .is_err()
    {
        drop(partial);
        let _ = fs::remove_file(&partial_path);
        return Err(());
    }
    drop(partial);

    let published = publish_without_overwrite(&partial_path, destination_path);

    if published.is_err() {
        let _ = fs::remove_file(&partial_path);
        return Err(());
    }
    Ok(())
}

#[cfg(windows)]
fn publish_without_overwrite(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "MoveFileW"]
        fn move_file_w(existing_file_name: *const u16, new_file_name: *const u16) -> i32;
    }

    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both buffers are valid, nul-terminated UTF-16 paths for the duration of the call.
    // MoveFileW is intentionally used instead of MoveFileExW so an existing destination fails.
    if unsafe { move_file_w(source.as_ptr(), destination.as_ptr()) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn publish_without_overwrite(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)?;
    if let Err(error) = fs::remove_file(source) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

trait TaskHandler: Send + Sync + 'static {
    fn prepares_artifacts(&self) -> bool {
        false
    }

    fn process(&self, module_id: &str, input: TaskInput, context: &TaskContext) -> HandlerResult;
}

struct ModuleInspectionHandler;

impl TaskHandler for ModuleInspectionHandler {
    fn prepares_artifacts(&self) -> bool {
        true
    }

    fn process(&self, module_id: &str, input: TaskInput, context: &TaskContext) -> HandlerResult {
        if context.is_cancelled() {
            return HandlerResult::without_artifact(HandlerOutcome::Skipped);
        }

        let TaskInput {
            value, artifact, ..
        } = input;
        let result = match (module_id, artifact) {
            ("chatgpt", Some(PreparedArtifact::ChatGpt(preparation))) => match preparation {
                auth_artifact::ChatGptArtifactPreparation::Ready(auth) => {
                    let outcome = match context.probe.as_ref() {
                        Some(TaskProbe::ChatGpt(probe)) => {
                            HandlerOutcome::ChatGpt(probe.check(&auth, context))
                        }
                        None => HandlerOutcome::Succeeded,
                        Some(TaskProbe::Cookie(_)) => HandlerOutcome::Failed,
                    };
                    HandlerResult {
                        outcome,
                        artifact_bytes: Some(auth.into_artifact_bytes()),
                    }
                }
                auth_artifact::ChatGptArtifactPreparation::Rejected {
                    reason,
                    artifact_bytes,
                } => {
                    let _ = reason;
                    HandlerResult {
                        outcome: HandlerOutcome::Failed,
                        artifact_bytes,
                    }
                }
            },
            (_, Some(PreparedArtifact::Cookie(preparation)))
                if cookie_policy_for_module(module_id).is_some() =>
            {
                match preparation {
                    Ok(artifact) => {
                        let outcome = match context.probe.as_ref() {
                            Some(TaskProbe::Cookie(probe)) => {
                                HandlerOutcome::Module(probe.check(&artifact, context))
                            }
                            None => HandlerOutcome::Succeeded,
                            Some(TaskProbe::ChatGpt(_)) => HandlerOutcome::Failed,
                        };
                        HandlerResult {
                            outcome,
                            artifact_bytes: Some(artifact.into_artifact_bytes()),
                        }
                    }
                    Err(artifact_bytes) => HandlerResult {
                        outcome: HandlerOutcome::Failed,
                        artifact_bytes,
                    },
                }
            }
            _ => HandlerResult::without_artifact(HandlerOutcome::Failed),
        };
        drop(value);

        if context.is_cancelled() {
            HandlerResult::without_artifact(HandlerOutcome::Skipped)
        } else {
            result
        }
    }
}

fn cookie_policy_for_module(module_id: &str) -> Option<CookiePolicy> {
    match module_id {
        "twitch" => Some(TWITCH_COOKIE_POLICY),
        "max" => Some(MAX_COOKIE_POLICY),
        _ => None,
    }
}

#[cfg(test)]
struct PreflightHandler;

#[cfg(test)]
impl TaskHandler for PreflightHandler {
    fn process(&self, _module_id: &str, input: TaskInput, context: &TaskContext) -> HandlerResult {
        if context.is_cancelled() {
            return HandlerResult::without_artifact(HandlerOutcome::Skipped);
        }

        let valid = !input.into_inner().is_empty();
        if context.is_cancelled() {
            HandlerResult::without_artifact(HandlerOutcome::Skipped)
        } else if valid {
            HandlerResult::without_artifact(HandlerOutcome::Succeeded)
        } else {
            HandlerResult::without_artifact(HandlerOutcome::Failed)
        }
    }
}

trait TaskEventSink: Send + Sync + 'static {
    fn progress(&self, snapshot: TaskSnapshot);
    fn done(&self, snapshot: TaskSnapshot);
}

struct TauriTaskEventSink {
    app: AppHandle,
}

impl TaskEventSink for TauriTaskEventSink {
    fn progress(&self, snapshot: TaskSnapshot) {
        let _ = self.app.emit("task:progress", snapshot);
    }

    fn done(&self, snapshot: TaskSnapshot) {
        let _ = self.app.emit("task:done", snapshot);
    }
}

struct ActiveTask {
    snapshot: TaskSnapshot,
    cancellation: CancellationToken,
    last_progress_event: Instant,
}

struct EngineState {
    active: Option<ActiveTask>,
    history: VecDeque<TaskHistoryEntry>,
}

struct EngineInner {
    history_path: PathBuf,
    lifecycle: Mutex<()>,
    state: Mutex<EngineState>,
    history_io: Mutex<()>,
    handler: Arc<dyn TaskHandler>,
    events: Arc<dyn TaskEventSink>,
}

pub struct TaskEngine {
    inner: Arc<EngineInner>,
}

impl TaskEngine {
    pub fn open(history_path: PathBuf, app: AppHandle) -> Self {
        Self::with_components(
            history_path,
            Arc::new(ModuleInspectionHandler),
            Arc::new(TauriTaskEventSink { app }),
        )
    }

    fn with_components(
        history_path: PathBuf,
        handler: Arc<dyn TaskHandler>,
        events: Arc<dyn TaskEventSink>,
    ) -> Self {
        let history = load_history(&history_path);
        Self {
            inner: Arc::new(EngineInner {
                history_path,
                lifecycle: Mutex::new(()),
                state: Mutex::new(EngineState {
                    active: None,
                    history,
                }),
                history_io: Mutex::new(()),
                handler,
                events,
            }),
        }
    }

    #[cfg(test)]
    pub fn start(&self, request: StartTaskRequest) -> Result<TaskSnapshot, String> {
        self.start_with_probe(request, None, 0, DiscoveryLimits::default())
    }

    pub(crate) fn start_with_chatgpt_probe(
        &self,
        request: StartTaskRequest,
        probe: Arc<dyn ChatGptProber>,
        proxy_count: usize,
        discovery_limits: DiscoveryLimits,
    ) -> Result<TaskSnapshot, String> {
        self.start_with_probe(
            request,
            Some(TaskProbe::ChatGpt(probe)),
            proxy_count,
            discovery_limits,
        )
    }

    pub(crate) fn start_with_cookie_probe(
        &self,
        request: StartTaskRequest,
        probe: Arc<dyn CookieModuleProber>,
        proxy_count: usize,
        discovery_limits: DiscoveryLimits,
    ) -> Result<TaskSnapshot, String> {
        self.start_with_probe(
            request,
            Some(TaskProbe::Cookie(probe)),
            proxy_count,
            discovery_limits,
        )
    }

    fn start_with_probe(
        &self,
        request: StartTaskRequest,
        probe: Option<TaskProbe>,
        proxy_count: usize,
        discovery_limits: DiscoveryLimits,
    ) -> Result<TaskSnapshot, String> {
        // Serialize starts without holding the state mutex across filesystem validation,
        // exporter creation, or sync operations. Read/cancel APIs remain responsive while
        // a new run is being prepared.
        let _lifecycle = lock_unpoison(&self.inner.lifecycle);
        {
            let state = lock_unpoison(&self.inner.state);
            if state.active.is_some() {
                return Err("a task is already running".to_string());
            }
        }

        let PreparedTask {
            module_id,
            roots,
            requested_concurrency,
            delay_ms,
            use_proxy,
            output_directory,
            discovery_limits,
        } = prepare_with_limits(request, discovery_limits)?;

        if use_proxy && proxy_count == 0 {
            return Err("no active proxy is available".to_string());
        }
        let concurrency = effective_concurrency(requested_concurrency, use_proxy, proxy_count);

        let run_id = new_task_id();
        let cancellation = CancellationToken::new();
        let snapshot = TaskSnapshot {
            run_id: run_id.clone(),
            module_id: module_id.clone(),
            status: TaskStatus::Running,
            total: 0,
            discovered: 0,
            locally_filtered: 0,
            discovery_complete: false,
            discovery_error: None,
            queued: 0,
            running: 0,
            processed: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            retried: 0,
            percent: 0.0,
            concurrency,
            requested_concurrency,
            delay_ms,
            use_proxy,
            proxy_count: if use_proxy { proxy_count } else { 0 },
            sequence: 1,
            started_at: now_millis(),
            finished_at: None,
            history_persisted: None,
            results_export_enabled: output_directory.is_some(),
            exported_active: 0,
            exported_failed: 0,
            export_errors: 0,
            chatgpt: (module_id == "chatgpt").then(ChatGptTaskSummary::default),
            module_summary: Some(ModuleTaskSummary::default()),
        };

        let result_exporter = output_directory
            .as_deref()
            .map(|directory| ResultExporter::new(directory, &module_id, &run_id))
            .transpose()?
            .map(Arc::new);

        {
            let mut state = lock_unpoison(&self.inner.state);
            // Starts are serialized by `lifecycle`; retain the check so this remains
            // fail-closed if another installation path is introduced later.
            if state.active.is_some() {
                return Err("a task is already running".to_string());
            }
            state.active = Some(ActiveTask {
                snapshot: snapshot.clone(),
                cancellation: cancellation.clone(),
                last_progress_event: Instant::now(),
            });
        }

        self.inner.events.progress(snapshot.clone());

        // Cookie-backed network probes own their global request limiter so retries and
        // proxy failover are spaced as well. Other handlers keep the existing per-worker
        // delay. The requested value remains visible in the snapshot either way.
        let worker_delay_ms = if matches!(&probe, Some(TaskProbe::Cookie(_))) {
            0
        } else {
            delay_ms
        };
        let inner = Arc::clone(&self.inner);
        let worker_run_id = run_id.clone();
        let spawn_result = thread::Builder::new()
            .name(format!("ayla-task-{run_id}"))
            .spawn(move || {
                run_task(
                    inner,
                    worker_run_id,
                    module_id,
                    roots,
                    concurrency,
                    worker_delay_ms,
                    probe,
                    cancellation,
                    result_exporter,
                    discovery_limits,
                    output_directory,
                );
            });

        if let Err(error) = spawn_result {
            self.inner.finish(&run_id, true);
            return Err(format!("unable to start the task: {error}"));
        }

        Ok(snapshot)
    }

    pub fn list_active(&self) -> Vec<TaskSnapshot> {
        lock_unpoison(&self.inner.state)
            .active
            .as_ref()
            .map(|active| vec![active.snapshot.clone()])
            .unwrap_or_default()
    }

    pub fn get_active(&self, run_id: &str) -> Option<TaskSnapshot> {
        lock_unpoison(&self.inner.state)
            .active
            .as_ref()
            .filter(|active| active.snapshot.run_id == run_id)
            .map(|active| active.snapshot.clone())
    }

    pub fn cancel(&self, run_id: &str) -> Option<TaskSnapshot> {
        let (cancellation, snapshot) = {
            let mut state = lock_unpoison(&self.inner.state);
            let active = state
                .active
                .as_mut()
                .filter(|active| active.snapshot.run_id == run_id)?;

            if active.snapshot.status == TaskStatus::Running {
                active.snapshot.status = TaskStatus::Cancelling;
                active.snapshot.sequence = active.snapshot.sequence.saturating_add(1);
            }

            let cancellation = active.cancellation.clone();
            let snapshot = active.snapshot.clone();
            // Emit while still holding the state lock so this Cancelling event is ordered
            // before the terminal done event finish() emits once it takes `active`; a late
            // Cancelling event could otherwise arrive after the run has completed.
            self.inner.events.progress(snapshot.clone());
            (cancellation, snapshot)
        };

        cancellation.cancel();
        Some(snapshot)
    }

    pub fn history(&self, limit: Option<usize>) -> Vec<TaskHistoryEntry> {
        let limit = limit.unwrap_or(MAX_HISTORY).min(MAX_HISTORY);
        lock_unpoison(&self.inner.state)
            .history
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn clear_history(&self) -> Result<(), String> {
        let _io_guard = lock_unpoison(&self.inner.history_io);
        persist_history_file(&self.inner.history_path, &VecDeque::new())?;
        lock_unpoison(&self.inner.state).history.clear();
        Ok(())
    }
}

impl EngineInner {
    fn record(&self, run_id: &str, message: WorkerMessage) {
        let progress = {
            let mut state = lock_unpoison(&self.state);
            let Some(active) = state
                .active
                .as_mut()
                .filter(|active| active.snapshot.run_id == run_id)
            else {
                return;
            };

            let force_progress = match message {
                WorkerMessage::CandidateReady => {
                    active.snapshot.discovered = active.snapshot.discovered.saturating_add(1);
                    active.snapshot.total = active.snapshot.total.saturating_add(1);
                    active.snapshot.queued = active.snapshot.queued.saturating_add(1);
                    false
                }
                WorkerMessage::CandidateFiltered => {
                    active.snapshot.discovered = active.snapshot.discovered.saturating_add(1);
                    active.snapshot.locally_filtered =
                        active.snapshot.locally_filtered.saturating_add(1);
                    false
                }
                WorkerMessage::DiscoveryComplete => {
                    active.snapshot.discovery_complete = true;
                    true
                }
                WorkerMessage::DiscoveryFailed(error) => {
                    active.snapshot.discovery_complete = true;
                    active.snapshot.discovery_error = Some(error);
                    true
                }
                WorkerMessage::Started => {
                    active.snapshot.queued = active.snapshot.queued.saturating_sub(1);
                    active.snapshot.running = active.snapshot.running.saturating_add(1);
                    false
                }
                WorkerMessage::Finished(outcome, export_record) => {
                    active.snapshot.running = active.snapshot.running.saturating_sub(1);
                    match outcome {
                        HandlerOutcome::Succeeded => {
                            active.snapshot.succeeded = active.snapshot.succeeded.saturating_add(1);
                            active.snapshot.processed = active.snapshot.processed.saturating_add(1);
                        }
                        HandlerOutcome::Failed => {
                            active.snapshot.failed = active.snapshot.failed.saturating_add(1);
                            active.snapshot.processed = active.snapshot.processed.saturating_add(1);
                            if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                summary.invalid = summary.invalid.saturating_add(1);
                            }
                            if let Some(summary) = active.snapshot.chatgpt.as_mut() {
                                summary.invalid = summary.invalid.saturating_add(1);
                            }
                        }
                        HandlerOutcome::Skipped => {
                            active.snapshot.skipped = active.snapshot.skipped.saturating_add(1);
                        }
                        HandlerOutcome::ChatGpt(result) => {
                            active.snapshot.processed = active.snapshot.processed.saturating_add(1);
                            active.snapshot.retried =
                                active.snapshot.retried.saturating_add(result.retries);
                            match result.status {
                                ChatGptProbeStatus::Active(plan) => {
                                    active.snapshot.succeeded =
                                        active.snapshot.succeeded.saturating_add(1);
                                    if let Some(summary) = active.snapshot.chatgpt.as_mut() {
                                        summary.record_active(plan);
                                    }
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.record_active(ModulePlan::ChatGpt(plan));
                                    }
                                }
                                ChatGptProbeStatus::Authenticated(lookup) => {
                                    active.snapshot.succeeded =
                                        active.snapshot.succeeded.saturating_add(1);
                                    match lookup {
                                        ChatGptPlanLookup::Known(plan) => {
                                            if let Some(summary) = active.snapshot.chatgpt.as_mut()
                                            {
                                                summary.record_active(plan);
                                            }
                                            if let Some(summary) =
                                                active.snapshot.module_summary.as_mut()
                                            {
                                                summary.record_active(ModulePlan::ChatGpt(plan));
                                            }
                                        }
                                        ChatGptPlanLookup::Unknown => {
                                            if let Some(summary) = active.snapshot.chatgpt.as_mut()
                                            {
                                                summary.active = summary.active.saturating_add(1);
                                                summary.authenticated_unknown =
                                                    summary.authenticated_unknown.saturating_add(1);
                                            }
                                            if let Some(summary) =
                                                active.snapshot.module_summary.as_mut()
                                            {
                                                summary.authenticated_unknown =
                                                    summary.authenticated_unknown.saturating_add(1);
                                            }
                                        }
                                        ChatGptPlanLookup::Unavailable => {
                                            if let Some(summary) = active.snapshot.chatgpt.as_mut()
                                            {
                                                summary.active = summary.active.saturating_add(1);
                                                summary.plan_unavailable =
                                                    summary.plan_unavailable.saturating_add(1);
                                            }
                                            if let Some(summary) =
                                                active.snapshot.module_summary.as_mut()
                                            {
                                                summary.authenticated_unknown =
                                                    summary.authenticated_unknown.saturating_add(1);
                                            }
                                        }
                                    }
                                }
                                ChatGptProbeStatus::Dead => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.chatgpt.as_mut() {
                                        summary.dead = summary.dead.saturating_add(1);
                                    }
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.dead = summary.dead.saturating_add(1);
                                    }
                                }
                                ChatGptProbeStatus::RateLimited => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.chatgpt.as_mut() {
                                        summary.rate_limited =
                                            summary.rate_limited.saturating_add(1);
                                    }
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.rate_limited =
                                            summary.rate_limited.saturating_add(1);
                                    }
                                }
                                ChatGptProbeStatus::Error => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.chatgpt.as_mut() {
                                        summary.errors = summary.errors.saturating_add(1);
                                    }
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.errors = summary.errors.saturating_add(1);
                                    }
                                }
                            }
                        }
                        HandlerOutcome::Module(result) => {
                            active.snapshot.processed = active.snapshot.processed.saturating_add(1);
                            active.snapshot.retried =
                                active.snapshot.retried.saturating_add(result.retries);
                            match result.status {
                                ModuleProbeStatus::Active(plan) => {
                                    active.snapshot.succeeded =
                                        active.snapshot.succeeded.saturating_add(1);
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.record_active(plan);
                                    }
                                }
                                ModuleProbeStatus::Authenticated(plan) => {
                                    active.snapshot.succeeded =
                                        active.snapshot.succeeded.saturating_add(1);
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.record_authenticated(plan);
                                    }
                                }
                                ModuleProbeStatus::NoEntitlement(plan) => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.record_no_entitlement(plan);
                                    }
                                }
                                ModuleProbeStatus::Dead => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.dead = summary.dead.saturating_add(1);
                                    }
                                }
                                ModuleProbeStatus::RateLimited => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.rate_limited =
                                            summary.rate_limited.saturating_add(1);
                                    }
                                }
                                ModuleProbeStatus::Error => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.module_summary.as_mut() {
                                        summary.errors = summary.errors.saturating_add(1);
                                    }
                                }
                            }
                        }
                    }

                    match export_record {
                        ExportRecord::None => {}
                        ExportRecord::Active => {
                            active.snapshot.exported_active =
                                active.snapshot.exported_active.saturating_add(1);
                        }
                        ExportRecord::Failed => {
                            active.snapshot.exported_failed =
                                active.snapshot.exported_failed.saturating_add(1);
                        }
                        ExportRecord::Error => {
                            active.snapshot.export_errors =
                                active.snapshot.export_errors.saturating_add(1);
                        }
                    }
                    false
                }
            };

            active.snapshot.sequence = active.snapshot.sequence.saturating_add(1);
            active.snapshot.percent = if active.snapshot.discovery_complete {
                percentage(
                    active
                        .snapshot
                        .processed
                        .saturating_add(active.snapshot.skipped),
                    active.snapshot.total,
                )
            } else {
                0.0
            };

            let should_emit = force_progress
                || active.last_progress_event.elapsed() >= PROGRESS_INTERVAL
                || (active.snapshot.discovery_complete
                    && active
                        .snapshot
                        .processed
                        .saturating_add(active.snapshot.skipped)
                        == active.snapshot.total);
            if should_emit {
                active.last_progress_event = Instant::now();
                Some(active.snapshot.clone())
            } else {
                None
            }
        };

        if let Some(progress) = progress {
            self.events.progress(progress);
        }
    }

    fn finish(&self, run_id: &str, fatal_worker_error: bool) {
        let _io_guard = lock_unpoison(&self.history_io);
        let Some((mut snapshot, _summary, history)) =
            self.finish_locked(run_id, fatal_worker_error)
        else {
            return;
        };

        let persisted = persist_history_file(&self.history_path, &history).is_ok();
        snapshot.history_persisted = Some(persisted);
        {
            let mut state = lock_unpoison(&self.state);
            state.history = history;
        }
        self.events.progress(snapshot.clone());
        self.events.done(snapshot);
    }

    fn finish_locked(
        &self,
        run_id: &str,
        fatal_worker_error: bool,
    ) -> Option<(TaskSnapshot, TaskHistoryEntry, VecDeque<TaskHistoryEntry>)> {
        let mut state = lock_unpoison(&self.state);
        let mut active = state.active.take()?;
        if active.snapshot.run_id != run_id {
            state.active = Some(active);
            return None;
        }

        let accounted = active
            .snapshot
            .processed
            .saturating_add(active.snapshot.skipped);
        active.snapshot.skipped = active
            .snapshot
            .skipped
            .saturating_add(active.snapshot.total.saturating_sub(accounted));

        active.snapshot.status = if active.snapshot.discovery_error.is_some() {
            TaskStatus::Failed
        } else if active.snapshot.discovery_complete
            && active.snapshot.processed == active.snapshot.total
        {
            // Every input was processed, so the run succeeded even if a later worker thread
            // failed to spawn: the surviving workers drained the shared queue to completion.
            TaskStatus::Completed
        } else if fatal_worker_error {
            TaskStatus::Failed
        } else if active.cancellation.is_cancelled() {
            TaskStatus::Cancelled
        } else if active.snapshot.skipped > 0 {
            TaskStatus::Failed
        } else {
            TaskStatus::Completed
        };
        active.snapshot.queued = 0;
        active.snapshot.running = 0;
        active.snapshot.percent = 100.0;
        active.snapshot.sequence = active.snapshot.sequence.saturating_add(1);

        let finished_at = now_millis();
        active.snapshot.finished_at = Some(finished_at);
        let summary = TaskHistoryEntry {
            run_id: active.snapshot.run_id.clone(),
            module_id: active.snapshot.module_id.clone(),
            status: active.snapshot.status,
            total: active.snapshot.total,
            discovered: active.snapshot.discovered,
            locally_filtered: active.snapshot.locally_filtered,
            discovery_error: active.snapshot.discovery_error.clone(),
            succeeded: active.snapshot.succeeded,
            failed: active.snapshot.failed,
            skipped: active.snapshot.skipped,
            concurrency: active.snapshot.concurrency,
            requested_concurrency: active.snapshot.requested_concurrency,
            delay_ms: active.snapshot.delay_ms,
            use_proxy: active.snapshot.use_proxy,
            proxy_count: active.snapshot.proxy_count,
            started_at: active.snapshot.started_at,
            finished_at,
            duration_ms: finished_at.saturating_sub(active.snapshot.started_at),
            results_export_enabled: active.snapshot.results_export_enabled,
            exported_active: active.snapshot.exported_active,
            exported_failed: active.snapshot.exported_failed,
            export_errors: active.snapshot.export_errors,
        };

        let mut history = state.history.clone();
        history.push_back(summary.clone());
        while history.len() > MAX_HISTORY {
            history.pop_front();
        }

        Some((active.snapshot, summary, history))
    }
}

struct PreparedTask {
    module_id: String,
    roots: Vec<SourceRoot>,
    requested_concurrency: usize,
    delay_ms: u64,
    use_proxy: bool,
    output_directory: Option<PathBuf>,
    discovery_limits: DiscoveryLimits,
}

struct SourceCandidate {
    value: Arc<str>,
    from_directory: bool,
}

struct SourceRoot {
    value: Arc<str>,
    path: PathBuf,
    is_directory: bool,
    key: String,
}

#[cfg(test)]
fn prepare(request: StartTaskRequest) -> Result<PreparedTask, String> {
    prepare_with_limits(request, DiscoveryLimits::default())
}

fn prepare_with_limits(
    request: StartTaskRequest,
    discovery_limits: DiscoveryLimits,
) -> Result<PreparedTask, String> {
    let StartTaskRequest {
        module_id,
        entries,
        concurrency,
        delay_ms,
        use_proxy,
        output_directory,
    } = request;
    let module_id = module_id.trim().to_ascii_lowercase();
    if !catalog::is_known_module(&module_id) {
        return Err("unknown module".to_string());
    }
    if !catalog::is_enabled_module(&module_id) {
        return Err("module has not been migrated yet".to_string());
    }
    if concurrency == 0 {
        return Err("concurrency must be greater than zero".to_string());
    }
    if delay_ms > MAX_DELAY_MS {
        return Err(format!("the maximum delay is {MAX_DELAY_MS} ms"));
    }
    if entries.len() > MAX_RAW_ENTRIES {
        return Err(format!("the limit is {MAX_RAW_ENTRIES} lines"));
    }
    let output_directory = prepare_output_directory(output_directory)?;
    let excluded_result_directory = output_directory.clone();

    let mut root_keys = HashSet::<String>::with_capacity(entries.len().min(MAX_RAW_ENTRIES));
    let mut roots = Vec::<SourceRoot>::with_capacity(entries.len().min(MAX_RAW_ENTRIES));
    let mut raw_input_bytes = 0usize;
    let mut explicit_files = 0usize;
    for entry in entries {
        let entry_bytes = entry.len();
        if entry_bytes > MAX_ENTRY_BYTES {
            return Err(format!(
                "each line can contain at most {MAX_ENTRY_BYTES} bytes"
            ));
        }
        raw_input_bytes = checked_total_input_bytes(raw_input_bytes, entry_bytes)?;

        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = PathBuf::from(trimmed);
        let is_local = auth_artifact::artifact_path_is_local(&path);
        if path.is_absolute() && !is_local {
            return Err(
                "task sources must be absolute local paths without links or reparse points"
                    .to_string(),
            );
        }
        if is_local {
            if is_inside_results_directory(&path) {
                return Err("a results directory cannot also be used as a task source".to_string());
            }
            if excluded_result_directory
                .as_deref()
                .is_some_and(|excluded| path_is_within(&path, excluded))
            {
                return Err("a results directory cannot also be used as a task source".to_string());
            }
        }
        let is_directory = is_local
            && fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.is_dir() && !is_link_or_reparse(&metadata));
        let key = stable_path_key(&path);
        if !root_keys.insert(key.clone()) {
            continue;
        }
        if !is_directory {
            explicit_files = explicit_files.saturating_add(1);
            if discovery_limits
                .max_files
                .is_some_and(|limit| explicit_files > limit)
            {
                return Err(format!(
                    "the limit is {} file paths",
                    discovery_limits.max_files.expect("checked finite limit")
                ));
            }
        }
        let value: Arc<str> = Arc::from(trimmed);
        roots.push(SourceRoot {
            value,
            path,
            is_directory,
            key,
        });
    }

    if roots.is_empty() {
        return Err("provide at least one valid entry".to_string());
    }

    // Only explicit roots are retained. Nested directory roots are redundant and would
    // otherwise discover the same subtree twice; pruning them is O(number of root path
    // components) and does not require a set of every file found later.
    let directory_keys: HashSet<_> = roots
        .iter()
        .filter(|root| root.is_directory)
        .map(|root| root.key.clone())
        .collect();
    roots.retain(|root| {
        !root.is_directory
            || !root
                .path
                .ancestors()
                .skip(1)
                .filter(|ancestor| !ancestor.as_os_str().is_empty())
                .any(|ancestor| directory_keys.contains(&stable_path_key(ancestor)))
    });

    Ok(PreparedTask {
        module_id,
        roots,
        requested_concurrency: concurrency.min(MAX_WORKERS),
        delay_ms,
        use_proxy,
        output_directory,
        discovery_limits,
    })
}

fn prepare_output_directory(value: Option<String>) -> Result<Option<PathBuf>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "the results path can contain at most {MAX_ENTRY_BYTES} bytes"
        ));
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let path = Path::new(trimmed);
    if !auth_artifact::output_directory_is_local(path) {
        return Err("the results directory must be an absolute local path".to_string());
    }
    let metadata = fs::metadata(path)
        .map_err(|_| "the selected results directory is not available".to_string())?;
    if !metadata.is_dir() {
        return Err("the selected results path must be a directory".to_string());
    }
    let resolved = fs::canonicalize(path)
        .map_err(|_| "unable to resolve the selected results directory".to_string())?;
    Ok(Some(resolved))
}

fn ensure_results_marker(directory: &Path) -> Result<(), String> {
    let trusted_directory = directory.to_path_buf();
    let marker = trusted_directory.join(RESULTS_MARKER_FILE);
    match fs::symlink_metadata(&marker) {
        Ok(_) if is_valid_results_marker(&marker) => return Ok(()),
        Ok(_) => {
            return Err("the selected results directory contains an invalid marker".to_string());
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err("unable to inspect the selected results directory".to_string());
        }
        Err(_) => {}
    }

    let marker_attempt = new_task_id();
    let partial_marker = trusted_directory.join(format!(
        "{RESULTS_MARKER_FILE}.p{:x}.{marker_attempt}.tmp",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&partial_marker)
        .map_err(|_| "unable to initialize the selected results directory".to_string())?;
    if !auth_artifact::opened_file_is_within_local_directory(&file, &trusted_directory)
        || file
            .write_all(RESULTS_MARKER_CONTENT)
            .and_then(|_| file.sync_all())
            .is_err()
    {
        drop(file);
        let _ = fs::remove_file(&partial_marker);
        return Err("unable to initialize the selected results directory".to_string());
    }
    drop(file);

    let published = publish_without_overwrite(&partial_marker, &marker);

    if published.is_err() {
        let _ = fs::remove_file(&partial_marker);
        if is_valid_results_marker(&marker) {
            return Ok(());
        }
        return Err("unable to initialize the selected results directory".to_string());
    }

    if is_valid_results_marker(&marker) {
        Ok(())
    } else {
        Err("unable to initialize the selected results directory".to_string())
    }
}

fn is_valid_results_marker(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_file()
        || is_link_or_reparse(&metadata)
        || metadata.len() != RESULTS_MARKER_CONTENT.len() as u64
    {
        return false;
    }
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let Ok(opened_metadata) = file.metadata() else {
        return false;
    };
    if !opened_metadata.is_file() || opened_metadata.len() != RESULTS_MARKER_CONTENT.len() as u64 {
        return false;
    }
    let mut contents = Vec::with_capacity(RESULTS_MARKER_CONTENT.len());
    file.take((RESULTS_MARKER_CONTENT.len() + 1) as u64)
        .read_to_end(&mut contents)
        .is_ok()
        && contents == RESULTS_MARKER_CONTENT
}

fn is_results_directory(path: &Path) -> bool {
    is_valid_results_marker(&path.join(RESULTS_MARKER_FILE))
}

fn is_inside_results_directory(path: &Path) -> bool {
    path.ancestors().any(is_results_directory)
}

fn effective_concurrency(requested: usize, use_proxy: bool, proxy_count: usize) -> usize {
    if use_proxy {
        requested.min(proxy_count)
    } else {
        requested
    }
}

fn stable_path_key(path: &Path) -> String {
    let normalized = if auth_artifact::artifact_path_is_local(path) {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    #[cfg(windows)]
    {
        normalized
            .to_string_lossy()
            .replace('/', "\\")
            .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        normalized.to_string_lossy().into_owned()
    }
}

enum DiscoveryAbort {
    Cancelled,
    Failed(String),
}

struct DiscoveryProducer<'a> {
    module_id: &'a str,
    limits: DiscoveryLimits,
    remaining_bytes: Option<AtomicU64>,
    input_sender: &'a mpsc::SyncSender<TaskInput>,
    status_sender: &'a mpsc::SyncSender<WorkerMessage>,
    cancellation: &'a CancellationToken,
    prepare_artifacts: bool,
    explicit_keys: HashSet<String>,
    excluded_directory: Option<&'a Path>,
    visited_directories: usize,
    discovered_files: usize,
    next_ordinal: usize,
    ready: usize,
}

impl DiscoveryProducer<'_> {
    fn check_cancelled(&self) -> Result<(), DiscoveryAbort> {
        if self.cancellation.is_cancelled() {
            Err(DiscoveryAbort::Cancelled)
        } else {
            Ok(())
        }
    }

    fn enter_directory(
        &mut self,
        path: &Path,
        root: bool,
    ) -> Result<Option<fs::ReadDir>, DiscoveryAbort> {
        self.check_cancelled()?;
        if !auth_artifact::artifact_path_is_local(path) {
            return if root {
                Err(DiscoveryAbort::Failed(
                    "a discovery root must be an absolute local directory without links or reparse points"
                        .to_string(),
                ))
            } else {
                Ok(None)
            };
        }
        self.visited_directories = self.visited_directories.checked_add(1).ok_or_else(|| {
            DiscoveryAbort::Failed("too many directories were visited".to_string())
        })?;
        if self
            .limits
            .max_directories
            .is_some_and(|limit| self.visited_directories > limit)
        {
            return Err(DiscoveryAbort::Failed(format!(
                "the limit is {} directories per validation",
                self.limits.max_directories.expect("checked finite limit")
            )));
        }
        match fs::read_dir(path) {
            Ok(reader) => Ok(Some(reader)),
            Err(error) if !root && transient_discovery_error(&error) => Ok(None),
            Err(_) => Err(DiscoveryAbort::Failed(
                "unable to read one of the provided directories".to_string(),
            )),
        }
    }

    fn walk_directory(&mut self, root: &Path) -> Result<(), DiscoveryAbort> {
        let mut readers = vec![
            self.enter_directory(root, true)?
                .expect("a readable root returns a directory iterator"),
        ];
        while !readers.is_empty() {
            self.check_cancelled()?;
            let entry = {
                let reader = readers.last_mut().expect("non-empty reader stack");
                reader.next()
            };
            let Some(entry) = entry else {
                readers.pop();
                continue;
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if transient_discovery_error(&error) => continue,
                Err(_) => {
                    return Err(DiscoveryAbort::Failed(
                        "unable to list one of the provided directories".to_string(),
                    ));
                }
            };
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if transient_discovery_error(&error) => continue,
                Err(_) => {
                    return Err(DiscoveryAbort::Failed(
                        "unable to read an item in the directory".to_string(),
                    ));
                }
            };
            if is_link_or_reparse(&metadata) || is_platform_metadata(&path, metadata.is_dir()) {
                continue;
            }
            if metadata.is_dir() {
                if !auth_artifact::artifact_path_is_local(&path) {
                    continue;
                }
                if !is_results_directory(&path)
                    && !self
                        .excluded_directory
                        .is_some_and(|excluded| path_is_within(&path, excluded))
                {
                    if let Some(reader) = self.enter_directory(&path, false)? {
                        readers.push(reader);
                    }
                }
            } else if metadata.is_file()
                && (self.explicit_keys.is_empty()
                    || !self.explicit_keys.contains(&stable_path_key(&path)))
            {
                let Some(value) = path.to_str() else {
                    continue;
                };
                self.submit_candidate(SourceCandidate {
                    value: Arc::from(value),
                    from_directory: true,
                })?;
            }
        }
        Ok(())
    }

    fn submit_candidate(&mut self, candidate: SourceCandidate) -> Result<(), DiscoveryAbort> {
        self.check_cancelled()?;
        if candidate.value.len() > MAX_ENTRY_BYTES {
            return Err(DiscoveryAbort::Failed(format!(
                "each path can contain at most {MAX_ENTRY_BYTES} bytes"
            )));
        }
        if let Some(limit) = self.limits.max_files {
            if self.discovered_files >= limit {
                return Err(DiscoveryAbort::Failed(format!(
                    "the limit is {limit} file paths"
                )));
            }
        }
        self.discovered_files = self.discovered_files.saturating_add(1);
        let artifact = if self.prepare_artifacts {
            match self.prepare_candidate(&candidate)? {
                Some(artifact) => Some(artifact),
                None => {
                    self.check_cancelled()?;
                    self.status_sender
                        .send(WorkerMessage::CandidateFiltered)
                        .map_err(|_| DiscoveryAbort::Cancelled)?;
                    return Ok(());
                }
            }
        } else {
            None
        };
        self.check_cancelled()?;

        self.next_ordinal = self.next_ordinal.saturating_add(1);
        let input = TaskInput {
            value: candidate.value,
            ordinal: self.next_ordinal,
            artifact,
        };
        // Account the item before making it visible to a worker. If cancellation wins after
        // this point, finish() accounts the accepted-but-not-processed item as skipped.
        self.status_sender
            .send(WorkerMessage::CandidateReady)
            .map_err(|_| DiscoveryAbort::Cancelled)?;
        if !send_task_input(self.input_sender, input, self.cancellation) {
            return Err(DiscoveryAbort::Cancelled);
        }
        self.ready = self.ready.saturating_add(1);
        Ok(())
    }

    fn prepare_candidate(
        &self,
        candidate: &SourceCandidate,
    ) -> Result<Option<PreparedArtifact>, DiscoveryAbort> {
        let aggregate_budget = candidate
            .from_directory
            .then_some(self.remaining_bytes.as_ref())
            .flatten();
        let bytes = match auth_artifact::read_artifact_path(
            Path::new(candidate.value.as_ref()),
            aggregate_budget,
        ) {
            Ok(bytes) => bytes,
            Err(auth_artifact::ArtifactReadError::BudgetExceeded) => {
                let limit = self
                    .limits
                    .scan_budget_bytes
                    .expect("a budget error requires a finite limit");
                return Err(DiscoveryAbort::Failed(format!(
                    "the local discovery scan can inspect at most {} MiB",
                    limit / (1024 * 1024)
                )));
            }
            Err(auth_artifact::ArtifactReadError::Inspection(reason)) => {
                if candidate.from_directory {
                    return Ok(None);
                }
                return Ok(Some(if self.module_id == "chatgpt" {
                    PreparedArtifact::ChatGpt(auth_artifact::ChatGptArtifactPreparation::Rejected {
                        reason,
                        artifact_bytes: None,
                    })
                } else {
                    PreparedArtifact::Cookie(Err(None))
                }));
            }
        };

        if self.module_id == "chatgpt" {
            let preparation = auth_artifact::load_chatgpt_bytes(bytes);
            if candidate.from_directory
                && !matches!(
                    &preparation,
                    auth_artifact::ChatGptArtifactPreparation::Ready(_)
                )
            {
                Ok(None)
            } else {
                Ok(Some(PreparedArtifact::ChatGpt(preparation)))
            }
        } else if let Some(policy) = cookie_policy_for_module(self.module_id) {
            let rejected_bytes = bytes.clone();
            match cookie_artifact::prepare_cookie_artifact(bytes, policy) {
                Ok(artifact) => Ok(Some(PreparedArtifact::Cookie(Ok(artifact)))),
                Err(_) if candidate.from_directory => Ok(None),
                Err(_) => Ok(Some(PreparedArtifact::Cookie(Err(Some(rejected_bytes))))),
            }
        } else {
            Ok(None)
        }
    }
}

fn transient_discovery_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

fn produce_task_inputs(
    module_id: &str,
    roots: &[SourceRoot],
    limits: DiscoveryLimits,
    excluded_directory: Option<&Path>,
    prepare_artifacts: bool,
    input_sender: &mpsc::SyncSender<TaskInput>,
    status_sender: &mpsc::SyncSender<WorkerMessage>,
    cancellation: &CancellationToken,
) {
    let explicit_keys = roots
        .iter()
        .filter(|root| !root.is_directory)
        .map(|root| root.key.clone())
        .collect();
    let mut producer = DiscoveryProducer {
        module_id,
        limits,
        remaining_bytes: limits.scan_budget_bytes.map(AtomicU64::new),
        input_sender,
        status_sender,
        cancellation,
        prepare_artifacts,
        explicit_keys,
        excluded_directory,
        visited_directories: 0,
        discovered_files: 0,
        next_ordinal: 0,
        ready: 0,
    };

    let outcome = roots.iter().try_for_each(|root| {
        producer.check_cancelled()?;
        if root.is_directory {
            producer.walk_directory(&root.path)
        } else {
            producer.submit_candidate(SourceCandidate {
                value: Arc::clone(&root.value),
                from_directory: false,
            })
        }
    });

    match outcome {
        Ok(()) if producer.ready == 0 => {
            let error = format!(
                "no structurally usable authentication files were found; {} unrelated or unusable files were ignored",
                producer.discovered_files
            );
            cancellation.cancel();
            let _ = status_sender.send(WorkerMessage::DiscoveryFailed(error));
        }
        Ok(()) => {
            let _ = status_sender.send(WorkerMessage::DiscoveryComplete);
        }
        Err(DiscoveryAbort::Cancelled) => {}
        Err(DiscoveryAbort::Failed(error)) => {
            cancellation.cancel();
            let _ = status_sender.send(WorkerMessage::DiscoveryFailed(error));
        }
    }
}

fn path_is_within(path: &Path, directory: &Path) -> bool {
    let resolved_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let resolved_directory =
        fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    resolved_path_is_within(&resolved_path, &resolved_directory)
}

fn resolved_path_is_within(resolved_path: &Path, resolved_directory: &Path) -> bool {
    #[cfg(windows)]
    {
        let path_components: Vec<_> = resolved_path
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        let directory_components: Vec<_> = resolved_directory
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        path_components.starts_with(&directory_components)
    }
    #[cfg(not(windows))]
    {
        resolved_path.starts_with(resolved_directory)
    }
}

fn is_platform_metadata(path: &Path, is_directory: bool) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if is_directory {
        name.eq_ignore_ascii_case("__MACOSX")
    } else {
        name.starts_with("._")
            || name.eq_ignore_ascii_case(".DS_Store")
            || name.eq_ignore_ascii_case(RESULTS_MARKER_FILE)
    }
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn checked_total_input_bytes(current: usize, additional: usize) -> Result<usize, String> {
    current
        .checked_add(additional)
        .filter(|total| *total <= MAX_TOTAL_INPUT_BYTES)
        .ok_or_else(|| format!("the total input can contain at most {MAX_TOTAL_INPUT_BYTES} bytes"))
}

#[allow(clippy::too_many_arguments)]
fn run_task(
    inner: Arc<EngineInner>,
    run_id: String,
    module_id: String,
    roots: Vec<SourceRoot>,
    concurrency: usize,
    delay_ms: u64,
    probe: Option<TaskProbe>,
    cancellation: CancellationToken,
    result_exporter: Option<Arc<ResultExporter>>,
    discovery_limits: DiscoveryLimits,
    excluded_directory: Option<PathBuf>,
) {
    let module_id: Arc<str> = Arc::from(module_id);
    let worker_count = concurrency.max(1);
    let delay = Duration::from_millis(delay_ms);
    let (input_sender, input_receiver) = mpsc::sync_channel((worker_count * 2).max(1));
    let input_receiver = Arc::new(Mutex::new(input_receiver));
    let (sender, receiver) = mpsc::sync_channel((worker_count * 2).max(1));
    let mut workers = Vec::with_capacity(worker_count);
    let mut fatal_worker_error = false;

    for worker_index in 0..worker_count {
        let input_receiver = Arc::clone(&input_receiver);
        let module_id = Arc::clone(&module_id);
        let sender = sender.clone();
        let handler = Arc::clone(&inner.handler);
        let context = TaskContext {
            cancellation: cancellation.clone(),
            probe: probe.clone(),
        };
        let result_exporter = result_exporter.clone();
        let result = thread::Builder::new()
            .name(format!("ayla-task-worker-{worker_index}"))
            .spawn(move || {
                worker_loop(
                    input_receiver,
                    module_id,
                    sender,
                    handler,
                    context,
                    delay,
                    result_exporter,
                )
            });

        match result {
            Ok(worker) => workers.push(worker),
            Err(_) => {
                fatal_worker_error = true;
                break;
            }
        }
    }
    drop(input_receiver);

    if workers.is_empty() {
        drop(input_sender);
        cancellation.cancel();
        inner.record(
            &run_id,
            WorkerMessage::DiscoveryFailed("unable to start task workers".to_string()),
        );
        inner.finish(&run_id, true);
        return;
    }

    let producer_module_id = Arc::clone(&module_id);
    let producer_sender = sender.clone();
    let producer_cancellation = cancellation.clone();
    let prepare_artifacts = inner.handler.prepares_artifacts();
    let producer = thread::Builder::new()
        .name(format!("ayla-task-discovery-{run_id}"))
        .spawn(move || {
            produce_task_inputs(
                producer_module_id.as_ref(),
                &roots,
                discovery_limits,
                excluded_directory.as_deref(),
                prepare_artifacts,
                &input_sender,
                &producer_sender,
                &producer_cancellation,
            );
        });
    if producer.is_err() {
        fatal_worker_error = true;
        cancellation.cancel();
        let _ = sender.send(WorkerMessage::DiscoveryFailed(
            "unable to start task discovery".to_string(),
        ));
    }
    drop(sender);

    for message in receiver {
        match message {
            WorkerMessage::DiscoveryFailed(error) => {
                fatal_worker_error = true;
                inner.record(&run_id, WorkerMessage::DiscoveryFailed(error));
            }
            message => inner.record(&run_id, message),
        }
    }

    if let Ok(producer) = producer {
        if producer.join().is_err() {
            fatal_worker_error = true;
            cancellation.cancel();
            inner.record(
                &run_id,
                WorkerMessage::DiscoveryFailed("task discovery stopped unexpectedly".to_string()),
            );
        }
    }

    for worker in workers {
        if worker.join().is_err() {
            fatal_worker_error = true;
        }
    }

    inner.finish(&run_id, fatal_worker_error);
}

fn send_task_input(
    sender: &mpsc::SyncSender<TaskInput>,
    mut input: TaskInput,
    cancellation: &CancellationToken,
) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        match sender.try_send(input) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(returned)) => {
                input = returned;
                if cancellation.wait_cancelled(Duration::from_millis(10)) {
                    return false;
                }
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn worker_loop(
    input_receiver: Arc<Mutex<mpsc::Receiver<TaskInput>>>,
    module_id: Arc<str>,
    sender: mpsc::SyncSender<WorkerMessage>,
    handler: Arc<dyn TaskHandler>,
    context: TaskContext,
    delay: Duration,
    result_exporter: Option<Arc<ResultExporter>>,
) {
    loop {
        if context.is_cancelled() {
            break;
        }

        let input = lock_unpoison(&input_receiver).recv();
        let Ok(input) = input else {
            break;
        };

        if sender.send(WorkerMessage::Started).is_err() {
            break;
        }

        if context.cancellation.wait_cancelled(delay) {
            let _ = sender.send(WorkerMessage::Finished(
                HandlerOutcome::Skipped,
                ExportRecord::None,
            ));
            break;
        }

        let ordinal = input.ordinal();
        let result = handler.process(module_id.as_ref(), input, &context);
        let export_record = result_exporter
            .as_deref()
            .map_or(ExportRecord::None, |exporter| {
                exporter.export(ordinal, result.outcome, result.artifact_bytes.as_deref())
            });
        if sender
            .send(WorkerMessage::Finished(result.outcome, export_record))
            .is_err()
        {
            break;
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TaskHistoryFile {
    version: u8,
    entries: Vec<TaskHistoryEntry>,
}

fn load_history(path: &Path) -> VecDeque<TaskHistoryEntry> {
    let candidates = [
        path.to_path_buf(),
        sidecar_path(path, "tmp"),
        sidecar_path(path, "bak"),
    ];
    let mut entries = candidates
        .iter()
        .find_map(|candidate| {
            let data = fs::read(candidate).ok()?;
            serde_json::from_slice::<TaskHistoryFile>(&data)
                .ok()
                .map(|file| file.entries)
        })
        .unwrap_or_default();

    entries.retain(|entry| entry.status.is_terminal());
    if entries.len() > MAX_HISTORY {
        entries.drain(..entries.len() - MAX_HISTORY);
    }
    entries.into()
}

fn persist_history_file(path: &Path, history: &VecDeque<TaskHistoryEntry>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("unable to create the task directory: {error}"))?;
    }

    let data = serde_json::to_vec_pretty(&TaskHistoryFile {
        version: HISTORY_VERSION,
        entries: history.iter().cloned().collect(),
    })
    .map_err(|error| format!("unable to serialize task history: {error}"))?;
    let temporary = sidecar_path(path, "tmp");
    let backup = sidecar_path(path, "bak");

    let mut file = fs::File::create(&temporary)
        .map_err(|error| format!("unable to create the temporary task history: {error}"))?;
    if let Err(error) = file.write_all(&data).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "unable to write the temporary task history: {error}"
        ));
    }
    drop(file);

    match fs::rename(&temporary, path) {
        Ok(()) => {
            let _ = fs::remove_file(&backup);
            Ok(())
        }
        Err(first_error) if path.exists() => {
            let _ = fs::remove_file(&backup);
            fs::rename(path, &backup).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("unable to prepare the task history replacement: {error}")
            })?;

            match fs::rename(&temporary, path) {
                Ok(()) => {
                    let _ = fs::remove_file(&backup);
                    Ok(())
                }
                Err(error) => {
                    let _ = fs::rename(&backup, path);
                    let _ = fs::remove_file(&temporary);
                    Err(format!(
                        "unable to replace the task history: {error}; initial attempt: {first_error}"
                    ))
                }
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(format!("unable to publish the task history: {error}"))
        }
    }
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task_history.json");
    path.with_file_name(format!("{file_name}.{suffix}"))
}

fn new_task_id() -> String {
    let sequence = TASK_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("task_{timestamp:x}_{sequence:x}")
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn percentage(done: usize, total: usize) -> f64 {
    if total == 0 {
        100.0
    } else {
        (done as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

fn lock_unpoison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_probe::{MaxPlan, MaxSubscriptionState, MaxTier, TwitchPlan, TwitchRole};
    use std::collections::{HashSet, VecDeque};

    #[derive(Default)]
    struct RecordingEvents {
        progress: Mutex<Vec<TaskSnapshot>>,
        done: Mutex<Vec<TaskSnapshot>>,
    }

    impl TaskEventSink for RecordingEvents {
        fn progress(&self, snapshot: TaskSnapshot) {
            lock_unpoison(&self.progress).push(snapshot);
        }

        fn done(&self, snapshot: TaskSnapshot) {
            lock_unpoison(&self.done).push(snapshot);
        }
    }

    struct TrackingHandler {
        seen: Mutex<Vec<String>>,
        in_flight: AtomicUsize,
        peak: AtomicUsize,
        pause: Duration,
    }

    impl TrackingHandler {
        fn new(pause: Duration) -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                pause,
            }
        }
    }

    impl TaskHandler for TrackingHandler {
        fn process(
            &self,
            _module_id: &str,
            input: TaskInput,
            context: &TaskContext,
        ) -> HandlerResult {
            if context.is_cancelled() {
                return HandlerResult::without_artifact(HandlerOutcome::Skipped);
            }

            let active = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            self.peak.fetch_max(active, Ordering::AcqRel);
            if !self.pause.is_zero() {
                thread::sleep(self.pause);
            }

            lock_unpoison(&self.seen).push(input.into_inner().to_string());
            self.in_flight.fetch_sub(1, Ordering::AcqRel);

            if context.is_cancelled() {
                HandlerResult::without_artifact(HandlerOutcome::Skipped)
            } else {
                HandlerResult::without_artifact(HandlerOutcome::Succeeded)
            }
        }
    }

    #[derive(Default)]
    struct CountingProber {
        calls: AtomicUsize,
    }

    impl ChatGptProber for CountingProber {
        fn check(
            &self,
            _auth: &auth_artifact::PreparedChatGptAuth,
            _control: &dyn ProbeControl,
        ) -> ChatGptProbeResult {
            self.calls.fetch_add(1, Ordering::AcqRel);
            ChatGptProbeResult {
                status: ChatGptProbeStatus::Active(ChatGptPlan::Free),
                retries: 0,
            }
        }
    }

    struct MutatingProber {
        source: PathBuf,
        replacement: Vec<u8>,
    }

    impl ChatGptProber for MutatingProber {
        fn check(
            &self,
            _auth: &auth_artifact::PreparedChatGptAuth,
            _control: &dyn ProbeControl,
        ) -> ChatGptProbeResult {
            fs::write(&self.source, &self.replacement).expect("replace source during probe");
            ChatGptProbeResult {
                status: ChatGptProbeStatus::Active(ChatGptPlan::Plus),
                retries: 0,
            }
        }
    }

    struct SequencedCookieProber {
        results: Mutex<VecDeque<ModuleProbeResult>>,
    }

    impl SequencedCookieProber {
        fn new(results: impl IntoIterator<Item = ModuleProbeResult>) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
            }
        }
    }

    impl CookieModuleProber for SequencedCookieProber {
        fn check(
            &self,
            _artifact: &cookie_artifact::PreparedCookieArtifact,
            _control: &dyn ProbeControl,
        ) -> ModuleProbeResult {
            lock_unpoison(&self.results)
                .pop_front()
                .expect("one synthetic result per artifact")
        }
    }

    fn write_ready_artifact(path: &Path, index: usize) {
        let token = format!("synthetic_scan_{index:04}_{}", "A".repeat(48));
        fs::write(
            path,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t4102444800\t__Secure-next-auth.session-token\t{token}\n"
            ),
        )
        .expect("write structurally ready fixture");
    }

    fn write_expired_artifact(path: &Path) {
        fs::write(
            path,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t1\t__Secure-next-auth.session-token\tsynthetic_expired_{}\n",
                "B".repeat(48)
            ),
        )
        .expect("write expired fixture");
    }

    fn write_twitch_artifact(path: &Path, index: usize) {
        let token = format!("synthetic_twitch_{index:04}_{}", "T".repeat(48));
        fs::write(
            path,
            format!(".twitch.tv\tTRUE\t/\tTRUE\t4102444800\tauth-token\t{token}\n"),
        )
        .expect("write structurally ready Twitch fixture");
    }

    fn write_max_artifact(path: &Path, index: usize) {
        let token = format!("synthetic_max_{index:04}_{}", "M".repeat(48));
        fs::write(
            path,
            format!(".api.hbomax.com\tTRUE\t/\tTRUE\t4102444800\tst\t{token}\n"),
        )
        .expect("write structurally ready Max fixture");
    }

    fn request(
        module_id: &str,
        entries: impl IntoIterator<Item = String>,
        concurrency: usize,
        delay_ms: u64,
    ) -> StartTaskRequest {
        StartTaskRequest {
            module_id: module_id.to_string(),
            entries: entries.into_iter().collect(),
            concurrency,
            delay_ms,
            use_proxy: false,
            output_directory: None,
        }
    }

    fn test_engine(
        path: PathBuf,
        handler: Arc<dyn TaskHandler>,
        events: Arc<dyn TaskEventSink>,
    ) -> TaskEngine {
        TaskEngine::with_components(path, handler, events)
    }

    fn run_local_inspection(request: StartTaskRequest, limits: DiscoveryLimits) -> TaskSnapshot {
        let state = tempfile::tempdir().expect("temporary task state");
        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            state.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            events.clone(),
        );
        let started = engine
            .start_with_probe(request, None, 0, limits)
            .expect("start local inspection");
        wait_for_done(&events, &started.run_id)
    }

    fn wait_for_history(engine: &TaskEngine, run_id: &str) -> TaskHistoryEntry {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if let Some(summary) = engine
                .history(None)
                .into_iter()
                .find(|summary| summary.run_id == run_id)
            {
                return summary;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("task did not finish in time: {run_id}");
    }

    fn wait_for_done(events: &RecordingEvents, run_id: &str) -> TaskSnapshot {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if let Some(snapshot) = lock_unpoison(&events.done)
                .iter()
                .find(|snapshot| snapshot.run_id == run_id)
                .cloned()
            {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("done event did not arrive: {run_id}");
    }

    #[test]
    fn request_deserialization_enforces_entry_limits_before_prepare() {
        let valid: StartTaskRequest = serde_json::from_value(serde_json::json!({
            "moduleId": "chatgpt",
            "entries": ["a", "b"],
            "concurrency": 2,
            "delayMs": 0
        }))
        .expect("deserialize bounded request");
        assert_eq!(valid.entries, vec!["a", "b"]);

        let too_long = "x".repeat(MAX_ENTRY_BYTES + 1);
        assert!(
            serde_json::from_value::<StartTaskRequest>(serde_json::json!({
                "moduleId": "chatgpt",
                "entries": [too_long],
                "concurrency": 1,
                "delayMs": 0
            }))
            .is_err()
        );

        let too_many = vec![String::new(); MAX_RAW_ENTRIES + 1];
        assert!(
            serde_json::from_value::<StartTaskRequest>(serde_json::json!({
                "moduleId": "chatgpt",
                "entries": too_many,
                "concurrency": 1,
                "delayMs": 0
            }))
            .is_err()
        );
    }

    #[test]
    fn preparation_deduplicates_trims_and_enforces_limits() {
        let prepared = prepare(request(
            " ChatGPT ",
            [" alpha ", "", "alpha", "beta", " beta "].map(String::from),
            999,
            0,
        ))
        .expect("prepare task");

        assert_eq!(prepared.module_id, "chatgpt");
        assert_eq!(prepared.roots.len(), 2);
        let order: Vec<_> = prepared
            .roots
            .iter()
            .map(|root| root.value.as_ref())
            .collect();
        assert_eq!(order, vec!["alpha", "beta"]);

        assert_eq!(prepared.requested_concurrency, MAX_WORKERS);
        assert!(!prepared.use_proxy);

        assert_eq!(effective_concurrency(12, false, 0), 12);
        assert_eq!(effective_concurrency(12, true, 4), 4);
        assert_eq!(effective_concurrency(3, true, 8), 3);

        assert!(prepare(request("unknown", ["a".to_string()], 1, 0)).is_err());
        assert!(prepare(request("reddit", ["a".to_string()], 1, 0)).is_err());
        assert!(prepare(request("chatgpt", ["a".to_string()], 0, 0)).is_err());

        let too_many = (0..=DEFAULT_MAX_FILES).map(|index| format!("entry-{index}"));
        assert!(prepare(request("chatgpt", too_many, 1, 0)).is_err());

        let too_long = "x".repeat(MAX_ENTRY_BYTES + 1);
        assert!(prepare(request("chatgpt", [too_long], 1, 0)).is_err());

        assert_eq!(
            checked_total_input_bytes(MAX_TOTAL_INPUT_BYTES - 1, 1),
            Ok(MAX_TOTAL_INPUT_BYTES)
        );
        assert!(
            checked_total_input_bytes(MAX_TOTAL_INPUT_BYTES - 1, 2).is_err(),
            "aggregate byte limit must fail closed"
        );

        let too_many_lines = (0..=MAX_RAW_ENTRIES).map(|_| String::new());
        assert!(prepare(request("chatgpt", too_many_lines, 1, 0)).is_err());
        assert!(prepare(request("chatgpt", ["a".to_string()], 1, MAX_DELAY_MS + 1)).is_err());
    }

    #[test]
    fn streaming_discovery_expands_directories_recursively() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("nested");
        fs::create_dir_all(&nested).expect("create nested directory");
        write_ready_artifact(&directory.path().join("one.txt"), 1);
        write_ready_artifact(&nested.join("two.json"), 2);

        let done = run_local_inspection(
            request("chatgpt", [directory.path().display().to_string()], 4, 0),
            DiscoveryLimits::default(),
        );

        assert!(done.discovery_complete);
        assert_eq!(done.discovered, 2);
        assert_eq!(done.total, 2);
        assert_eq!(done.locally_filtered, 0);
        assert_eq!(done.succeeded, 2);
    }

    #[test]
    fn configurable_discovery_limits_bound_files_directories_and_scan_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("nested");
        fs::create_dir_all(&nested).expect("create nested directory");
        write_ready_artifact(&directory.path().join("one.txt"), 1);
        write_ready_artifact(&directory.path().join("two.txt"), 2);
        write_ready_artifact(&nested.join("three.txt"), 3);

        let file_done = run_local_inspection(
            request("chatgpt", [directory.path().display().to_string()], 1, 0),
            DiscoveryLimits::new(Some(10), Some(2), Some(8)),
        );
        assert_eq!(file_done.status, TaskStatus::Failed);
        assert!(
            file_done
                .discovery_error
                .as_deref()
                .is_some_and(|error| error.contains("2"))
        );

        let directory_done = run_local_inspection(
            request("chatgpt", [directory.path().display().to_string()], 1, 0),
            DiscoveryLimits::new(Some(1), Some(10), Some(8)),
        );
        assert_eq!(directory_done.status, TaskStatus::Failed);
        assert!(
            directory_done
                .discovery_error
                .as_deref()
                .is_some_and(|error| error.contains("1 directories"))
        );

        let budget_directory = tempfile::tempdir().expect("temporary budget directory");
        fs::write(
            budget_directory.path().join("large-a.txt"),
            vec![b'a'; 600 * 1024],
        )
        .expect("write first large file");
        fs::write(
            budget_directory.path().join("large-b.txt"),
            vec![b'b'; 600 * 1024],
        )
        .expect("write second large file");
        let budget_done = run_local_inspection(
            request(
                "chatgpt",
                [budget_directory.path().display().to_string()],
                1,
                0,
            ),
            DiscoveryLimits::new(Some(10), Some(10), Some(1)),
        );
        assert_eq!(budget_done.status, TaskStatus::Failed);
        assert!(
            budget_done
                .discovery_error
                .as_deref()
                .is_some_and(|error| error.contains("1 MiB"))
        );
    }

    #[test]
    fn discovery_limit_constructor_preserves_unlimited_and_finite_values() {
        let unlimited = DiscoveryLimits::new(None, None, None);
        assert_eq!(unlimited.max_directories, None);
        assert_eq!(unlimited.scan_budget_bytes, None);
        assert_eq!(unlimited.max_files, None);

        let limits = DiscoveryLimits::new(Some(u32::MAX), Some(u32::MAX), Some(u32::MAX));
        assert_eq!(limits.max_directories, Some(u32::MAX as usize));
        assert_eq!(limits.max_files, Some(HARD_MAX_FILES));
        assert_eq!(
            limits.scan_budget_bytes,
            Some(u64::from(u32::MAX) * 1024 * 1024)
        );
    }

    #[test]
    fn unlimited_file_limit_bypasses_finite_and_legacy_hard_caps() {
        let state = tempfile::tempdir().expect("temporary task state");
        let finite_engine = test_engine(
            state.path().join("finite-history.json"),
            Arc::new(PreflightHandler),
            Arc::new(RecordingEvents::default()),
        );
        let entries = (0..3).map(|index| format!("synthetic-value-{index}"));
        let finite_error = finite_engine
            .start_with_probe(
                request("chatgpt", entries, 1, 0),
                None,
                0,
                DiscoveryLimits::new(None, Some(2), None),
            )
            .expect_err("three inputs must exceed a finite two-file limit");
        assert!(finite_error.contains("2 file paths"));

        let unlimited_engine = test_engine(
            state.path().join("unlimited-history.json"),
            Arc::new(PreflightHandler),
            Arc::new(RecordingEvents::default()),
        );
        let entries = (0..3).map(|index| format!("synthetic-value-{index}"));
        let started = unlimited_engine
            .start_with_probe(
                request("chatgpt", entries, 1, 0),
                None,
                0,
                DiscoveryLimits::new(None, None, None),
            )
            .expect("unlimited file discovery must accept every bounded IPC entry");
        let summary = wait_for_history(&unlimited_engine, &started.run_id);
        assert_eq!(summary.status, TaskStatus::Completed);
        assert_eq!(summary.total, 3);

        // Simulate an already-streamed discovery at the previous hard cap. This avoids a
        // 100,001-file fixture while proving `None` does not retain that hidden ceiling.
        let (input_sender, input_receiver) = mpsc::sync_channel(1);
        let (status_sender, status_receiver) = mpsc::sync_channel(1);
        let cancellation = CancellationToken::new();
        let mut producer = DiscoveryProducer {
            module_id: "chatgpt",
            limits: DiscoveryLimits::new(None, None, None),
            remaining_bytes: None,
            input_sender: &input_sender,
            status_sender: &status_sender,
            cancellation: &cancellation,
            prepare_artifacts: false,
            explicit_keys: HashSet::new(),
            excluded_directory: None,
            visited_directories: 0,
            discovered_files: HARD_MAX_FILES,
            next_ordinal: HARD_MAX_FILES,
            ready: HARD_MAX_FILES,
        };
        assert!(
            producer
                .submit_candidate(SourceCandidate {
                    value: Arc::from("synthetic-past-old-cap"),
                    from_directory: true,
                })
                .is_ok()
        );
        assert_eq!(producer.discovered_files, HARD_MAX_FILES + 1);
        assert!(matches!(
            status_receiver.recv().expect("candidate-ready status"),
            WorkerMessage::CandidateReady
        ));
        assert_eq!(
            input_receiver.recv().expect("queued task input").ordinal(),
            HARD_MAX_FILES + 1
        );
    }

    #[test]
    fn unlimited_discovery_limits_stream_the_complete_tree() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).expect("create nested directory");
        for (index, parent) in [directory.path(), nested.as_path()].into_iter().enumerate() {
            let path = parent.join(format!("cookies-{index}.txt"));
            let token = format!("synthetic_unlimited_{index:04}_{}", "U".repeat(48));
            let mut content = format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t4102444800\t__Secure-next-auth.session-token\t{token}\n"
            );
            content.push_str(&"#padding\n".repeat(70_000));
            fs::write(path, content).expect("write padded artifact");
        }

        let done = run_local_inspection(
            request("chatgpt", [directory.path().display().to_string()], 2, 0),
            DiscoveryLimits::new(None, None, None),
        );

        assert_eq!(done.status, TaskStatus::Completed);
        assert!(done.discovery_complete);
        assert!(done.discovery_error.is_none());
        assert_eq!(done.discovered, 2);
        assert_eq!(done.total, 2);
        assert_eq!(done.succeeded, 2);
    }

    #[test]
    fn empty_and_all_invalid_directories_finish_with_a_persisted_discovery_error() {
        let empty = tempfile::tempdir().expect("empty source directory");
        let empty_done = run_local_inspection(
            request("chatgpt", [empty.path().display().to_string()], 1, 0),
            DiscoveryLimits::new(None, Some(10), None),
        );
        assert_eq!(empty_done.status, TaskStatus::Failed);
        assert!(empty_done.discovery_complete);
        assert_eq!(empty_done.total, 0);
        assert!(empty_done.discovery_error.is_some());

        let source = tempfile::tempdir().expect("invalid source directory");
        fs::write(source.path().join("one.txt"), b"not an artifact")
            .expect("write first invalid artifact");
        fs::write(source.path().join("two.txt"), b"still not an artifact")
            .expect("write second invalid artifact");
        let state = tempfile::tempdir().expect("temporary task state");
        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            state.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            events.clone(),
        );
        let started = engine
            .start_with_probe(
                request("chatgpt", [source.path().display().to_string()], 1, 0),
                None,
                0,
                DiscoveryLimits::new(None, Some(10), None),
            )
            .expect("start invalid directory scan");
        let history = wait_for_history(&engine, &started.run_id);
        let done = wait_for_done(&events, &started.run_id);
        assert_eq!(done.status, TaskStatus::Failed);
        assert!(done.discovery_complete);
        assert_eq!(done.discovered, 2);
        assert_eq!(done.locally_filtered, 2);
        assert_eq!(done.total, 0);
        assert!(done.discovery_error.is_some());
        assert_eq!(history.discovery_error, done.discovery_error);
    }

    #[test]
    fn directory_plus_the_same_explicit_file_is_processed_once() {
        for explicit_first in [false, true] {
            let source = tempfile::tempdir().expect("temporary source directory");
            let artifact = source.path().join("cookies.txt");
            write_ready_artifact(&artifact, explicit_first as usize);
            let entries = if explicit_first {
                vec![
                    artifact.display().to_string(),
                    source.path().display().to_string(),
                ]
            } else {
                vec![
                    source.path().display().to_string(),
                    artifact.display().to_string(),
                ]
            };
            let state = tempfile::tempdir().expect("temporary task state");
            let probe = Arc::new(CountingProber::default());
            let engine = test_engine(
                state.path().join("task_history.json"),
                Arc::new(ModuleInspectionHandler),
                Arc::new(RecordingEvents::default()),
            );
            let started = engine
                .start_with_chatgpt_probe(
                    request("chatgpt", entries, 1, 0),
                    probe.clone(),
                    0,
                    DiscoveryLimits::default(),
                )
                .expect("start deduplicated scan");
            let history = wait_for_history(&engine, &started.run_id);
            assert_eq!(history.discovered, 1);
            assert_eq!(history.total, 1);
            assert_eq!(probe.calls.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    fn preparation_skips_macos_metadata_without_skipping_real_nested_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let macos_metadata = directory.path().join("__MACOSX");
        let real_nested = directory.path().join("nested");
        fs::create_dir_all(&macos_metadata).expect("create macOS metadata directory");
        fs::create_dir_all(&real_nested).expect("create real nested directory");

        for index in 0..5 {
            write_ready_artifact(&directory.path().join(format!("cookie-{index}.txt")), index);
            fs::write(
                macos_metadata.join(format!("._cookie-{index}.txt")),
                b"apple-double",
            )
            .expect("write AppleDouble fixture");
        }
        fs::write(directory.path().join(".DS_Store"), b"finder metadata")
            .expect("write Finder metadata fixture");
        write_ready_artifact(&real_nested.join("nested-cookie.txt"), 99);

        let done = run_local_inspection(
            request("chatgpt", [directory.path().display().to_string()], 4, 0),
            DiscoveryLimits::default(),
        );

        assert_eq!(done.discovered, 6);
        assert_eq!(done.total, 6);
        assert_eq!(done.locally_filtered, 0);
        assert_eq!(done.succeeded, 6);
    }

    #[test]
    fn directory_preflight_only_queues_ready_artifacts_and_preserves_explicit_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let ready = directory.path().join("ready.txt");
        let malformed = directory.path().join("notes.txt");
        let expired = directory.path().join("expired.txt");
        write_ready_artifact(&ready, 1);
        fs::write(&malformed, b"not an authentication export").expect("write malformed fixture");
        write_expired_artifact(&expired);

        let done = run_local_inspection(
            request("chatgpt", [directory.path().display().to_string()], 4, 0),
            DiscoveryLimits::default(),
        );
        assert_eq!(done.discovered, 3);
        assert_eq!(done.locally_filtered, 2);
        assert_eq!(done.total, 1);

        for entries in [
            vec![
                directory.path().display().to_string(),
                malformed.display().to_string(),
            ],
            vec![
                malformed.display().to_string(),
                directory.path().display().to_string(),
            ],
        ] {
            let explicit = run_local_inspection(
                request("chatgpt", entries, 4, 0),
                DiscoveryLimits::default(),
            );
            assert_eq!(explicit.discovered, 3);
            assert_eq!(explicit.locally_filtered, 1);
            assert_eq!(explicit.total, 2);
            assert_eq!(explicit.succeeded, 1);
            assert_eq!(explicit.failed, 1);
        }
    }

    #[test]
    fn remote_probe_receives_only_directory_artifacts_that_pass_local_preflight() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_ready_artifact(&directory.path().join("ready.txt"), 1);
        fs::write(directory.path().join("notes.txt"), b"not a cookie")
            .expect("write unrelated fixture");
        write_expired_artifact(&directory.path().join("expired.txt"));

        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            directory.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            events,
        );
        let probe = Arc::new(CountingProber::default());
        let started = engine
            .start_with_chatgpt_probe(
                request("chatgpt", [directory.path().display().to_string()], 4, 0),
                probe.clone(),
                0,
                DiscoveryLimits::default(),
            )
            .expect("start preflighted task");

        assert_eq!(started.discovered, 0);
        assert_eq!(started.locally_filtered, 0);
        assert_eq!(started.total, 0);
        assert!(!started.discovery_complete);
        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.total, 1);
        assert_eq!(summary.succeeded, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.discovered, 3);
        assert_eq!(summary.locally_filtered, 2);
        assert_eq!(probe.calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn per_file_cap_rejects_oversized_files_at_execution() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let oversized = directory.path().join("oversized.txt");
        // Larger than the per-file artifact cap, so the read is rejected before the probe.
        fs::write(&oversized, vec![b'x'; 2 * 1024 * 1024 + 1]).expect("write oversized fixture");

        let engine = test_engine(
            directory.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            Arc::new(RecordingEvents::default()),
        );
        let probe = Arc::new(CountingProber::default());
        let started = engine
            .start_with_chatgpt_probe(
                request("chatgpt", [oversized.display().to_string()], 1, 0),
                probe.clone(),
                0,
                DiscoveryLimits::default(),
            )
            .expect("start task");

        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.succeeded, 0);
        assert_eq!(probe.calls.load(Ordering::Acquire), 0);
    }

    #[test]
    fn valid_explicit_files_are_not_failed_by_a_shared_execution_budget() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut entries = Vec::new();
        for index in 0..4 {
            let path = directory.path().join(format!("cookies-{index}.txt"));
            let token = format!("synthetic_bulk_{index:04}_{}", "A".repeat(48));
            let mut content = format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t4102444800\t__Secure-next-auth.session-token\t{token}\n"
            );
            // ~360 KiB of ignored comment lines so the four files together exceed the 1 MiB
            // discovery budget; each stays under the per-file cap and must still succeed.
            content.push_str(&"#padding\n".repeat(40_000));
            fs::write(&path, content).expect("write padded cookie");
            entries.push(path.display().to_string());
        }

        let engine = test_engine(
            directory.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            Arc::new(RecordingEvents::default()),
        );
        let probe = Arc::new(CountingProber::default());
        let started = engine
            .start_with_chatgpt_probe(
                request("chatgpt", entries, 4, 0),
                probe.clone(),
                0,
                DiscoveryLimits::new(Some(10), Some(10), Some(1)),
            )
            .expect("start task");

        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.succeeded, 4);
        assert_eq!(summary.failed, 0);
        assert_eq!(probe.calls.load(Ordering::Acquire), 4);
    }

    #[test]
    fn oversized_unrelated_file_does_not_abort_directory_discovery() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_ready_artifact(&directory.path().join("cookies.txt"), 1);
        // A large unrelated file exceeding both the per-file cap and the scan budget must be
        // skipped, not abort discovery of the valid cookie beside it.
        fs::write(
            directory.path().join("video.bin"),
            vec![b'x'; 3 * 1024 * 1024],
        )
        .expect("write oversized file");

        let done = run_local_inspection(
            request("chatgpt", [directory.path().display().to_string()], 4, 0),
            DiscoveryLimits::new(Some(10), Some(10), Some(1)),
        );
        assert_eq!(done.status, TaskStatus::Completed);
        assert_eq!(done.discovered, 2);
        assert_eq!(done.total, 1);
        assert_eq!(done.locally_filtered, 1);
    }

    #[test]
    #[ignore = "requires AYLA_AUTH_EXAMPLES and explicit local authorization"]
    fn discovers_authorized_external_examples_without_remote_calls() {
        let Some(root) = std::env::var_os("AYLA_AUTH_EXAMPLES") else {
            return;
        };
        let done = run_local_inspection(
            request("chatgpt", [PathBuf::from(root).display().to_string()], 4, 0),
            DiscoveryLimits::default(),
        );

        assert!(done.total > 0);
        assert_eq!(done.discovered, done.total + done.locally_filtered);
        println!(
            "authorized aggregate only: discovered={}, structurally_ready={}, locally_filtered={}",
            done.discovered, done.total, done.locally_filtered
        );
    }

    #[test]
    fn every_unique_entry_is_consumed_once_with_bounded_concurrency() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let handler = Arc::new(TrackingHandler::new(Duration::from_millis(15)));
        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            directory.path().join("task_history.json"),
            handler.clone(),
            events.clone(),
        );

        let mut entries: Vec<String> = (0..40).map(|index| format!("value-{index}")).collect();
        entries.extend(["value-1".to_string(), " value-2 ".to_string()]);
        let started = engine
            .start(request("chatgpt", entries, 4, 0))
            .expect("start task");
        assert_eq!(started.total, 0);
        assert_eq!(started.discovered, 0);
        assert!(!started.discovery_complete);

        let second = engine.start(request("chatgpt", ["blocked".to_string()], 1, 0));
        assert!(second.is_err(), "only one global run may be active");

        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.status, TaskStatus::Completed);
        assert_eq!(summary.succeeded, 40);
        assert_eq!(summary.skipped, 0);
        assert!(handler.peak.load(Ordering::Acquire) > 1);
        assert!(handler.peak.load(Ordering::Acquire) <= 4);

        let snapshots = lock_unpoison(&events.progress);
        let mut completed_total = None;
        for snapshot in snapshots
            .iter()
            .filter(|item| item.run_id == started.run_id)
        {
            assert_eq!(
                snapshot.discovered,
                snapshot.total + snapshot.locally_filtered
            );
            assert_eq!(
                snapshot.total,
                snapshot.queued + snapshot.running + snapshot.processed + snapshot.skipped
            );
            if !snapshot.discovery_complete && snapshot.status == TaskStatus::Running {
                assert_eq!(snapshot.percent, 0.0);
            }
            if snapshot.discovery_complete {
                let final_total = *completed_total.get_or_insert(snapshot.total);
                assert_eq!(snapshot.total, final_total);
            }
        }

        let seen = lock_unpoison(&handler.seen);
        assert_eq!(seen.len(), 40);
        assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 40);
        assert!(engine.list_active().is_empty());
    }

    #[test]
    fn cancellation_interrupts_delay_and_is_accounted_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = test_engine(
            directory.path().join("task_history.json"),
            Arc::new(PreflightHandler),
            Arc::new(RecordingEvents::default()),
        );
        let entries = (0..50).map(|index| format!("value-{index}"));
        let started = engine
            .start(request("chatgpt", entries, 4, 2_000))
            .expect("start task");

        // Four workers plus an 8-item bounded queue can retain twelve items. Wait until the
        // producer has published the thirteenth and is backpressured before cancelling.
        let fill_started = Instant::now();
        loop {
            let accepted = engine
                .get_active(&started.run_id)
                .map_or(0, |snapshot| snapshot.total);
            if accepted >= 13 {
                break;
            }
            assert!(
                fill_started.elapsed() < Duration::from_secs(1),
                "the bounded producer did not reach backpressure"
            );
            thread::sleep(Duration::from_millis(2));
        }
        thread::sleep(Duration::from_millis(25));
        let retained = engine
            .get_active(&started.run_id)
            .expect("task remains active while workers are delayed")
            .total;
        assert!(
            retained <= 13,
            "the producer retained {retained} items instead of respecting the bounded queue"
        );
        let cancel_started = Instant::now();
        let cancelling = engine
            .cancel(&started.run_id)
            .expect("active task should be cancellable");
        assert_eq!(cancelling.status, TaskStatus::Cancelling);

        let summary = wait_for_history(&engine, &started.run_id);
        assert!(cancel_started.elapsed() < Duration::from_millis(500));
        assert_eq!(summary.status, TaskStatus::Cancelled);
        assert!(summary.skipped > 0);
        assert_eq!(
            summary.total,
            summary.succeeded + summary.failed + summary.skipped
        );
        assert!(engine.cancel(&started.run_id).is_none());
    }

    #[test]
    fn persisted_history_and_events_never_contain_input_secrets() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("task_history.json");
        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(path.clone(), Arc::new(PreflightHandler), events.clone());
        let secret = "person@example.test:super-secret-value";

        let started = engine
            .start(request("chatgpt", [secret.to_string()], 1, 0))
            .expect("start task");
        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.status, TaskStatus::Completed);

        let disk = fs::read_to_string(&path).expect("read persisted history");
        let event_json = serde_json::to_string(&(
            lock_unpoison(&events.progress).clone(),
            lock_unpoison(&events.done).clone(),
        ))
        .expect("serialize recorded events");

        for output in [&disk, &event_json] {
            assert!(!output.contains(secret));
            assert!(!output.contains("super-secret-value"));
            assert!(!output.contains("person@example.test"));
        }
        assert!(!sidecar_path(&path, "tmp").exists());

        let reopened = test_engine(
            path.clone(),
            Arc::new(PreflightHandler),
            Arc::new(RecordingEvents::default()),
        );
        assert_eq!(reopened.history(Some(1)), vec![summary]);
        reopened.clear_history().expect("clear history");
        assert!(reopened.history(None).is_empty());

        let cleared = fs::read_to_string(path).expect("read cleared history");
        assert!(!cleared.contains(&started.run_id));
    }

    #[test]
    fn production_inspection_persists_only_aggregate_counts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let artifact_name = "synthetic-auth-fixture.txt";
        let artifact_path = directory.path().join(artifact_name);
        let synthetic_token = "synthetic_engine_token_value_that_must_never_be_persisted";
        fs::write(
            &artifact_path,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t4102444800\t__Secure-next-auth.session-token\t{synthetic_token}\n"
            ),
        )
        .expect("write synthetic artifact");

        let history_path = directory.path().join("task_history.json");
        let input_path = artifact_path.to_string_lossy().into_owned();
        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            history_path.clone(),
            Arc::new(ModuleInspectionHandler),
            events.clone(),
        );

        let started = engine
            .start(request("chatgpt", [input_path.clone()], 1, 0))
            .expect("start local inspection");
        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.status, TaskStatus::Completed);
        assert_eq!(summary.succeeded, 1);
        assert_eq!(summary.failed, 0);

        let disk = fs::read_to_string(history_path).expect("read persisted history");
        let event_json = serde_json::to_string(&(
            lock_unpoison(&events.progress).clone(),
            lock_unpoison(&events.done).clone(),
        ))
        .expect("serialize recorded events");
        for output in [&disk, &event_json] {
            assert!(!output.contains(synthetic_token));
            assert!(!output.contains(&input_path));
            assert!(!output.contains(artifact_name));
        }
    }

    #[test]
    fn twitch_probe_updates_generic_summary_without_chatgpt_regression() {
        let source_directory = tempfile::tempdir().expect("temporary source directory");
        let state_directory = tempfile::tempdir().expect("temporary state directory");
        let mut entries = Vec::new();
        for index in 0..4 {
            let path = source_directory.path().join(format!("twitch-{index}.txt"));
            write_twitch_artifact(&path, index);
            entries.push(path.display().to_string());
        }

        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            state_directory.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            events.clone(),
        );
        let probe = Arc::new(SequencedCookieProber::new([
            ModuleProbeResult {
                status: ModuleProbeStatus::Active(ModulePlan::Twitch(TwitchPlan {
                    has_prime: true,
                    has_turbo: false,
                    role: TwitchRole::Affiliate,
                })),
                retries: 2,
            },
            ModuleProbeResult {
                status: ModuleProbeStatus::Dead,
                retries: 1,
            },
            ModuleProbeResult {
                status: ModuleProbeStatus::RateLimited,
                retries: 3,
            },
            ModuleProbeResult {
                status: ModuleProbeStatus::Error,
                retries: 0,
            },
        ]));

        let started = engine
            .start_with_cookie_probe(
                request("twitch", entries, 1, 0),
                probe,
                0,
                DiscoveryLimits::default(),
            )
            .expect("start Twitch task");
        let history = wait_for_history(&engine, &started.run_id);
        let done = wait_for_done(&events, &started.run_id);

        assert_eq!(history.succeeded, 1);
        assert_eq!(history.failed, 3);
        assert_eq!(done.retried, 6);
        assert!(done.chatgpt.is_none());
        assert_eq!(
            done.module_summary,
            Some(ModuleTaskSummary {
                active: 1,
                authenticated_unknown: 0,
                no_entitlement: 0,
                dead: 1,
                rate_limited: 1,
                errors: 1,
                invalid: 0,
                plans: BTreeMap::from([("Prime + Affiliate".to_string(), 1)]),
            })
        );
    }

    #[test]
    fn max_no_entitlement_is_counted_and_exported_without_marking_the_session_dead() {
        let source_directory = tempfile::tempdir().expect("temporary source directory");
        let output_directory = tempfile::tempdir().expect("temporary output directory");
        let state_directory = tempfile::tempdir().expect("temporary state directory");
        let source = source_directory.path().join("max.txt");
        write_max_artifact(&source, 1);
        let source_bytes = fs::read(&source).expect("read Max source bytes");
        let plan = MaxPlan {
            tier: MaxTier::Premium,
            state: MaxSubscriptionState::Cancelled,
        };

        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            state_directory.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            events.clone(),
        );
        let probe = Arc::new(SequencedCookieProber::new([ModuleProbeResult {
            status: ModuleProbeStatus::NoEntitlement(ModulePlan::Max(plan)),
            retries: 1,
        }]));
        let mut task_request = request("max", [source.display().to_string()], 1, 0);
        task_request.output_directory = Some(output_directory.path().display().to_string());

        let started = engine
            .start_with_cookie_probe(task_request, probe, 0, DiscoveryLimits::default())
            .expect("start Max task");
        let history = wait_for_history(&engine, &started.run_id);
        let done = wait_for_done(&events, &started.run_id);

        assert_eq!(history.succeeded, 0);
        assert_eq!(history.failed, 1);
        assert_eq!(history.exported_active, 0);
        assert_eq!(history.exported_failed, 1);
        assert_eq!(done.retried, 1);
        assert_eq!(
            done.module_summary,
            Some(ModuleTaskSummary {
                active: 0,
                authenticated_unknown: 0,
                no_entitlement: 1,
                dead: 0,
                rate_limited: 0,
                errors: 0,
                invalid: 0,
                plans: BTreeMap::from([(plan.label(), 1)]),
            })
        );

        let run_prefix: String = started
            .run_id
            .trim_start_matches("task_")
            .chars()
            .filter(|character| character.is_ascii_hexdigit())
            .take(12)
            .collect();
        let exported = output_directory.path().join(format!(
            "max/failed/premium-cancelled__no-entitlement__{run_prefix}-p{:x}__000001.txt",
            std::process::id()
        ));
        assert_eq!(
            fs::read(exported).expect("read exported Max result"),
            source_bytes
        );
    }

    #[test]
    fn authenticated_max_with_unknown_plan_stays_distinct_and_exports_as_active() {
        let plan = ModulePlan::Max(MaxPlan::default());
        let outcome = HandlerOutcome::Module(ModuleProbeResult {
            status: ModuleProbeStatus::Authenticated(plan),
            retries: 2,
        });
        assert_eq!(
            export_classification(outcome),
            Some((
                true,
                "unknown-plan-unknown-status".to_string(),
                Some("plan-unavailable"),
            ))
        );

        let mut summary = ModuleTaskSummary::default();
        summary.record_authenticated(plan);
        assert_eq!(summary.active, 0);
        assert_eq!(summary.authenticated_unknown, 1);
        assert_eq!(summary.no_entitlement, 0);
        assert_eq!(summary.dead, 0);
        assert_eq!(
            summary.plans,
            BTreeMap::from([("Unknown plan (Unknown status)".to_string(), 1)])
        );
    }

    #[test]
    fn twitch_directory_prefilter_only_queues_endpoint_usable_auth() {
        let source_directory = tempfile::tempdir().expect("temporary source directory");
        write_twitch_artifact(&source_directory.path().join("valid.txt"), 1);
        fs::write(
            source_directory.path().join("wrong-scope.txt"),
            format!(
                "help.twitch.tv\tFALSE\t/\tTRUE\t4102444800\tauth-token\tsynthetic_twitch_wrong_scope_{}\n",
                "W".repeat(48)
            ),
        )
        .expect("write wrong-scope Twitch fixture");

        let done = run_local_inspection(
            request(
                "twitch",
                [source_directory.path().display().to_string()],
                1,
                0,
            ),
            DiscoveryLimits::default(),
        );

        assert_eq!(done.discovered, 2);
        assert_eq!(done.locally_filtered, 1);
        assert_eq!(done.total, 1);
        assert_eq!(done.succeeded, 1);
    }

    #[test]
    fn result_exporter_routes_plan_results_without_overwriting() {
        let output_directory = tempfile::tempdir().expect("temporary output directory");
        let output_root = fs::canonicalize(output_directory.path()).expect("resolve output root");
        let exporter = ResultExporter::new(&output_root, "chatgpt", "task_abcdef0123456789_1")
            .expect("create exporter");
        let run_tag = format!("abcdef012345-p{:x}", std::process::id());

        assert_eq!(
            exporter.export(
                1,
                HandlerOutcome::ChatGpt(ChatGptProbeResult {
                    status: ChatGptProbeStatus::Active(ChatGptPlan::Plus),
                    retries: 0,
                }),
                Some(b"active-cookie-bytes")
            ),
            ExportRecord::Active
        );
        assert_eq!(
            exporter.export(
                2,
                HandlerOutcome::ChatGpt(ChatGptProbeResult {
                    status: ChatGptProbeStatus::Dead,
                    retries: 0,
                }),
                Some(b"failed-cookie-bytes")
            ),
            ExportRecord::Failed
        );
        assert_eq!(
            exporter.export(
                3,
                HandlerOutcome::ChatGpt(ChatGptProbeResult {
                    status: ChatGptProbeStatus::Authenticated(ChatGptPlanLookup::Unknown),
                    retries: 0,
                }),
                Some(b"unknown-plan-cookie-bytes")
            ),
            ExportRecord::Active
        );
        assert_eq!(
            exporter.export(
                4,
                HandlerOutcome::ChatGpt(ChatGptProbeResult {
                    status: ChatGptProbeStatus::Authenticated(ChatGptPlanLookup::Unavailable),
                    retries: 0,
                }),
                Some(b"unavailable-plan-cookie-bytes")
            ),
            ExportRecord::Active
        );

        let active_path = output_directory
            .path()
            .join(format!("chatgpt/active/plus__{run_tag}__000001.txt"));
        let failed_path = output_directory.path().join(format!(
            "chatgpt/failed/unknown-plan__dead__{run_tag}__000002.txt"
        ));
        let unknown_path = output_directory.path().join(format!(
            "chatgpt/active/unknown-plan__plan-unknown__{run_tag}__000003.txt"
        ));
        let unavailable_path = output_directory.path().join(format!(
            "chatgpt/active/unknown-plan__plan-unavailable__{run_tag}__000004.txt"
        ));
        assert_eq!(
            fs::read(&active_path).expect("read active copy"),
            b"active-cookie-bytes"
        );
        assert_eq!(
            fs::read(&failed_path).expect("read failed copy"),
            b"failed-cookie-bytes"
        );
        assert_eq!(
            fs::read(&unknown_path).expect("read unknown-plan copy"),
            b"unknown-plan-cookie-bytes"
        );
        assert_eq!(
            fs::read(&unavailable_path).expect("read unavailable-plan copy"),
            b"unavailable-plan-cookie-bytes"
        );

        assert_eq!(
            exporter.export(
                1,
                HandlerOutcome::ChatGpt(ChatGptProbeResult {
                    status: ChatGptProbeStatus::Active(ChatGptPlan::Plus),
                    retries: 0,
                }),
                Some(b"changed-source")
            ),
            ExportRecord::Error,
            "an existing result must never be overwritten"
        );
        assert_eq!(
            fs::read(active_path).expect("read preserved copy"),
            b"active-cookie-bytes"
        );
    }

    #[test]
    fn result_exporter_uses_module_plan_slug_for_twitch() {
        let output_directory = tempfile::tempdir().expect("temporary output directory");
        let output_root = fs::canonicalize(output_directory.path()).expect("resolve output root");
        let exporter = ResultExporter::new(&output_root, "twitch", "task_123456789abc0000_1")
            .expect("create Twitch exporter");
        let run_tag = format!("123456789abc-p{:x}", std::process::id());

        assert_eq!(
            exporter.export(
                7,
                HandlerOutcome::Module(ModuleProbeResult {
                    status: ModuleProbeStatus::Active(ModulePlan::Twitch(TwitchPlan {
                        has_prime: true,
                        has_turbo: true,
                        role: TwitchRole::Partner,
                    })),
                    retries: 0,
                }),
                Some(b"synthetic-twitch-cookie-bytes"),
            ),
            ExportRecord::Active
        );

        let exported = output_directory.path().join(format!(
            "twitch/active/prime-turbo-partner__{run_tag}__000007.txt"
        ));
        assert_eq!(
            fs::read(exported).expect("read Twitch result copy"),
            b"synthetic-twitch-cookie-bytes"
        );
    }

    #[test]
    fn atomic_publication_never_replaces_an_existing_file() {
        let directory = tempfile::tempdir().expect("temporary output directory");
        let partial = directory.path().join("result.part");
        let destination = directory.path().join("result.txt");
        fs::write(&partial, b"new-result").expect("write partial result");
        fs::write(&destination, b"existing-result").expect("write existing result");

        assert!(publish_without_overwrite(&partial, &destination).is_err());
        assert_eq!(
            fs::read(&destination).expect("read existing result"),
            b"existing-result"
        );
        assert_eq!(
            fs::read(&partial).expect("read unpublished partial"),
            b"new-result"
        );
    }

    #[test]
    fn concurrent_exporters_can_initialize_the_same_results_root() {
        let output_directory = tempfile::tempdir().expect("temporary output directory");
        let output_root = fs::canonicalize(output_directory.path()).expect("resolve output root");
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let first_root = output_root.clone();
        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            ResultExporter::new(&first_root, "chatgpt", "task_111111111111_1")
        });
        let second_root = output_root.clone();
        let second = std::thread::spawn(move || {
            barrier.wait();
            ResultExporter::new(&second_root, "chatgpt", "task_222222222222_2")
        });

        assert!(first.join().expect("join first initializer").is_ok());
        assert!(second.join().expect("join second initializer").is_ok());
        assert!(is_valid_results_marker(
            &output_root.join(RESULTS_MARKER_FILE)
        ));
    }

    #[test]
    fn task_export_uses_the_exact_bytes_that_were_validated() {
        let source_directory = tempfile::tempdir().expect("temporary source directory");
        let output_directory = tempfile::tempdir().expect("temporary output directory");
        let state_directory = tempfile::tempdir().expect("temporary state directory");
        let source = source_directory.path().join("source.txt");
        write_ready_artifact(&source, 41);
        let validated_bytes = fs::read(&source).expect("read original source");
        let replacement = b"source changed while the remote probe was running".to_vec();

        let engine = test_engine(
            state_directory.path().join("task_history.json"),
            Arc::new(ModuleInspectionHandler),
            Arc::new(RecordingEvents::default()),
        );
        let mut export_request = request("chatgpt", [source.display().to_string()], 1, 0);
        export_request.output_directory = Some(output_directory.path().display().to_string());
        let started = engine
            .start_with_chatgpt_probe(
                export_request,
                Arc::new(MutatingProber {
                    source: source.clone(),
                    replacement: replacement.clone(),
                }),
                0,
                DiscoveryLimits::default(),
            )
            .expect("start task with mutating probe");
        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.exported_active, 1);
        assert_eq!(fs::read(&source).expect("read changed source"), replacement);

        let exported = fs::read_dir(output_directory.path().join("chatgpt/active"))
            .expect("list active output")
            .find_map(|entry| {
                let path = entry.ok()?.path();
                (path.extension().and_then(|value| value.to_str()) == Some("txt"))
                    .then(|| fs::read(path).ok())
                    .flatten()
            })
            .expect("read exported result");
        assert_eq!(exported, validated_bytes);
    }

    #[test]
    fn task_export_copies_parallel_results_and_persists_only_aggregate_counts() {
        let source_directory = tempfile::tempdir().expect("temporary source directory");
        let output_directory = tempfile::tempdir().expect("temporary output directory");
        let state_directory = tempfile::tempdir().expect("temporary state directory");
        let history_path = state_directory.path().join("task_history.json");
        let mut entries = Vec::new();
        for index in 0..8 {
            let source = source_directory.path().join(format!("source-{index}.txt"));
            write_ready_artifact(&source, index);
            entries.push(source.display().to_string());
        }

        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            history_path.clone(),
            Arc::new(ModuleInspectionHandler),
            events.clone(),
        );
        let prober = Arc::new(CountingProber::default());
        let mut export_request = request("chatgpt", entries, 4, 0);
        export_request.output_directory = Some(output_directory.path().display().to_string());
        let started = engine
            .start_with_chatgpt_probe(export_request, prober, 0, DiscoveryLimits::default())
            .expect("start exporting task");
        assert!(started.results_export_enabled);

        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.succeeded, 8);
        assert_eq!(summary.exported_active, 8);
        assert_eq!(summary.exported_failed, 0);
        assert_eq!(summary.export_errors, 0);

        let active_directory = output_directory.path().join("chatgpt/active");
        let names: HashSet<_> = fs::read_dir(active_directory)
            .expect("list active results")
            .map(|entry| {
                entry
                    .expect("read active result")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names.len(), 8);
        assert!(names.iter().all(|name| name.starts_with("free__")));

        let output_path = output_directory.path().display().to_string();
        let source_path = source_directory.path().display().to_string();
        let disk = fs::read_to_string(history_path).expect("read task history");
        let event_json = serde_json::to_string(&(
            lock_unpoison(&events.progress).clone(),
            lock_unpoison(&events.done).clone(),
        ))
        .expect("serialize events");
        for serialized in [&disk, &event_json] {
            assert!(!serialized.contains(&output_path));
            assert!(!serialized.contains(&source_path));
        }
    }

    #[test]
    fn preparation_rejects_relative_output_and_excludes_previous_results() {
        let mut relative = request("chatgpt", ["explicit.txt".to_string()], 1, 0);
        relative.output_directory = Some("relative/results".to_string());
        assert!(prepare(relative).is_err());

        let same_root = tempfile::tempdir().expect("temporary overlapping root");
        write_ready_artifact(&same_root.path().join("source.txt"), 0);
        let mut overlapping = request("chatgpt", [same_root.path().display().to_string()], 1, 0);
        overlapping.output_directory = Some(same_root.path().display().to_string());
        assert!(prepare(overlapping).is_err());
        assert!(
            !same_root.path().join(RESULTS_MARKER_FILE).exists(),
            "a rejected setup must not mark the source as a result directory"
        );

        let workspace = tempfile::tempdir().expect("temporary workspace");
        let source = workspace.path().join("source.txt");
        write_ready_artifact(&source, 1);
        let previous_root = workspace.path().join("results-a");
        fs::create_dir(&previous_root).expect("create previous result root");
        let previous_root = fs::canonicalize(previous_root).expect("resolve previous result root");
        ensure_results_marker(&previous_root).expect("mark previous result root");
        let previous_results = previous_root.join("chatgpt/active");
        fs::create_dir_all(&previous_results).expect("create previous result directories");
        write_ready_artifact(&previous_results.join("free__previous__000001.txt"), 2);

        let next_results = workspace.path().join("results-b");
        fs::create_dir(&next_results).expect("create next result root");

        let mut next = request("chatgpt", [workspace.path().display().to_string()], 2, 0);
        next.output_directory = Some(next_results.display().to_string());
        let done = run_local_inspection(next, DiscoveryLimits::default());
        assert_eq!(done.status, TaskStatus::Completed);
        assert_eq!(done.discovered, 1);
        assert_eq!(done.total, 1);
    }

    #[test]
    fn invalid_marker_content_does_not_hide_source_files() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let nested = workspace.path().join("nested");
        fs::create_dir(&nested).expect("create nested directory");
        fs::write(nested.join(RESULTS_MARKER_FILE), b"unrelated file")
            .expect("write unrelated marker name");
        write_ready_artifact(&nested.join("source.txt"), 8);

        let done = run_local_inspection(
            request("chatgpt", [workspace.path().display().to_string()], 1, 0),
            DiscoveryLimits::default(),
        );
        assert_eq!(done.status, TaskStatus::Completed);
        assert_eq!(done.discovered, 1);
        assert_eq!(done.total, 1);
    }

    #[test]
    fn failed_history_persistence_is_reported_without_exposing_inputs() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let blocker = directory.path().join("not-a-directory");
        fs::write(&blocker, "block history directory creation").expect("write blocker");
        let history_path = blocker.join("task_history.json");
        let events = Arc::new(RecordingEvents::default());
        let engine = test_engine(
            history_path.clone(),
            Arc::new(PreflightHandler),
            events.clone(),
        );

        let started = engine
            .start(request(
                "chatgpt",
                ["synthetic-entry-never-persisted".to_string()],
                1,
                0,
            ))
            .expect("start task");
        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.status, TaskStatus::Completed);

        let wait_started = Instant::now();
        let done = loop {
            if let Some(done) = lock_unpoison(&events.done)
                .iter()
                .find(|snapshot| snapshot.run_id == started.run_id)
                .cloned()
            {
                break done;
            }
            assert!(
                wait_started.elapsed() < Duration::from_secs(5),
                "done event did not arrive"
            );
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(done.history_persisted, Some(false));
        assert!(!history_path.exists());
        let event_json = serde_json::to_string(&done).expect("serialize done event");
        assert!(!event_json.contains("synthetic-entry-never-persisted"));
    }

    #[test]
    fn history_keeps_only_the_latest_one_hundred_summaries() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("task_history.json");
        let engine = test_engine(
            path.clone(),
            Arc::new(PreflightHandler),
            Arc::new(RecordingEvents::default()),
        );
        let mut first_run_id = String::new();
        let mut last_run_id = String::new();

        for index in 0..=MAX_HISTORY {
            let started = engine
                .start(request("chatgpt", [format!("history-entry-{index}")], 1, 0))
                .expect("start history task");
            if index == 0 {
                first_run_id = started.run_id.clone();
            }
            last_run_id = started.run_id.clone();
            wait_for_history(&engine, &started.run_id);
        }

        let history = engine.history(None);
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history[0].run_id, last_run_id);
        assert!(!history.iter().any(|item| item.run_id == first_run_id));

        let reopened = test_engine(
            path,
            Arc::new(PreflightHandler),
            Arc::new(RecordingEvents::default()),
        );
        assert_eq!(reopened.history(None).len(), MAX_HISTORY);
        assert_eq!(reopened.history(Some(1))[0].run_id, last_run_id);
    }
}
