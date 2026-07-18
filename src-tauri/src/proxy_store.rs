use crate::proxy::{ParsedProxy, RejectedProxy, normalized_protocol, parse_entries};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

static PROXY_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PROXY_STORE_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyItem {
    pub id: String,
    pub display: String,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub has_auth: bool,
    pub status: String,
    pub latency_ms: u64,
    pub message: String,
    pub ip: String,
    pub country: String,
    pub country_code: String,
    pub city: String,
    pub created_at: String,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddProxiesResult {
    pub added: usize,
    pub duplicates: usize,
    pub rejected: Vec<RejectedProxy>,
    pub total: usize,
    pub items: Vec<ProxyItem>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct StoredProxy {
    pub(crate) id: String,
    pub(crate) protocol: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) status: String,
    pub(crate) latency_ms: u64,
    pub(crate) message: String,
    pub(crate) ip: String,
    pub(crate) country: String,
    pub(crate) country_code: String,
    pub(crate) city: String,
    pub(crate) created_at: String,
    pub(crate) checked_at: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CheckUpdate {
    pub(crate) live: bool,
    pub(crate) latency_ms: u64,
    pub(crate) message: String,
    pub(crate) ip: String,
    pub(crate) country: String,
    pub(crate) country_code: String,
    pub(crate) city: String,
}

#[derive(Clone)]
struct ActiveCheck {
    run_id: String,
    cancellation: Arc<AtomicBool>,
}

#[derive(Default)]
struct ManagerState {
    items: Vec<StoredProxy>,
    positions: HashMap<String, usize>,
    active: Option<ActiveCheck>,
}

#[derive(Clone)]
pub(crate) struct CheckLease {
    pub(crate) run_id: String,
    pub(crate) cancellation: Arc<AtomicBool>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ProxyStoreFile {
    version: u8,
    items: Vec<StoredProxy>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyStoreRef<'a> {
    version: u8,
    items: &'a [StoredProxy],
}

pub struct ProxyManager {
    path: PathBuf,
    state: Mutex<ManagerState>,
}

impl ProxyManager {
    pub fn open(path: PathBuf) -> Self {
        let items = load(&path);
        let positions = proxy_positions(&items);
        Self {
            path,
            state: Mutex::new(ManagerState {
                items,
                positions,
                active: None,
            }),
        }
    }

    pub fn snapshot(&self) -> Result<Vec<ProxyItem>, String> {
        let state = self.lock()?;
        Ok(sorted_items(&state.items))
    }

    pub fn counts(&self) -> Result<(usize, usize), String> {
        let state = self.lock()?;
        let live = state
            .items
            .iter()
            .filter(|item| item.status == "live")
            .count();
        Ok((state.items.len(), live))
    }

    pub fn add(&self, raw: &str, protocol: &str) -> Result<AddProxiesResult, String> {
        let parsed = parse_entries(raw, protocol);
        let mut state = self.lock()?;
        let mut next = state.items.clone();
        let mut existing: HashSet<String> = next.iter().map(StoredProxy::key).collect();
        let mut duplicates = parsed.duplicates;
        let mut added = 0;
        let now = now_timestamp();

        for entry in parsed.entries {
            if !existing.insert(entry.key()) {
                duplicates += 1;
                continue;
            }
            next.push(StoredProxy::from_parsed(entry, &now));
            added += 1;
        }

        persist(&self.path, &next)?;
        state.items = next;
        state.positions = proxy_positions(&state.items);
        let items = sorted_items(&state.items);

        Ok(AddProxiesResult {
            added,
            duplicates,
            rejected: parsed.rejected,
            total: items.len(),
            items,
        })
    }

    pub fn remove(&self, ids: &[String]) -> Result<Vec<ProxyItem>, String> {
        let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let mut state = self.lock()?;
        let next: Vec<_> = state
            .items
            .iter()
            .filter(|item| !ids.contains(item.id.as_str()))
            .cloned()
            .collect();
        persist(&self.path, &next)?;
        state.items = next;
        state.positions = proxy_positions(&state.items);
        Ok(sorted_items(&state.items))
    }

    pub fn clear(&self) -> Result<Vec<ProxyItem>, String> {
        let mut state = self.lock()?;
        if let Some(active) = &state.active {
            active.cancellation.store(true, Ordering::Release);
        }
        persist(&self.path, &[])?;
        state.items.clear();
        state.positions.clear();
        Ok(Vec::new())
    }

    pub fn is_checking(&self) -> Result<bool, String> {
        Ok(self.lock()?.active.is_some())
    }

    pub fn stop_check(&self) -> Result<bool, String> {
        let state = self.lock()?;
        if let Some(active) = &state.active {
            active.cancellation.store(true, Ordering::Release);
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn begin_check(&self) -> Result<CheckLease, String> {
        let mut state = self.lock()?;
        if state.active.is_some() {
            return Err("a proxy check is already in progress".to_string());
        }

        let lease = CheckLease {
            run_id: new_proxy_id("run"),
            cancellation: Arc::new(AtomicBool::new(false)),
        };
        state.active = Some(ActiveCheck {
            run_id: lease.run_id.clone(),
            cancellation: lease.cancellation.clone(),
        });
        Ok(lease)
    }

    pub(crate) fn finish_check(&self, run_id: &str) {
        if let Ok(mut state) = self.state.lock()
            && state
                .active
                .as_ref()
                .is_some_and(|active| active.run_id == run_id)
        {
            state.active = None;
        }
    }
    pub(crate) fn live_targets(&self) -> Result<Vec<StoredProxy>, String> {
        let state = self.lock()?;
        Ok(state
            .items
            .iter()
            .filter(|item| item.status == "live")
            .cloned()
            .collect())
    }

    pub(crate) fn targets(&self, ids: &[String]) -> Result<Vec<StoredProxy>, String> {
        let state = self.lock()?;
        if ids.is_empty() {
            return Ok(state.items.clone());
        }
        let mut seen = HashSet::with_capacity(ids.len());
        Ok(ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .filter_map(|id| {
                let position = state.positions.get(id.as_str())?;
                state.items.get(*position).cloned()
            })
            .collect())
    }

    pub(crate) fn apply_check(
        &self,
        id: &str,
        update: CheckUpdate,
    ) -> Result<Option<ProxyItem>, String> {
        let mut state = self.lock()?;
        let Some(&position) = state.positions.get(id) else {
            return Ok(None);
        };

        if update.live {
            let item = &mut state.items[position];
            item.status = "live".to_string();
            item.latency_ms = update.latency_ms;
            item.message = update.message;
            item.ip = update.ip;
            item.country = update.country;
            item.country_code = update.country_code;
            item.city = update.city;
            item.checked_at = now_timestamp();
            Ok(Some(item.item()))
        } else {
            let removed = state.items.swap_remove(position);
            state.positions.remove(&removed.id);
            if position < state.items.len() {
                let moved_id = state.items[position].id.clone();
                state.positions.insert(moved_id, position);
            }
            Ok(None)
        }
    }

    pub(crate) fn commit_check(&self) -> Result<(), String> {
        let state = self.lock()?;
        persist(&self.path, &state.items)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ManagerState>, String> {
        self.state
            .lock()
            .map_err(|_| "unable to access the proxy list".to_string())
    }
}

impl StoredProxy {
    fn from_parsed(parsed: ParsedProxy, now: &str) -> Self {
        Self {
            id: new_proxy_id("px"),
            protocol: parsed.protocol,
            host: parsed.host,
            port: parsed.port,
            username: parsed.username,
            password: parsed.password,
            status: "pending".to_string(),
            created_at: now.to_string(),
            ..Self::default()
        }
    }

    pub(crate) fn key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.protocol,
            self.host.to_ascii_lowercase(),
            self.port,
            self.username.as_deref().unwrap_or_default(),
            self.password.as_deref().unwrap_or_default()
        )
    }

    pub(crate) fn item(&self) -> ProxyItem {
        ProxyItem {
            id: self.id.clone(),
            display: self.display(),
            protocol: self.protocol.clone(),
            host: self.host.clone(),
            port: self.port,
            has_auth: self.username.is_some(),
            status: self.status.clone(),
            latency_ms: self.latency_ms,
            message: self.message.clone(),
            ip: self.ip.clone(),
            country: self.country.clone(),
            country_code: self.country_code.clone(),
            city: self.city.clone(),
            created_at: self.created_at.clone(),
            checked_at: self.checked_at.clone(),
        }
    }

    fn display(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{}://{}:{}", self.protocol, host, self.port)
    }
}

fn sorted_items(items: &[StoredProxy]) -> Vec<ProxyItem> {
    let mut items: Vec<_> = items.iter().map(StoredProxy::item).collect();
    items.sort_by(
        |left, right| match (left.status.as_str(), right.status.as_str()) {
            ("live", "live") => left.latency_ms.cmp(&right.latency_ms),
            ("live", _) => std::cmp::Ordering::Less,
            (_, "live") => std::cmp::Ordering::Greater,
            _ => left.created_at.cmp(&right.created_at),
        },
    );
    items
}

fn load(path: &Path) -> Vec<StoredProxy> {
    let candidates = [
        path.to_path_buf(),
        sidecar_path(path, "tmp"),
        sidecar_path(path, "bak"),
    ];
    let Some(file) = candidates.iter().find_map(|candidate| {
        let data = fs::read(candidate).ok()?;
        serde_json::from_slice::<ProxyStoreFile>(&data).ok()
    }) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    file.items
        .into_iter()
        .filter_map(|mut item| {
            if item.host.trim().is_empty() || item.port == 0 {
                return None;
            }
            item.protocol = normalized_protocol(&item.protocol).to_string();
            if !matches!(item.status.as_str(), "pending" | "live") {
                item.status = "pending".to_string();
            }
            if item.id.is_empty() {
                item.id = new_proxy_id("px");
            }
            seen.insert(item.key()).then_some(item)
        })
        .collect()
}

fn proxy_positions(items: &[StoredProxy]) -> HashMap<String, usize> {
    items
        .iter()
        .enumerate()
        .map(|(position, item)| (item.id.clone(), position))
        .collect()
}

fn persist(path: &Path, items: &[StoredProxy]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("unable to create the proxy directory: {error}"))?;
    }
    let data = serde_json::to_vec_pretty(&ProxyStoreRef {
        version: PROXY_STORE_VERSION,
        items,
    })
    .map_err(|error| format!("unable to serialize proxies: {error}"))?;
    let temporary = sidecar_path(path, "tmp");
    let backup = sidecar_path(path, "bak");

    let mut file = fs::File::create(&temporary)
        .map_err(|error| format!("unable to create the temporary proxy store: {error}"))?;
    file.write_all(&data)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("unable to write the temporary proxy store: {error}"))?;
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
                format!("unable to prepare the proxy store replacement: {error}")
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
                        "unable to replace the proxy store: {error}; initial attempt: {first_error}"
                    ))
                }
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(format!("unable to publish the proxy store: {error}"))
        }
    }
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("proxies.json");
    path.with_file_name(format!("{file_name}.{suffix}"))
}

fn new_proxy_id(prefix: &str) -> String {
    let sequence = PROXY_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{timestamp:x}_{sequence:x}")
}

fn now_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_deduplicates_persists_and_hides_credentials_from_api() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("proxies.json");
        let manager = ProxyManager::open(path.clone());

        let result = manager
            .add(
                "user:secret@1.2.3.4:8080\n1.2.3.4:8080:user:secret\n5.6.7.8:3128",
                "http",
            )
            .expect("import proxies");

        assert_eq!(result.added, 2);
        assert_eq!(result.duplicates, 1);
        assert_eq!(result.total, 2);
        let api_json = serde_json::to_string(&result.items).expect("serialize API items");
        assert!(!api_json.contains("secret"));
        assert!(result.items[0].has_auth || result.items[1].has_auth);

        let reopened = ProxyManager::open(path);
        assert_eq!(reopened.snapshot().expect("snapshot").len(), 2);
    }

    #[test]
    fn remove_and_clear_are_persisted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("proxies.json");
        let manager = ProxyManager::open(path.clone());
        let added = manager
            .add("1.1.1.1:80\n2.2.2.2:81", "http")
            .expect("import proxies");

        manager
            .remove(&[added.items[0].id.clone()])
            .expect("remove proxy");
        assert_eq!(manager.snapshot().expect("snapshot").len(), 1);

        manager.clear().expect("clear proxies");
        assert!(
            ProxyManager::open(path)
                .snapshot()
                .expect("reopen snapshot")
                .is_empty()
        );
    }

    #[test]
    fn check_updates_use_index_and_persist_once_at_commit() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("proxies.json");
        let manager = ProxyManager::open(path.clone());
        let added = manager
            .add("1.1.1.1:80\n2.2.2.2:81\n3.3.3.3:82", "http")
            .expect("import proxies");
        let live_id = added.items[0].id.clone();
        let dead_id = added.items[1].id.clone();

        manager
            .apply_check(
                &live_id,
                CheckUpdate {
                    live: true,
                    latency_ms: 42,
                    message: "ok".to_string(),
                    ip: "203.0.113.10".to_string(),
                    country: "Exampleland".to_string(),
                    country_code: "EX".to_string(),
                    city: "Example City".to_string(),
                },
            )
            .expect("mark live");
        manager
            .apply_check(
                &dead_id,
                CheckUpdate {
                    live: false,
                    latency_ms: 0,
                    message: "dead".to_string(),
                    ip: String::new(),
                    ..CheckUpdate::default()
                },
            )
            .expect("remove dead");

        assert_eq!(
            ProxyManager::open(path.clone()).snapshot().unwrap().len(),
            3
        );
        manager.commit_check().expect("commit check");

        let reopened = ProxyManager::open(path);
        let items = reopened.snapshot().expect("reopen snapshot");
        assert_eq!(items.len(), 2);
        let live = items
            .iter()
            .find(|item| item.id == live_id)
            .expect("live item");
        assert_eq!(live.status, "live");
        assert_eq!(live.latency_ms, 42);
        assert_eq!(live.country_code, "EX");
        assert_eq!(live.city, "Example City");
        assert!(!items.iter().any(|item| item.id == dead_id));
    }

    #[test]
    fn load_recovers_from_a_valid_sidecar_when_the_main_file_is_corrupt() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("proxies.json");
        let manager = ProxyManager::open(path.clone());
        manager
            .add("1.1.1.1:80", "http")
            .expect("persist initial proxy");

        fs::copy(&path, sidecar_path(&path, "bak")).expect("create recovery copy");
        fs::write(&path, b"{truncated").expect("corrupt main store");

        let recovered = ProxyManager::open(path)
            .snapshot()
            .expect("recovered snapshot");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].host, "1.1.1.1");
    }
    #[test]
    fn only_one_check_can_run_and_stop_is_idempotent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manager = ProxyManager::open(directory.path().join("proxies.json"));
        let lease = manager.begin_check().expect("begin check");
        assert!(manager.begin_check().is_err());
        assert!(manager.is_checking().expect("checking"));
        assert!(manager.stop_check().expect("stop"));
        assert!(lease.cancellation.load(Ordering::Acquire));
        manager.finish_check(&lease.run_id);
        assert!(!manager.is_checking().expect("not checking"));
    }
}
