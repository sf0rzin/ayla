mod auth_artifact;
mod catalog;
mod chatgpt_client;
mod cookie_artifact;
mod module_probe;
mod proxy;
mod proxy_checker;
mod proxy_store;
mod settings;
mod task_engine;
mod twitch_client;

use chatgpt_client::ChatGptProbePool;
use proxy_checker::{CheckProxiesRequest, CheckProxiesResponse};
use proxy_store::{AddProxiesResult, ProxyItem, ProxyManager};
use serde::Serialize;
use settings::{AppSettings, SettingsStore};
use std::sync::{Arc, Mutex};
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
fn list_modules() -> Vec<catalog::ModuleInfo> {
    catalog::modules()
}

#[tauri::command]
fn get_settings(state: State<'_, SettingsStore>) -> Result<AppSettings, String> {
    state.snapshot()
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
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let settings_path = config_dir.join("settings.json");
            let proxies_path = config_dir.join("proxies.json");
            let task_history_path = config_dir.join("task_history.json");
            app.manage(SettingsStore::open(settings_path));
            app.manage(ProxyManager::open(proxies_path));
            app.manage(TaskEngine::open(task_history_path, app.handle().clone()));
            app.manage(SystemMetricsStore::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_overview,
            get_system_metrics,
            list_modules,
            get_settings,
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
