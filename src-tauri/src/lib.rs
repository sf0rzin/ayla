mod auth_artifact;
mod catalog;
mod chatgpt_client;
mod cookie_artifact;
mod max_client;
mod module_probe;
mod proxy;
mod proxy_checker;
mod proxy_store;
mod settings;
mod task_engine;
mod twitch_client;

use chatgpt_client::ChatGptProbePool;
use max_client::MaxProbePool;
use proxy_checker::{CheckProxiesRequest, CheckProxiesResponse};
use proxy_store::{AddProxiesResult, ProxyItem, ProxyManager};
use serde::Serialize;
use settings::{AppSettings, SettingsStore};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError},
};
use sysinfo::System;
use task_engine::{DiscoveryLimits, StartTaskRequest, TaskEngine, TaskHistoryEntry, TaskSnapshot};
use tauri::{AppHandle, Manager, State};
use twitch_client::TwitchProbePool;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppOverview {
    version: &'static str,
    modules_total: usize,
    default_threads: u16,
    proxies_total: usize,
    proxies_live: usize,
    storage_path: String,
}

struct SystemMetricsStore(Mutex<System>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DevelopmentLoginSession {
    token: &'static str,
    expires_at: &'static str,
    user: DevelopmentLoginUser,
}

#[derive(Serialize)]
struct DevelopmentLoginUser {
    id: &'static str,
    name: &'static str,
    email: &'static str,
    role: &'static str,
}

#[cfg(debug_assertions)]
const DEVELOPMENT_LOGIN_BYPASS_ARGUMENT: &str = "--skip-login";

#[cfg(debug_assertions)]
fn development_login_bypass_requested_from(arguments: impl IntoIterator<Item = OsString>) -> bool {
    arguments
        .into_iter()
        .any(|argument| argument == std::ffi::OsStr::new(DEVELOPMENT_LOGIN_BYPASS_ARGUMENT))
}

#[cfg(not(debug_assertions))]
fn development_login_bypass_requested_from(_arguments: impl IntoIterator<Item = OsString>) -> bool {
    false
}

fn development_login_bypass_session_from(
    arguments: impl IntoIterator<Item = OsString>,
) -> Option<DevelopmentLoginSession> {
    if !development_login_bypass_requested_from(arguments) {
        return None;
    }

    #[cfg(debug_assertions)]
    {
        Some(DevelopmentLoginSession {
            token: "development-local-test-session",
            expires_at: "9999-12-31T23:59:59.999Z",
            user: DevelopmentLoginUser {
                id: "development-local-test",
                name: "Local Tester",
                email: "test@local.invalid",
                role: "user",
            },
        })
    }

    #[cfg(not(debug_assertions))]
    {
        None
    }
}

#[derive(Default)]
struct UpdateInstallGate(RwLock<bool>);

impl UpdateInstallGate {
    fn begin_background_operation(&self) -> Result<RwLockReadGuard<'_, bool>, String> {
        let guard = self
            .0
            .read()
            .map_err(|_| "the update safety gate is unavailable".to_string())?;
        if *guard {
            return Err(
                "an application update is being installed; restart Ayla to continue".to_string(),
            );
        }
        Ok(guard)
    }

    fn try_lock_for_install(&self) -> Result<Option<RwLockWriteGuard<'_, bool>>, String> {
        match self.0.try_write() {
            Ok(guard) => Ok(Some(guard)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Poisoned(_)) => {
                Err("the update safety gate is unavailable".to_string())
            }
        }
    }

    fn release(&self) -> Result<(), String> {
        let mut armed = self
            .0
            .write()
            .map_err(|_| "the update safety gate is unavailable".to_string())?;
        *armed = false;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInstallReadiness {
    can_install: bool,
    task_running: bool,
    proxy_check_running: bool,
    blocked_reason: Option<&'static str>,
}

fn evaluate_update_install_readiness(
    task_running: bool,
    proxy_check_running: bool,
) -> UpdateInstallReadiness {
    let blocked_reason = match (task_running, proxy_check_running) {
        (true, true) => Some("Wait for the active task and proxy check to finish."),
        (true, false) => Some("Wait for the active task to finish."),
        (false, true) => Some("Wait for the proxy check to finish."),
        (false, false) => None,
    };

    UpdateInstallReadiness {
        can_install: blocked_reason.is_none(),
        task_running,
        proxy_check_running,
        blocked_reason,
    }
}

fn background_operation_is_starting() -> UpdateInstallReadiness {
    UpdateInstallReadiness {
        can_install: false,
        task_running: false,
        proxy_check_running: false,
        blocked_reason: Some("A background operation is starting. Try again in a moment."),
    }
}

impl SystemMetricsStore {
    fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_memory();
        Self(Mutex::new(system))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemMetrics {
    cpu_percent: f32,
    cpu_count: usize,
    memory_used_bytes: u64,
    memory_total_bytes: u64,
}

#[tauri::command]
fn get_system_metrics(state: State<'_, SystemMetricsStore>) -> Result<SystemMetrics, String> {
    let mut system = state
        .0
        .lock()
        .map_err(|_| "system metrics are temporarily unavailable".to_string())?;
    system.refresh_cpu_usage();
    system.refresh_memory();

    Ok(SystemMetrics {
        cpu_percent: system.global_cpu_usage(),
        cpu_count: system.cpus().len(),
        memory_used_bytes: system.used_memory(),
        memory_total_bytes: system.total_memory(),
    })
}

#[tauri::command]
fn get_app_overview(
    settings: State<'_, SettingsStore>,
    proxies: State<'_, ProxyManager>,
) -> Result<AppOverview, String> {
    let current_settings = settings.snapshot()?;
    let (proxies_total, proxies_live) = proxies.counts()?;

    Ok(AppOverview {
        version: env!("CARGO_PKG_VERSION"),
        modules_total: catalog::MODULE_IDS.len(),
        default_threads: current_settings.threads,
        proxies_total,
        proxies_live,
        storage_path: settings.path().display().to_string(),
    })
}

#[tauri::command]
fn prepare_update_install(
    gate: State<'_, UpdateInstallGate>,
    tasks: State<'_, TaskEngine>,
    proxies: State<'_, ProxyManager>,
) -> Result<UpdateInstallReadiness, String> {
    let Some(mut armed) = gate.try_lock_for_install()? else {
        return Ok(background_operation_is_starting());
    };
    if *armed {
        return Ok(evaluate_update_install_readiness(false, false));
    }

    let readiness =
        evaluate_update_install_readiness(!tasks.list_active().is_empty(), proxies.is_checking()?);
    if readiness.can_install {
        *armed = true;
    }
    Ok(readiness)
}

#[tauri::command]
fn release_update_install_gate(gate: State<'_, UpdateInstallGate>) -> Result<(), String> {
    gate.release()
}

#[tauri::command]
fn list_modules() -> Vec<catalog::ModuleInfo> {
    catalog::modules()
}

#[tauri::command]
fn get_settings(state: State<'_, SettingsStore>) -> Result<AppSettings, String> {
    state.snapshot()
}

#[tauri::command]
fn development_login_bypass_session() -> Option<DevelopmentLoginSession> {
    development_login_bypass_session_from(std::env::args_os())
}

#[tauri::command]
fn get_settings_recovery_notice(state: State<'_, SettingsStore>) -> Result<Option<String>, String> {
    state.recovery_notice()
}

#[tauri::command]
fn save_settings(
    state: State<'_, SettingsStore>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    state.replace(settings)
}

#[tauri::command]
fn parse_proxy_input(raw: &str, default_protocol: &str) -> proxy::ProxyParseReport {
    proxy::parse_batch(raw, default_protocol)
}

#[tauri::command]
fn list_proxies(state: State<'_, ProxyManager>) -> Result<Vec<ProxyItem>, String> {
    state.snapshot()
}

#[tauri::command]
fn add_proxies(
    state: State<'_, ProxyManager>,
    raw: &str,
    protocol: &str,
) -> Result<AddProxiesResult, String> {
    state.add(raw, protocol)
}

#[tauri::command]
fn remove_proxies(
    state: State<'_, ProxyManager>,
    ids: Vec<String>,
) -> Result<Vec<ProxyItem>, String> {
    state.remove(&ids)
}

#[tauri::command]
fn clear_proxies(state: State<'_, ProxyManager>) -> Result<Vec<ProxyItem>, String> {
    state.clear()
}

#[tauri::command]
fn is_proxy_check_running(state: State<'_, ProxyManager>) -> Result<bool, String> {
    state.is_checking()
}

#[tauri::command]
fn stop_proxy_check(state: State<'_, ProxyManager>) -> Result<bool, String> {
    state.stop_check()
}

#[tauri::command]
async fn check_proxies(
    app: AppHandle,
    request: CheckProxiesRequest,
) -> Result<CheckProxiesResponse, String> {
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let gate = worker_app.state::<UpdateInstallGate>();
        let _operation = gate.begin_background_operation()?;
        let manager = worker_app.state::<ProxyManager>();
        proxy_checker::run_check(&worker_app, &manager, request)
    })
    .await
    .map_err(|error| format!("the proxy check was interrupted: {error}"))?
}

#[tauri::command]
async fn start_task(app: AppHandle, request: StartTaskRequest) -> Result<TaskSnapshot, String> {
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let gate = worker_app.state::<UpdateInstallGate>();
        let _operation = gate.begin_background_operation()?;
        let settings = worker_app.state::<SettingsStore>();
        let proxies = worker_app.state::<ProxyManager>();
        let engine = worker_app.state::<TaskEngine>();
        let current = settings.snapshot()?;
        let proxy_targets = if request.use_proxy {
            let live = proxies.live_targets()?;
            if live.is_empty() {
                return Err("no active proxy is available".to_string());
            }
            live
        } else {
            Vec::new()
        };
        let discovery_limits = DiscoveryLimits::new(
            current.max_scan_directories,
            current.max_scan_files,
            current.scan_budget_mib,
        );
        let module_id = request.module_id.trim().to_ascii_lowercase();
        match module_id.as_str() {
            "chatgpt" => {
                let probe =
                    ChatGptProbePool::new(current.timeout_ms, current.retries, &proxy_targets)?;
                let proxy_count = probe.proxy_count();
                engine.start_with_chatgpt_probe(
                    request,
                    Arc::new(probe),
                    proxy_count,
                    discovery_limits,
                )
            }
            "twitch" => {
                let probe = TwitchProbePool::new(
                    current.timeout_ms,
                    current.retries,
                    request.delay_ms,
                    &proxy_targets,
                )?;
                let proxy_count = probe.proxy_count();
                engine.start_with_cookie_probe(
                    request,
                    Arc::new(probe),
                    proxy_count,
                    discovery_limits,
                )
            }
            "max" => {
                let probe = MaxProbePool::new(
                    current.timeout_ms,
                    current.retries,
                    request.delay_ms,
                    &proxy_targets,
                )?;
                let proxy_count = probe.proxy_count();
                engine.start_with_cookie_probe(
                    request,
                    Arc::new(probe),
                    proxy_count,
                    discovery_limits,
                )
            }
            _ => Err("module has not been migrated yet".to_string()),
        }
    })
    .await
    .map_err(|error| format!("the task preparation was interrupted: {error}"))?
}

#[tauri::command]
fn list_tasks(state: State<'_, TaskEngine>) -> Vec<TaskSnapshot> {
    state.list_active()
}

#[tauri::command]
fn get_task(state: State<'_, TaskEngine>, run_id: String) -> Option<TaskSnapshot> {
    state.get_active(&run_id)
}

#[tauri::command]
fn cancel_task(state: State<'_, TaskEngine>, run_id: String) -> Option<TaskSnapshot> {
    state.cancel(&run_id)
}

#[tauri::command]
fn task_history(state: State<'_, TaskEngine>, limit: Option<usize>) -> Vec<TaskHistoryEntry> {
    state.history(limit)
}

#[tauri::command]
fn clear_task_history(state: State<'_, TaskEngine>) -> Result<(), String> {
    state.clear_history()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let settings_path = config_dir.join("settings.json");
            let proxies_path = config_dir.join("proxies.json");
            let task_history_path = config_dir.join("task_history.json");
            app.manage(SettingsStore::open(settings_path));
            app.manage(ProxyManager::open(proxies_path));
            app.manage(TaskEngine::open(task_history_path, app.handle().clone()));
            app.manage(SystemMetricsStore::new());
            app.manage(UpdateInstallGate::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            development_login_bypass_session,
            get_app_overview,
            prepare_update_install,
            release_update_install_gate,
            get_system_metrics,
            list_modules,
            get_settings,
            get_settings_recovery_notice,
            save_settings,
            parse_proxy_input,
            list_proxies,
            add_proxies,
            remove_proxies,
            clear_proxies,
            is_proxy_check_running,
            stop_proxy_check,
            check_proxies,
            start_task,
            list_tasks,
            get_task,
            cancel_task,
            task_history,
            clear_task_history
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Ayla");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_bypass_requires_the_exact_argument_and_a_debug_build() {
        let session = development_login_bypass_session_from([
            OsString::from("ayla.exe"),
            OsString::from("--skip-login"),
        ]);
        assert_eq!(session.is_some(), cfg!(debug_assertions));

        assert!(
            development_login_bypass_session_from([
                OsString::from("ayla.exe"),
                OsString::from("--skip-login=true"),
            ])
            .is_none()
        );
        assert!(
            development_login_bypass_session_from([
                OsString::from("ayla.exe"),
                OsString::from("--skip-auth"),
            ])
            .is_none()
        );
    }

    #[test]
    fn update_install_is_ready_only_when_background_work_is_idle() {
        let ready = evaluate_update_install_readiness(false, false);
        assert!(ready.can_install);
        assert_eq!(ready.blocked_reason, None);

        let task_blocked = evaluate_update_install_readiness(true, false);
        assert!(!task_blocked.can_install);
        assert_eq!(
            task_blocked.blocked_reason,
            Some("Wait for the active task to finish.")
        );

        let proxy_blocked = evaluate_update_install_readiness(false, true);
        assert!(!proxy_blocked.can_install);
        assert_eq!(
            proxy_blocked.blocked_reason,
            Some("Wait for the proxy check to finish.")
        );
    }

    #[test]
    fn update_gate_excludes_new_background_operations_until_released() {
        let gate = UpdateInstallGate::default();
        let operation = gate
            .begin_background_operation()
            .expect("background operation starts while idle");
        assert!(
            gate.try_lock_for_install()
                .expect("gate remains healthy")
                .is_none()
        );
        drop(operation);

        let mut install = gate
            .try_lock_for_install()
            .expect("gate remains healthy")
            .expect("install obtains the exclusive startup lock");
        *install = true;
        drop(install);
        assert!(gate.begin_background_operation().is_err());

        gate.release().expect("release install gate");
        assert!(gate.begin_background_operation().is_ok());
    }
}
