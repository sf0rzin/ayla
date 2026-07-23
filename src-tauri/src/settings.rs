use crate::catalog;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::RwLock,
};

const DEFAULT_THREADS: u16 = 24;
const MAX_THREADS: u16 = 200;
const DEFAULT_DELAY_MS: u64 = 120;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_RETRIES: u8 = 1;
const DEFAULT_MAX_SCAN_DIRECTORIES: u32 = 1_000;
const DEFAULT_MAX_SCAN_FILES: u32 = 10_000;
const DEFAULT_SCAN_BUDGET_MIB: u32 = 512;
const MAX_DELAY_MS: u64 = 60_000;
const MIN_TIMEOUT_MS: u64 = 3_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_SCAN_DIRECTORIES: u32 = 10_000;
const MAX_SCAN_FILES: u32 = 100_000;
const MAX_SCAN_BUDGET_MIB: u32 = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub threads: u16,
    pub module_threads: BTreeMap<String, u16>,
    pub delay_ms: u64,
    pub timeout_ms: u64,
    pub retries: u8,
    pub max_scan_directories: u32,
    pub max_scan_files: u32,
    pub scan_budget_mib: u32,
    pub auto_check_updates: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            threads: DEFAULT_THREADS,
            module_threads: BTreeMap::new(),
            delay_ms: DEFAULT_DELAY_MS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            retries: DEFAULT_RETRIES,
            max_scan_directories: DEFAULT_MAX_SCAN_DIRECTORIES,
            max_scan_files: DEFAULT_MAX_SCAN_FILES,
            scan_budget_mib: DEFAULT_SCAN_BUDGET_MIB,
            auto_check_updates: true,
        }
    }
}

impl AppSettings {
    fn normalized(mut self) -> Self {
        if self.threads == 0 {
            self.threads = DEFAULT_THREADS;
        }
        self.threads = self.threads.min(MAX_THREADS);
        self.delay_ms = self.delay_ms.min(MAX_DELAY_MS);
        self.timeout_ms = if self.timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            self.timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
        };
        self.retries = self.retries.min(5);
        self.max_scan_directories = if self.max_scan_directories == 0 {
            DEFAULT_MAX_SCAN_DIRECTORIES
        } else {
            self.max_scan_directories.min(MAX_SCAN_DIRECTORIES)
        };
        self.max_scan_files = if self.max_scan_files == 0 {
            DEFAULT_MAX_SCAN_FILES
        } else {
            self.max_scan_files.min(MAX_SCAN_FILES)
        };
        self.scan_budget_mib = if self.scan_budget_mib == 0 {
            DEFAULT_SCAN_BUDGET_MIB
        } else {
            self.scan_budget_mib.min(MAX_SCAN_BUDGET_MIB)
        };

        self.module_threads = self
            .module_threads
            .into_iter()
            .filter_map(|(id, threads)| {
                let id = id.trim().to_ascii_lowercase();
                if !catalog::is_known_module(&id) || threads == 0 {
                    return None;
                }
                Some((id, threads.min(MAX_THREADS)))
            })
            .collect();

        self
    }
}

pub struct SettingsStore {
    path: PathBuf,
    current: RwLock<AppSettings>,
}

impl SettingsStore {
    pub fn open(path: PathBuf) -> Self {
        let current = load(&path).unwrap_or_default().normalized();
        Self {
            path,
            current: RwLock::new(current),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn snapshot(&self) -> Result<AppSettings, String> {
        self.current
            .read()
            .map(|settings| settings.clone())
            .map_err(|_| "unable to access settings".to_string())
    }

    pub fn replace(&self, settings: AppSettings) -> Result<AppSettings, String> {
        let settings = settings.normalized();
        persist(&self.path, &settings)?;

        let mut current = self
            .current
            .write()
            .map_err(|_| "unable to update settings".to_string())?;
        *current = settings.clone();
        Ok(settings)
    }
}

fn load(path: &Path) -> Option<AppSettings> {
    let data = fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn persist(path: &Path, settings: &AppSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("unable to create the data directory: {error}"))?;
    }

    let data = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("unable to serialize settings: {error}"))?;
    fs::write(path, data).map_err(|error| format!("unable to save settings: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_matches_application_limits() {
        let settings = AppSettings {
            threads: 999,
            module_threads: BTreeMap::from([
                (" GROK ".into(), 250),
                ("unknown".into(), 10),
                ("reddit".into(), 0),
            ]),
            delay_ms: 1,
            timeout_ms: 1,
            retries: 99,
            max_scan_directories: u32::MAX,
            max_scan_files: u32::MAX,
            scan_budget_mib: u32::MAX,
            auto_check_updates: false,
        }
        .normalized();

        assert_eq!(settings.threads, 200);
        assert_eq!(settings.module_threads.get("grok"), Some(&200));
        assert!(!settings.module_threads.contains_key("unknown"));
        assert!(!settings.module_threads.contains_key("reddit"));
        assert_eq!(settings.delay_ms, 1);
        assert_eq!(settings.timeout_ms, MIN_TIMEOUT_MS);
        assert_eq!(settings.retries, 5);
        assert_eq!(settings.max_scan_directories, MAX_SCAN_DIRECTORIES);
        assert_eq!(settings.max_scan_files, MAX_SCAN_FILES);
        assert_eq!(settings.scan_budget_mib, MAX_SCAN_BUDGET_MIB);
        assert!(!settings.auto_check_updates);
    }

    #[test]
    fn zero_discovery_limits_restore_safe_defaults() {
        let settings = AppSettings {
            max_scan_directories: 0,
            max_scan_files: 0,
            scan_budget_mib: 0,
            ..AppSettings::default()
        }
        .normalized();

        assert_eq!(settings.max_scan_directories, DEFAULT_MAX_SCAN_DIRECTORIES);
        assert_eq!(settings.max_scan_files, DEFAULT_MAX_SCAN_FILES);
        assert_eq!(settings.scan_budget_mib, DEFAULT_SCAN_BUDGET_MIB);
        assert!(settings.auto_check_updates);
    }

    #[test]
    fn legacy_settings_receive_new_defaults() {
        let settings: AppSettings = serde_json::from_value(serde_json::json!({
            "threads": 32,
            "moduleThreads": {},
            "delayMs": 120,
            "timeoutMs": 10_000,
            "retries": 1
        }))
        .expect("deserialize legacy settings");

        assert_eq!(settings.max_scan_directories, DEFAULT_MAX_SCAN_DIRECTORIES);
        assert_eq!(settings.max_scan_files, DEFAULT_MAX_SCAN_FILES);
        assert_eq!(settings.scan_budget_mib, DEFAULT_SCAN_BUDGET_MIB);
        assert!(settings.auto_check_updates);
    }

    #[test]
    fn settings_round_trip_to_disk() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone());

        let saved = store
            .replace(AppSettings {
                threads: 32,
                max_scan_directories: 2_500,
                max_scan_files: 25_000,
                scan_budget_mib: 768,
                auto_check_updates: false,
                ..AppSettings::default()
            })
            .expect("save settings");
        assert_eq!(saved.threads, 32);
        assert_eq!(saved.max_scan_directories, 2_500);
        assert_eq!(saved.max_scan_files, 25_000);
        assert_eq!(saved.scan_budget_mib, 768);
        assert!(!saved.auto_check_updates);

        let reopened = SettingsStore::open(path);
        assert_eq!(reopened.snapshot().expect("read settings"), saved);
    }
}
