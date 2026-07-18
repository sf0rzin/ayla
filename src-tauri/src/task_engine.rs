use crate::{
    auth_artifact, catalog,
    chatgpt_client::{ChatGptPlan, ChatGptProbeResult, ChatGptProbeStatus, ChatGptProber},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};

const DEFAULT_MAX_DIRECTORIES: usize = 1_000;
const DEFAULT_MAX_FILES: usize = 10_000;
const DEFAULT_SCAN_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_DIRECTORIES: usize = 10_000;
const HARD_MAX_FILES: usize = 100_000;
const HARD_MAX_SCAN_BUDGET_MIB: u32 = 4_096;
const MAX_RAW_ENTRIES: usize = 20_000;
const MAX_ENTRY_BYTES: usize = 32 * 1024;
const MAX_TOTAL_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_WORKERS: usize = 32;
const MAX_DISCOVERY_WORKERS: usize = 8;
const MAX_HISTORY: usize = 100;
const MAX_DELAY_MS: u64 = 60_000;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
const HISTORY_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscoveryLimits {
    max_directories: usize,
    max_files: usize,
    scan_budget_bytes: u64,
}

impl DiscoveryLimits {
    pub(crate) fn new(max_directories: u32, max_files: u32, scan_budget_mib: u32) -> Self {
        Self {
            max_directories: (max_directories as usize).clamp(1, HARD_MAX_DIRECTORIES),
            max_files: (max_files as usize).clamp(1, HARD_MAX_FILES),
            scan_budget_bytes: u64::from(scan_budget_mib.clamp(1, HARD_MAX_SCAN_BUDGET_MIB))
                .saturating_mul(1024 * 1024),
        }
    }

    fn max_directory_items(self) -> usize {
        self.max_files.saturating_add(self.max_directories)
    }
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            max_directories: DEFAULT_MAX_DIRECTORIES,
            max_files: DEFAULT_MAX_FILES,
            scan_budget_bytes: DEFAULT_SCAN_BUDGET_BYTES,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub run_id: String,
    pub module_id: String,
    pub status: TaskStatus,
    pub total: usize,
    pub discovered: usize,
    pub locally_filtered: usize,
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
    pub chatgpt: Option<ChatGptTaskSummary>,
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
}

struct TaskInput(Arc<str>);

impl TaskInput {
    fn into_inner(self) -> Arc<str> {
        self.0
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

struct TaskContext {
    cancellation: CancellationToken,
    chatgpt_probe: Option<Arc<dyn ChatGptProber>>,
}

impl TaskContext {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

#[derive(Clone, Copy)]
enum HandlerOutcome {
    Succeeded,
    Failed,
    Skipped,
    ChatGpt(ChatGptProbeResult),
}

#[derive(Clone, Copy)]
enum WorkerMessage {
    Started,
    Finished(HandlerOutcome),
}

trait TaskHandler: Send + Sync + 'static {
    fn process(&self, module_id: &str, input: TaskInput, context: &TaskContext) -> HandlerOutcome;
}

struct ModuleInspectionHandler;

impl TaskHandler for ModuleInspectionHandler {
    fn process(&self, module_id: &str, input: TaskInput, context: &TaskContext) -> HandlerOutcome {
        if context.is_cancelled() {
            return HandlerOutcome::Skipped;
        }

        let value = input.into_inner();
        let path = Path::new(value.as_ref());
        let outcome = match module_id {
            "chatgpt" if path.is_absolute() => {
                // Each file is bounded independently by the per-file artifact cap. There is
                // no shared aggregate budget at execution, so a structurally valid file is
                // never failed merely because earlier files consumed a common allowance.
                let budget = AtomicU64::new(auth_artifact::MAX_ARTIFACT_BYTES);
                match auth_artifact::prepare_chatgpt_path_with_budget(path, &budget) {
                    Ok(auth) => context
                        .chatgpt_probe
                        .as_ref()
                        .map_or(HandlerOutcome::Succeeded, |probe| {
                            HandlerOutcome::ChatGpt(probe.check(&auth))
                        }),
                    Err(_) => HandlerOutcome::Failed,
                }
            }
            _ => HandlerOutcome::Failed,
        };
        drop(value);

        if context.is_cancelled() {
            HandlerOutcome::Skipped
        } else {
            outcome
        }
    }
}

#[cfg(test)]
struct PreflightHandler;

#[cfg(test)]
impl TaskHandler for PreflightHandler {
    fn process(&self, _module_id: &str, input: TaskInput, context: &TaskContext) -> HandlerOutcome {
        if context.is_cancelled() {
            return HandlerOutcome::Skipped;
        }

        let valid = !input.into_inner().is_empty();
        if context.is_cancelled() {
            HandlerOutcome::Skipped
        } else if valid {
            HandlerOutcome::Succeeded
        } else {
            HandlerOutcome::Failed
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
        self.start_with_probe(request, Some(probe), proxy_count, discovery_limits)
    }

    fn start_with_probe(
        &self,
        request: StartTaskRequest,
        chatgpt_probe: Option<Arc<dyn ChatGptProber>>,
        proxy_count: usize,
        discovery_limits: DiscoveryLimits,
    ) -> Result<TaskSnapshot, String> {
        let PreparedTask {
            module_id,
            inputs,
            discovered,
            locally_filtered,
            requested_concurrency,
            delay_ms,
            use_proxy,
        } = prepare_with_limits(request, discovery_limits)?;

        if use_proxy && proxy_count == 0 {
            return Err("no active proxy is available".to_string());
        }
        let concurrency = effective_concurrency(requested_concurrency, use_proxy, proxy_count);

        let total = inputs.len();
        let run_id = new_task_id();
        let cancellation = CancellationToken::new();
        let snapshot = TaskSnapshot {
            run_id: run_id.clone(),
            module_id: module_id.clone(),
            status: TaskStatus::Running,
            total,
            discovered,
            locally_filtered,
            queued: total,
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
            chatgpt: (module_id == "chatgpt").then(ChatGptTaskSummary::default),
        };

        {
            let mut state = lock_unpoison(&self.inner.state);
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

        let inner = Arc::clone(&self.inner);
        let worker_run_id = run_id.clone();
        let spawn_result = thread::Builder::new()
            .name(format!("ayla-task-{run_id}"))
            .spawn(move || {
                run_task(
                    inner,
                    worker_run_id,
                    module_id,
                    inputs,
                    concurrency,
                    delay_ms,
                    chatgpt_probe,
                    cancellation,
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

            match message {
                WorkerMessage::Started => {
                    active.snapshot.queued = active.snapshot.queued.saturating_sub(1);
                    active.snapshot.running = active.snapshot.running.saturating_add(1);
                }
                WorkerMessage::Finished(outcome) => {
                    active.snapshot.running = active.snapshot.running.saturating_sub(1);
                    match outcome {
                        HandlerOutcome::Succeeded => {
                            active.snapshot.succeeded = active.snapshot.succeeded.saturating_add(1);
                            active.snapshot.processed = active.snapshot.processed.saturating_add(1);
                        }
                        HandlerOutcome::Failed => {
                            active.snapshot.failed = active.snapshot.failed.saturating_add(1);
                            active.snapshot.processed = active.snapshot.processed.saturating_add(1);
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
                                }
                                ChatGptProbeStatus::Dead => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.chatgpt.as_mut() {
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
                                }
                                ChatGptProbeStatus::Error => {
                                    active.snapshot.failed =
                                        active.snapshot.failed.saturating_add(1);
                                    if let Some(summary) = active.snapshot.chatgpt.as_mut() {
                                        summary.errors = summary.errors.saturating_add(1);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            active.snapshot.sequence = active.snapshot.sequence.saturating_add(1);
            active.snapshot.percent = percentage(
                active
                    .snapshot
                    .processed
                    .saturating_add(active.snapshot.skipped),
                active.snapshot.total,
            );

            let should_emit = active.last_progress_event.elapsed() >= PROGRESS_INTERVAL
                || active
                    .snapshot
                    .processed
                    .saturating_add(active.snapshot.skipped)
                    == active.snapshot.total;
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

        active.snapshot.status = if active.snapshot.processed == active.snapshot.total {
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
    inputs: VecDeque<TaskInput>,
    discovered: usize,
    locally_filtered: usize,
    requested_concurrency: usize,
    delay_ms: u64,
    use_proxy: bool,
}

struct SourceCandidate {
    value: Arc<str>,
    from_directory: bool,
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

    let mut candidate_indexes =
        HashMap::<Arc<str>, usize>::with_capacity(entries.len().min(discovery_limits.max_files));
    let mut candidates =
        Vec::<SourceCandidate>::with_capacity(entries.len().min(discovery_limits.max_files));
    let mut raw_input_bytes = 0usize;
    let mut expanded_input_bytes = 0usize;
    let mut visited_directories = 0usize;
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
        let path = Path::new(trimmed);
        let (expanded, from_directory) = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !is_link_or_reparse(&metadata) => (
                collect_directory_files(path, &mut visited_directories, discovery_limits)?,
                true,
            ),
            _ => (vec![PathBuf::from(trimmed)], false),
        };

        for candidate in expanded {
            let Some(value) = candidate.to_str() else {
                // A scanned directory can surface a non-UTF-8 filename; skip it rather than
                // aborting discovery of every other valid file in the batch. Explicit
                // entries always originate from valid UTF-8 input, so this only drops
                // unrepresentable discovered paths.
                continue;
            };
            let candidate_bytes = value.len();
            if candidate_bytes > MAX_ENTRY_BYTES {
                return Err(format!(
                    "each path can contain at most {MAX_ENTRY_BYTES} bytes"
                ));
            }
            let value: Arc<str> = Arc::from(value);
            if let Some(index) = candidate_indexes.get(&value).copied() {
                if !from_directory {
                    candidates[index].from_directory = false;
                }
                continue;
            }
            if candidates.len() >= discovery_limits.max_files {
                return Err(format!(
                    "the limit is {} unique files",
                    discovery_limits.max_files
                ));
            }
            expanded_input_bytes =
                checked_total_input_bytes(expanded_input_bytes, candidate_bytes)?;
            candidate_indexes.insert(Arc::clone(&value), candidates.len());
            candidates.push(SourceCandidate {
                value,
                from_directory,
            });
        }
    }

    if candidates.is_empty() {
        return Err("provide at least one valid entry".to_string());
    }

    let discovered = candidates.len();
    let ready =
        structurally_ready_candidates(&module_id, &candidates, discovery_limits.scan_budget_bytes)?;
    let inputs: VecDeque<_> = candidates
        .into_iter()
        .zip(ready)
        .filter_map(|(candidate, ready)| ready.then_some(TaskInput(candidate.value)))
        .collect();
    let locally_filtered = discovered.saturating_sub(inputs.len());
    if inputs.is_empty() {
        return Err(format!(
            "no structurally usable authentication files were found; {locally_filtered} unrelated or unusable files were ignored"
        ));
    }

    Ok(PreparedTask {
        module_id,
        inputs,
        discovered,
        locally_filtered,
        requested_concurrency: concurrency.min(MAX_WORKERS),
        delay_ms,
        use_proxy,
    })
}

fn structurally_ready_candidates(
    module_id: &str,
    candidates: &[SourceCandidate],
    scan_budget_bytes: u64,
) -> Result<Vec<bool>, String> {
    let scan_indices: Vec<_> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| candidate.from_directory.then_some(index))
        .collect();
    if module_id != "chatgpt" || scan_indices.is_empty() {
        return Ok(vec![true; candidates.len()]);
    }

    let mut scan_bytes = 0u64;
    for index in &scan_indices {
        if let Ok(metadata) = fs::metadata(Path::new(candidates[*index].value.as_ref())) {
            // Files above the per-file cap are rejected by the scan without consuming any
            // budget, so counting them here would let a single large unrelated file falsely
            // abort discovery of the whole batch.
            if metadata.len() > auth_artifact::MAX_ARTIFACT_BYTES {
                continue;
            }
            scan_bytes = scan_bytes
                .checked_add(metadata.len())
                .filter(|total| *total <= scan_budget_bytes)
                .ok_or_else(|| {
                    format!(
                        "the local discovery scan can inspect at most {} MiB",
                        scan_budget_bytes / (1024 * 1024)
                    )
                })?;
        }
    }

    let ready: Vec<_> = candidates
        .iter()
        .map(|candidate| AtomicBool::new(!candidate.from_directory))
        .collect();
    let next = AtomicUsize::new(0);
    let remaining_bytes = AtomicU64::new(scan_budget_bytes);
    let workers = thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(MAX_DISCOVERY_WORKERS)
        .min(scan_indices.len())
        .max(1);

    thread::scope(|scope| {
        for _ in 0..workers {
            let ready = &ready;
            let next = &next;
            let remaining_bytes = &remaining_bytes;
            let scan_indices = &scan_indices;
            scope.spawn(move || {
                loop {
                    let scan_index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(candidate_index) = scan_indices.get(scan_index).copied() else {
                        break;
                    };
                    let path = Path::new(candidates[candidate_index].value.as_ref());
                    let is_ready =
                        auth_artifact::inspect_chatgpt_path_with_budget(path, remaining_bytes)
                            == auth_artifact::InspectionResult::Ready;
                    ready[candidate_index].store(is_ready, Ordering::Release);
                }
            });
        }
    });

    Ok(ready
        .into_iter()
        .map(|value| value.load(Ordering::Acquire))
        .collect())
}

fn effective_concurrency(requested: usize, use_proxy: bool, proxy_count: usize) -> usize {
    if use_proxy {
        requested.min(proxy_count)
    } else {
        requested
    }
}

fn collect_directory_files(
    root: &Path,
    visited_directories: &mut usize,
    discovery_limits: DiscoveryLimits,
) -> Result<Vec<PathBuf>, String> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(directory) = directories.pop() {
        *visited_directories = (*visited_directories).saturating_add(1);
        if *visited_directories > discovery_limits.max_directories {
            return Err(format!(
                "the limit is {} directories per validation",
                discovery_limits.max_directories
            ));
        }

        let reader = fs::read_dir(&directory)
            .map_err(|_| "unable to read one of the provided directories".to_string())?;
        let mut entries = Vec::new();
        for entry in reader {
            if entries.len() >= discovery_limits.max_directory_items() {
                return Err(format!(
                    "a directory can contain at most {} items",
                    discovery_limits.max_directory_items()
                ));
            }
            entries.push(
                entry.map_err(|_| "unable to list one of the provided directories".to_string())?,
            );
        }
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| "unable to read an item in the directory".to_string())?;
            if is_link_or_reparse(&metadata) {
                continue;
            }
            if is_platform_metadata(&path, metadata.is_dir()) {
                continue;
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file() {
                if files.len() >= discovery_limits.max_files {
                    return Err(format!(
                        "the limit is {} files per directory",
                        discovery_limits.max_files
                    ));
                }
                files.push(path);
            }
        }
    }

    files.sort();
    Ok(files)
}

fn is_platform_metadata(path: &Path, is_directory: bool) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if is_directory {
        name.eq_ignore_ascii_case("__MACOSX")
    } else {
        name.starts_with("._") || name.eq_ignore_ascii_case(".DS_Store")
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
    inputs: VecDeque<TaskInput>,
    concurrency: usize,
    delay_ms: u64,
    chatgpt_probe: Option<Arc<dyn ChatGptProber>>,
    cancellation: CancellationToken,
) {
    let module_id: Arc<str> = Arc::from(module_id);
    let queue = Arc::new(Mutex::new(inputs));
    let worker_count = {
        let total = lock_unpoison(&queue).len();
        concurrency.min(total)
    };
    let delay = Duration::from_millis(delay_ms);
    let (sender, receiver) = mpsc::sync_channel((worker_count * 2).max(1));
    let mut workers = Vec::with_capacity(worker_count);
    let mut fatal_worker_error = false;

    for worker_index in 0..worker_count {
        let queue = Arc::clone(&queue);
        let module_id = Arc::clone(&module_id);
        let chatgpt_probe = chatgpt_probe.clone();
        let sender = sender.clone();
        let handler = Arc::clone(&inner.handler);
        let cancellation = cancellation.clone();
        let result = thread::Builder::new()
            .name(format!("ayla-task-worker-{worker_index}"))
            .spawn(move || {
                worker_loop(
                    queue,
                    module_id,
                    chatgpt_probe,
                    sender,
                    handler,
                    cancellation,
                    delay,
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
    drop(sender);

    for outcome in receiver {
        inner.record(&run_id, outcome);
    }

    for worker in workers {
        if worker.join().is_err() {
            fatal_worker_error = true;
        }
    }

    inner.finish(&run_id, fatal_worker_error);
}

fn worker_loop(
    queue: Arc<Mutex<VecDeque<TaskInput>>>,
    module_id: Arc<str>,
    chatgpt_probe: Option<Arc<dyn ChatGptProber>>,
    sender: mpsc::SyncSender<WorkerMessage>,
    handler: Arc<dyn TaskHandler>,
    cancellation: CancellationToken,
    delay: Duration,
) {
    loop {
        if cancellation.is_cancelled() {
            break;
        }

        let input = lock_unpoison(&queue).pop_front();
        let Some(input) = input else {
            break;
        };

        if sender.send(WorkerMessage::Started).is_err() {
            break;
        }

        if cancellation.wait_cancelled(delay) {
            let _ = sender.send(WorkerMessage::Finished(HandlerOutcome::Skipped));
            break;
        }

        let context = TaskContext {
            cancellation: cancellation.clone(),
            chatgpt_probe: chatgpt_probe.clone(),
        };
        let outcome = handler.process(module_id.as_ref(), input, &context);
        if sender.send(WorkerMessage::Finished(outcome)).is_err() {
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
    use std::collections::HashSet;

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
        ) -> HandlerOutcome {
            if context.is_cancelled() {
                return HandlerOutcome::Skipped;
            }

            let active = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            self.peak.fetch_max(active, Ordering::AcqRel);
            if !self.pause.is_zero() {
                thread::sleep(self.pause);
            }

            lock_unpoison(&self.seen).push(input.into_inner().to_string());
            self.in_flight.fetch_sub(1, Ordering::AcqRel);

            if context.is_cancelled() {
                HandlerOutcome::Skipped
            } else {
                HandlerOutcome::Succeeded
            }
        }
    }

    #[derive(Default)]
    struct CountingProber {
        calls: AtomicUsize,
    }

    impl ChatGptProber for CountingProber {
        fn check(&self, _auth: &auth_artifact::PreparedChatGptAuth) -> ChatGptProbeResult {
            self.calls.fetch_add(1, Ordering::AcqRel);
            ChatGptProbeResult {
                status: ChatGptProbeStatus::Active(ChatGptPlan::Free),
                retries: 0,
            }
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
        }
    }

    fn test_engine(
        path: PathBuf,
        handler: Arc<dyn TaskHandler>,
        events: Arc<dyn TaskEventSink>,
    ) -> TaskEngine {
        TaskEngine::with_components(path, handler, events)
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
        assert_eq!(prepared.inputs.len(), 2);
        let order: Vec<_> = prepared
            .inputs
            .iter()
            .map(|input| input.0.as_ref())
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
    fn preparation_expands_directories_recursively() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("nested");
        fs::create_dir_all(&nested).expect("create nested directory");
        write_ready_artifact(&directory.path().join("one.txt"), 1);
        write_ready_artifact(&nested.join("two.json"), 2);

        let prepared = prepare(request(
            "chatgpt",
            [directory.path().display().to_string()],
            4,
            0,
        ))
        .expect("expand directory");

        assert_eq!(prepared.inputs.len(), 2);
        assert_eq!(prepared.discovered, 2);
        assert_eq!(prepared.locally_filtered, 0);
        let paths: Vec<_> = prepared
            .inputs
            .iter()
            .map(|input| input.0.as_ref())
            .collect();
        assert!(paths.iter().any(|path| path.ends_with("one.txt")));
        assert!(paths.iter().any(|path| path.ends_with("two.json")));
    }

    #[test]
    fn configurable_discovery_limits_bound_files_directories_and_scan_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("nested");
        fs::create_dir_all(&nested).expect("create nested directory");
        write_ready_artifact(&directory.path().join("one.txt"), 1);
        write_ready_artifact(&directory.path().join("two.txt"), 2);
        write_ready_artifact(&nested.join("three.txt"), 3);

        let file_error = prepare_with_limits(
            request("chatgpt", [directory.path().display().to_string()], 1, 0),
            DiscoveryLimits::new(10, 2, 8),
        )
        .err()
        .expect("file limit must be enforced");
        assert!(file_error.contains("2"));

        let directory_error = prepare_with_limits(
            request("chatgpt", [directory.path().display().to_string()], 1, 0),
            DiscoveryLimits::new(1, 10, 8),
        )
        .err()
        .expect("directory limit must be enforced");
        assert!(directory_error.contains("1 directories"));

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
        let budget_error = prepare_with_limits(
            request(
                "chatgpt",
                [budget_directory.path().display().to_string()],
                1,
                0,
            ),
            DiscoveryLimits::new(10, 10, 1),
        )
        .err()
        .expect("scan budget must be enforced");
        assert!(budget_error.contains("1 MiB"));
    }

    #[test]
    fn discovery_limit_constructor_clamps_to_absolute_safety_caps() {
        let limits = DiscoveryLimits::new(0, u32::MAX, u32::MAX);
        assert_eq!(limits.max_directories, 1);
        assert_eq!(limits.max_files, HARD_MAX_FILES);
        assert_eq!(
            limits.scan_budget_bytes,
            u64::from(HARD_MAX_SCAN_BUDGET_MIB) * 1024 * 1024
        );
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

        let prepared = prepare(request(
            "chatgpt",
            [directory.path().display().to_string()],
            4,
            0,
        ))
        .expect("expand directory without platform metadata");

        assert_eq!(prepared.inputs.len(), 6);
        assert_eq!(prepared.discovered, 6);
        assert_eq!(prepared.locally_filtered, 0);
        assert!(
            prepared
                .inputs
                .iter()
                .any(|input| input.0.ends_with("nested-cookie.txt"))
        );
        assert!(prepared.inputs.iter().all(|input| {
            !input.0.contains("__MACOSX")
                && !Path::new(input.0.as_ref())
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("._") || name == ".DS_Store")
        }));
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

        let prepared = prepare(request(
            "chatgpt",
            [directory.path().display().to_string()],
            4,
            0,
        ))
        .expect("preflight directory");
        assert_eq!(prepared.discovered, 3);
        assert_eq!(prepared.locally_filtered, 2);
        assert_eq!(prepared.inputs.len(), 1);
        assert!(prepared.inputs[0].0.ends_with("ready.txt"));

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
            let explicit = prepare(request("chatgpt", entries, 4, 0))
                .expect("explicit file must preserve the previous behavior");
            assert_eq!(explicit.discovered, 3);
            assert_eq!(explicit.locally_filtered, 1);
            assert_eq!(explicit.inputs.len(), 2);
            assert!(
                explicit
                    .inputs
                    .iter()
                    .any(|input| input.0.ends_with("notes.txt"))
            );
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

        assert_eq!(started.discovered, 3);
        assert_eq!(started.locally_filtered, 2);
        assert_eq!(started.total, 1);
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
                DiscoveryLimits::new(10, 10, 1),
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

        let prepared = prepare_with_limits(
            request("chatgpt", [directory.path().display().to_string()], 4, 0),
            DiscoveryLimits::new(10, 10, 1),
        )
        .expect("discovery should succeed despite the oversized file");
        assert_eq!(prepared.discovered, 2);
        assert_eq!(prepared.inputs.len(), 1);
        assert!(prepared.inputs[0].0.ends_with("cookies.txt"));
    }

    #[test]
    #[ignore = "requires AYLA_AUTH_EXAMPLES and explicit local authorization"]
    fn discovers_authorized_external_examples_without_remote_calls() {
        let Some(root) = std::env::var_os("AYLA_AUTH_EXAMPLES") else {
            return;
        };
        let prepared = prepare(request(
            "chatgpt",
            [PathBuf::from(root).display().to_string()],
            4,
            0,
        ))
        .expect("discover authorized examples");

        assert!(!prepared.inputs.is_empty());
        assert_eq!(
            prepared.discovered,
            prepared.inputs.len() + prepared.locally_filtered
        );
        println!(
            "authorized aggregate only: discovered={}, structurally_ready={}, locally_filtered={}",
            prepared.discovered,
            prepared.inputs.len(),
            prepared.locally_filtered
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
            events,
        );

        let mut entries: Vec<String> = (0..40).map(|index| format!("value-{index}")).collect();
        entries.extend(["value-1".to_string(), " value-2 ".to_string()]);
        let started = engine
            .start(request("chatgpt", entries, 4, 0))
            .expect("start task");
        assert_eq!(started.total, 40);

        let second = engine.start(request("chatgpt", ["blocked".to_string()], 1, 0));
        assert!(second.is_err(), "only one global run may be active");

        let summary = wait_for_history(&engine, &started.run_id);
        assert_eq!(summary.status, TaskStatus::Completed);
        assert_eq!(summary.succeeded, 40);
        assert_eq!(summary.skipped, 0);
        assert!(handler.peak.load(Ordering::Acquire) > 1);
        assert!(handler.peak.load(Ordering::Acquire) <= 4);

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

        thread::sleep(Duration::from_millis(25));
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
