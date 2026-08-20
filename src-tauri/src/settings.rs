use crate::catalog;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_THREADS: u16 = 24;
const MAX_THREADS: u16 = 200;
const MAX_MODULE_THREADS: u16 = 32;
const DEFAULT_DELAY_MS: u64 = 120;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_RETRIES: u8 = 1;
const DEFAULT_MAX_SCAN_DIRECTORIES: u32 = 1_000;
const DEFAULT_MAX_SCAN_FILES: u32 = 10_000;
const DEFAULT_SCAN_BUDGET_MIB: u32 = 512;
const MAX_DELAY_MS: u64 = 60_000;
const MIN_TIMEOUT_MS: u64 = 3_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_SCAN_FILES: u32 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub threads: u16,
    pub module_threads: BTreeMap<String, u16>,
    pub delay_ms: u64,
    pub timeout_ms: u64,
    pub retries: u8,
    pub max_scan_directories: Option<u32>,
    pub max_scan_files: Option<u32>,
    pub scan_budget_mib: Option<u32>,
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
            max_scan_directories: None,
            max_scan_files: Some(DEFAULT_MAX_SCAN_FILES),
            scan_budget_mib: Some(DEFAULT_SCAN_BUDGET_MIB),
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
        // Missing fields receive the current defaults through `AppSettings::default`:
        // directory traversal is Unlimited, while file count and byte budget stay finite.
        // Explicit JSON null is the persisted/IPC representation of Unlimited. A zero
        // written by an older build was never a valid user-selected limit and therefore
        // retains its former meaning: restore the former finite default.
        if self.max_scan_directories == Some(0) {
            self.max_scan_directories = Some(DEFAULT_MAX_SCAN_DIRECTORIES);
        }
        self.max_scan_files = match self.max_scan_files {
            Some(0) => Some(DEFAULT_MAX_SCAN_FILES),
            Some(limit) => Some(limit.min(MAX_SCAN_FILES)),
            None => None,
        };
        if self.scan_budget_mib == Some(0) {
            self.scan_budget_mib = Some(DEFAULT_SCAN_BUDGET_MIB);
        }

        self.module_threads = self
            .module_threads
            .into_iter()
            .filter_map(|(id, threads)| {
                let id = id.trim().to_ascii_lowercase();
                if !catalog::is_known_module(&id) || threads == 0 {
                    return None;
                }
                Some((id, threads.min(MAX_MODULE_THREADS)))
            })
            .collect();

        self
    }
}

pub struct SettingsStore {
    path: PathBuf,
    current: RwLock<AppSettings>,
    recovery_notice: RwLock<Option<String>>,
}

impl SettingsStore {
    pub fn open(path: PathBuf) -> Self {
        let (current, recovery_notice) = load(&path);
        let current = current.unwrap_or_default().normalized();
        Self {
            path,
            current: RwLock::new(current),
            recovery_notice: RwLock::new(recovery_notice),
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

    pub fn recovery_notice(&self) -> Result<Option<String>, String> {
        self.recovery_notice
            .read()
            .map(|notice| notice.clone())
            .map_err(|_| "unable to read settings recovery status".to_string())
    }

    pub fn replace(&self, settings: AppSettings) -> Result<AppSettings, String> {
        let settings = settings.normalized();
        let mut current = self
            .current
            .write()
            .map_err(|_| "unable to update settings".to_string())?;
        // Hold the writer lock across persistence so concurrent saves cannot publish
        // older drafts after newer ones or race over backup replacement.
        persist(&self.path, &settings)?;
        *current = settings.clone();
        *self
            .recovery_notice
            .write()
            .map_err(|_| "unable to update settings recovery status".to_string())? = None;
        Ok(settings)
    }
}

fn load(path: &Path) -> (Option<AppSettings>, Option<String>) {
    if let Some(settings) = read_settings(path) {
        return (Some(settings), None);
    }

    let backup = backup_path(path);
    if let Some(settings) = read_settings(&backup) {
        return (
            Some(settings),
            Some(
                "The primary settings file was unreadable. Ayla recovered the last synced backup; review and save it to repair the primary file."
                    .to_string(),
            ),
        );
    }

    if path.exists() || backup.exists() {
        return (
            None,
            Some(
                "Saved settings were unreadable and no valid backup was available. Safe defaults are active; the damaged files were left in place for recovery."
                    .to_string(),
            ),
        );
    }

    (None, None)
}

fn persist(path: &Path, settings: &AppSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("unable to create the data directory: {error}"))?;
    }

    let data = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("unable to serialize settings: {error}"))?;
    let temporary = temporary_path(path);
    let backup = backup_path(path);

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("unable to create the temporary settings file: {error}"))?;
    if let Err(error) = file.write_all(&data).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "unable to write the temporary settings file: {error}"
        ));
    }
    drop(file);

    enum PreviousFile {
        None,
        ValidBackup,
        Corrupt(PathBuf),
    }

    let previous = if path.exists() {
        if read_settings(path).is_some() {
            if backup.exists() {
                fs::remove_file(&backup).map_err(|error| {
                    let _ = fs::remove_file(&temporary);
                    format!("unable to rotate the settings backup: {error}")
                })?;
            }
            fs::rename(path, &backup).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("unable to prepare the settings replacement: {error}")
            })?;
            PreviousFile::ValidBackup
        } else {
            // Never overwrite the last valid backup with a corrupt primary. Preserve
            // the damaged file separately for inspection and keep recovery viable
            // throughout publication of the replacement.
            let corrupt = corrupt_path(path);
            fs::rename(path, &corrupt).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("unable to preserve the corrupt settings file: {error}")
            })?;
            PreviousFile::Corrupt(corrupt)
        }
    } else {
        PreviousFile::None
    };

    if let Err(error) = fs::rename(&temporary, path) {
        match previous {
            PreviousFile::ValidBackup => {
                let _ = fs::rename(&backup, path);
            }
            PreviousFile::Corrupt(corrupt) => {
                let _ = fs::rename(corrupt, path);
            }
            PreviousFile::None => {}
        }
        let _ = fs::remove_file(&temporary);
        return Err(format!("unable to publish the settings file: {error}"));
    }

    sync_parent(path)?;
    Ok(())
}

fn read_settings(path: &Path) -> Option<AppSettings> {
    let data = fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{nonce}.tmp"));
    PathBuf::from(name)
}

fn corrupt_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".corrupt.{nonce}"));
    PathBuf::from(name)
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match File::open(parent).and_then(|directory| directory.sync_all()) {
        Ok(()) => Ok(()),
        // Windows does not allow opening directories as regular files. The two file
        // renames above are still atomic on the same volume and each payload is synced.
        Err(error) if cfg!(windows) => {
            let _ = error;
            Ok(())
        }
        Err(error) => Err(format!("unable to sync the settings directory: {error}")),
    }
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
            max_scan_directories: Some(u32::MAX),
            max_scan_files: Some(u32::MAX),
            scan_budget_mib: Some(u32::MAX),
            auto_check_updates: false,
        }
        .normalized();

        assert_eq!(settings.threads, 200);
        assert_eq!(settings.module_threads.get("grok"), Some(&32));
        assert!(!settings.module_threads.contains_key("unknown"));
        assert!(!settings.module_threads.contains_key("reddit"));
        assert_eq!(settings.delay_ms, 1);
        assert_eq!(settings.timeout_ms, MIN_TIMEOUT_MS);
        assert_eq!(settings.retries, 5);
        assert_eq!(settings.max_scan_directories, Some(u32::MAX));
        assert_eq!(settings.max_scan_files, Some(MAX_SCAN_FILES));
        assert_eq!(settings.scan_budget_mib, Some(u32::MAX));
        assert!(!settings.auto_check_updates);
    }

    #[test]
    fn legacy_zero_discovery_limits_restore_safe_defaults() {
        let settings = AppSettings {
            max_scan_directories: Some(0),
            max_scan_files: Some(0),
            scan_budget_mib: Some(0),
            ..AppSettings::default()
        }
        .normalized();

        assert_eq!(
            settings.max_scan_directories,
            Some(DEFAULT_MAX_SCAN_DIRECTORIES)
        );
        assert_eq!(settings.max_scan_files, Some(DEFAULT_MAX_SCAN_FILES));
        assert_eq!(settings.scan_budget_mib, Some(DEFAULT_SCAN_BUDGET_MIB));
        assert!(settings.auto_check_updates);
    }

    #[test]
    fn unlimited_discovery_limits_round_trip_to_disk() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone());

        let saved = store
            .replace(AppSettings {
                max_scan_directories: None,
                max_scan_files: None,
                scan_budget_mib: None,
                ..AppSettings::default()
            })
            .expect("save unlimited discovery limits");
        assert_eq!(saved.max_scan_directories, None);
        assert_eq!(saved.max_scan_files, None);
        assert_eq!(saved.scan_budget_mib, None);

        let reopened = SettingsStore::open(path).snapshot().expect("read settings");
        assert_eq!(reopened.max_scan_directories, None);
        assert_eq!(reopened.max_scan_files, None);
        assert_eq!(reopened.scan_budget_mib, None);
    }

    #[test]
    fn missing_discovery_fields_receive_current_defaults() {
        let settings: AppSettings = serde_json::from_value(serde_json::json!({
            "threads": 32,
            "moduleThreads": {},
            "delayMs": 120,
            "timeoutMs": 10_000,
            "retries": 1
        }))
        .expect("deserialize legacy settings");

        assert_eq!(settings.max_scan_directories, None);
        assert_eq!(settings.max_scan_files, Some(DEFAULT_MAX_SCAN_FILES));
        assert_eq!(settings.scan_budget_mib, Some(DEFAULT_SCAN_BUDGET_MIB));
        assert!(settings.auto_check_updates);
    }

    #[test]
    fn fresh_settings_default_directory_scanning_to_unlimited() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let settings = SettingsStore::open(directory.path().join("missing-settings.json"))
            .snapshot()
            .expect("read fresh defaults");

        assert_eq!(settings.max_scan_directories, None);
        assert_eq!(settings.max_scan_files, Some(DEFAULT_MAX_SCAN_FILES));
        assert_eq!(settings.scan_budget_mib, Some(DEFAULT_SCAN_BUDGET_MIB));
    }

    #[test]
    fn settings_round_trip_to_disk() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone());

        let saved = store
            .replace(AppSettings {
                threads: 32,
                max_scan_directories: Some(2_500),
                max_scan_files: Some(25_000),
                scan_budget_mib: Some(768),
                auto_check_updates: false,
                ..AppSettings::default()
            })
            .expect("save settings");
        assert_eq!(saved.threads, 32);
        assert_eq!(saved.max_scan_directories, Some(2_500));
        assert_eq!(saved.max_scan_files, Some(25_000));
        assert_eq!(saved.scan_budget_mib, Some(768));
        assert!(!saved.auto_check_updates);

        let reopened = SettingsStore::open(path.clone());
        assert_eq!(reopened.snapshot().expect("read settings"), saved);
    }

    #[test]
    fn corrupt_primary_recovers_last_synced_backup() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone());

        let first = store
            .replace(AppSettings {
                threads: 12,
                ..AppSettings::default()
            })
            .expect("save first settings");
        store
            .replace(AppSettings {
                threads: 18,
                ..AppSettings::default()
            })
            .expect("save second settings");

        fs::write(&path, b"{truncated").expect("corrupt primary");
        let reopened = SettingsStore::open(path.clone());
        let recovered = reopened.snapshot().expect("read recovered settings");
        assert_eq!(recovered, first);
        assert!(
            reopened
                .recovery_notice()
                .expect("read recovery notice")
                .is_some()
        );

        let repaired = reopened
            .replace(AppSettings {
                threads: 22,
                ..AppSettings::default()
            })
            .expect("repair settings after recovery");
        assert_eq!(read_settings(&path), Some(repaired));
        assert_eq!(read_settings(&backup_path(&path)), Some(first));
        assert!(
            fs::read_dir(directory.path())
                .expect("list recovered settings")
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains(".corrupt."))
        );
    }

    #[test]
    fn successful_save_never_leaves_a_temporary_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone());

        store
            .replace(AppSettings::default())
            .expect("save settings");

        let temporary_files = fs::read_dir(directory.path())
            .expect("list settings directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temporary_files, 0);
    }

    #[test]
    fn corrupt_settings_without_backup_are_reported_instead_of_silent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.json");
        fs::write(&path, b"{truncated").expect("write corrupt settings");

        let store = SettingsStore::open(path);
        assert_eq!(
            store.snapshot().expect("safe defaults"),
            AppSettings::default()
        );
        assert!(
            store
                .recovery_notice()
                .expect("read recovery notice")
                .is_some()
        );
    }
}
