use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_COOKIES: usize = 10_000;
const MAX_TOKEN_CHUNKS: usize = 20;
const MAX_JSON_DEPTH: usize = 128;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_COOKIE_HEADER_BYTES: usize = 64 * 1024;
// Bounded by MAX_COOKIES with a generous per-cookie structural-token allowance so a
// legitimate multi-thousand-cookie export is not rejected before serde parses it. The
// 2 MiB artifact cap already bounds the absolute amount of work regardless of this value.
const MAX_JSON_STRUCTURAL_TOKENS: usize = MAX_COOKIES * 64;

const SESSION_COOKIE: &str = "__Secure-next-auth.session-token";
const LEGACY_SESSION_COOKIE: &str = "next-auth.session-token";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InspectionResult {
    Ready,
    MissingAuth,
    Expired,
    Invalid,
    Unreadable,
    TooLarge,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactReadError {
    Inspection(InspectionResult),
    BudgetExceeded,
}

impl ArtifactReadError {
    #[cfg(test)]
    fn inspection_result(self) -> InspectionResult {
        match self {
            Self::Inspection(result) => result,
            Self::BudgetExceeded => InspectionResult::TooLarge,
        }
    }
}

pub(crate) struct PreparedChatGptAuth {
    cookies: Vec<CookieMeta>,
    artifact_bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatGptEndpoint {
    Session,
    Accounts,
    Me,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatGptCookieError {
    HeaderTooLarge,
}

pub(crate) enum ChatGptArtifactPreparation {
    Ready(PreparedChatGptAuth),
    Rejected {
        reason: InspectionResult,
        artifact_bytes: Option<Vec<u8>>,
    },
}

impl PreparedChatGptAuth {
    /// Renders only the session cookies which a browser would attach to the exact
    /// ChatGPT endpoint. Domain, host-only, path, Secure, and expiry metadata remain
    /// authoritative; cookies from another OpenAI host or path never enter the header.
    pub(crate) fn cookie_header_for(
        &self,
        endpoint: ChatGptEndpoint,
        now: i64,
    ) -> Result<Option<String>, ChatGptCookieError> {
        cookie_header_for_request(
            &self.cookies,
            endpoint.host(),
            endpoint.path(),
            endpoint.is_https(),
            now,
            |name| endpoint.allows_cookie(name),
        )
    }

    /// `oai-did` is read as request metadata, not forwarded as a cookie. The same
    /// browser scope checks still apply before it can become the device-id header.
    pub(crate) fn device_id_for(&self, endpoint: ChatGptEndpoint, now: i64) -> Option<&str> {
        matching_cookies(
            &self.cookies,
            endpoint.host(),
            endpoint.path(),
            endpoint.is_https(),
            now,
        )
        .into_iter()
        .find(|cookie| cookie.name == "oai-did")
        .map(|cookie| cookie.value.as_str())
    }

    pub(crate) fn into_artifact_bytes(self) -> Vec<u8> {
        self.artifact_bytes
    }
}

impl ChatGptEndpoint {
    const fn host(self) -> &'static str {
        "chatgpt.com"
    }

    const fn path(self) -> &'static str {
        match self {
            Self::Session => "/api/auth/session",
            Self::Accounts => "/backend-api/accounts/check/v4-2023-04-27",
            Self::Me => "/backend-api/me",
        }
    }

    const fn is_https(self) -> bool {
        true
    }

    fn allows_cookie(self, name: &str) -> bool {
        // All three endpoints can authenticate from the NextAuth session cookie when a
        // bearer token is unavailable. No unrelated browser cookie is ever forwarded.
        matches!(self, Self::Session | Self::Accounts | Self::Me) && is_session_cookie_name(name)
    }
}

#[derive(Clone)]
struct CookieMeta {
    domain: String,
    path: String,
    secure: bool,
    host_only: bool,
    scope_valid: bool,
    name: String,
    expires_at: Option<i64>,
    expiry_valid: bool,
    value: String,
    value_valid: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialState {
    Ready,
    Expired,
    Empty,
    Invalid,
}

#[cfg(test)]
pub(crate) fn prepare_chatgpt_path_with_budget(
    path: &Path,
    remaining_bytes: &AtomicU64,
) -> Result<PreparedChatGptAuth, InspectionResult> {
    prepare_chatgpt_path_at_with_budget(path, now_seconds(), Some(remaining_bytes))
}

/// Reads an artifact once for the task pipeline. `None` disables only the aggregate byte
/// budget; locality, regular-file, reparse-point, and per-artifact size checks still apply.
pub(crate) fn read_artifact_path(
    path: &Path,
    remaining_bytes: Option<&AtomicU64>,
) -> Result<Vec<u8>, ArtifactReadError> {
    read_limited_detailed(path, remaining_bytes)
}

pub(crate) fn load_chatgpt_bytes(data: Vec<u8>) -> ChatGptArtifactPreparation {
    load_chatgpt_bytes_at(data, now_seconds())
}

pub(crate) fn output_directory_is_local(path: &Path) -> bool {
    artifact_path_is_local(path)
}

pub(crate) fn artifact_path_is_local(path: &Path) -> bool {
    path.is_absolute() && !has_disallowed_path_prefix(path)
}

#[cfg(windows)]
pub(crate) fn opened_file_is_within_local_directory(file: &File, trusted_directory: &Path) -> bool {
    let Some(file_path) = windows_handle_target_path(file) else {
        return false;
    };
    if !windows_handle_path_is_local(&file_path) {
        return false;
    }
    let file_components: Vec<_> = Path::new(&file_path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let directory_components: Vec<_> = trusted_directory
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    file_components.starts_with(&directory_components)
}

#[cfg(not(windows))]
pub(crate) fn opened_file_is_within_local_directory(_file: &File, _directory: &Path) -> bool {
    true
}

#[cfg(test)]
fn inspect_chatgpt_path_at(path: &Path, now: i64) -> InspectionResult {
    inspect_chatgpt_path_at_with_budget(path, now, None)
}

#[cfg(test)]
fn inspect_chatgpt_path_at_with_budget(
    path: &Path,
    now: i64,
    remaining_bytes: Option<&AtomicU64>,
) -> InspectionResult {
    match prepare_chatgpt_path_at_with_budget(path, now, remaining_bytes) {
        Ok(_) => InspectionResult::Ready,
        Err(result) => result,
    }
}

#[cfg(test)]
fn prepare_chatgpt_path_at_with_budget(
    path: &Path,
    now: i64,
    remaining_bytes: Option<&AtomicU64>,
) -> Result<PreparedChatGptAuth, InspectionResult> {
    match load_chatgpt_path_at_with_budget(path, now, remaining_bytes) {
        ChatGptArtifactPreparation::Ready(auth) => Ok(auth),
        ChatGptArtifactPreparation::Rejected { reason, .. } => Err(reason),
    }
}

#[cfg(test)]
fn load_chatgpt_path_at_with_budget(
    path: &Path,
    now: i64,
    remaining_bytes: Option<&AtomicU64>,
) -> ChatGptArtifactPreparation {
    let data = match read_limited(path, remaining_bytes) {
        Ok(data) => data,
        Err(reason) => {
            return ChatGptArtifactPreparation::Rejected {
                reason,
                artifact_bytes: None,
            };
        }
    };
    load_chatgpt_bytes_at(data, now)
}

fn load_chatgpt_bytes_at(data: Vec<u8>, now: i64) -> ChatGptArtifactPreparation {
    let text = match std::str::from_utf8(&data) {
        Ok(text) => text.trim().trim_start_matches('\u{feff}').trim(),
        Err(_) => {
            return ChatGptArtifactPreparation::Rejected {
                reason: InspectionResult::Invalid,
                artifact_bytes: Some(data),
            };
        }
    };
    if text.is_empty() {
        return ChatGptArtifactPreparation::Rejected {
            reason: InspectionResult::Invalid,
            artifact_bytes: Some(data),
        };
    }

    let cookies = match parse_document(text) {
        Ok(cookies) if !cookies.is_empty() => cookies,
        _ => {
            return ChatGptArtifactPreparation::Rejected {
                reason: InspectionResult::Invalid,
                artifact_bytes: Some(data),
            };
        }
    };
    let result = classify_chatgpt(&cookies, now);
    if result != InspectionResult::Ready {
        return ChatGptArtifactPreparation::Rejected {
            reason: result,
            artifact_bytes: Some(data),
        };
    }
    let Some(mut auth) = prepared_auth(&cookies, now) else {
        return ChatGptArtifactPreparation::Rejected {
            reason: InspectionResult::Invalid,
            artifact_bytes: Some(data),
        };
    };
    auth.artifact_bytes = data;
    ChatGptArtifactPreparation::Ready(auth)
}

#[cfg(test)]
fn read_limited(
    path: &Path,
    remaining_bytes: Option<&AtomicU64>,
) -> Result<Vec<u8>, InspectionResult> {
    read_limited_detailed(path, remaining_bytes).map_err(ArtifactReadError::inspection_result)
}

fn read_limited_detailed(
    path: &Path,
    remaining_bytes: Option<&AtomicU64>,
) -> Result<Vec<u8>, ArtifactReadError> {
    if !path.is_absolute() || has_disallowed_path_prefix(path) {
        return Err(ArtifactReadError::Inspection(InspectionResult::Unreadable));
    }

    let file = File::open(path)
        .map_err(|_| ArtifactReadError::Inspection(InspectionResult::Unreadable))?;
    let metadata = file
        .metadata()
        .map_err(|_| ArtifactReadError::Inspection(InspectionResult::Unreadable))?;
    if !metadata.is_file() {
        return Err(ArtifactReadError::Inspection(InspectionResult::Unreadable));
    }
    #[cfg(windows)]
    if !windows_handle_target_is_local(&file) {
        return Err(ArtifactReadError::Inspection(InspectionResult::Unreadable));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(ArtifactReadError::Inspection(InspectionResult::TooLarge));
    }

    // Reserve the declared size up front so an exhausted shared budget stops the read
    // before any bytes are consumed; the reservation is reconciled with the bytes
    // actually consumed, including partial reads and per-file-limit failures.
    let reserved = metadata.len();
    if !charge_budget(remaining_bytes, reserved) {
        return Err(ArtifactReadError::BudgetExceeded);
    }

    let mut reader = file.take(MAX_ARTIFACT_BYTES + 1);
    let mut data = Vec::with_capacity(reserved.min(MAX_ARTIFACT_BYTES) as usize);
    let outcome = match reader.read_to_end(&mut data) {
        Ok(_) if data.len() as u64 > MAX_ARTIFACT_BYTES => {
            Err(ArtifactReadError::Inspection(InspectionResult::TooLarge))
        }
        Ok(_) => Ok(()),
        Err(_) => Err(ArtifactReadError::Inspection(InspectionResult::Unreadable)),
    };

    // Charge the bytes actually consumed even when the read or per-file check fails. This
    // prevents a file which grows after metadata from being retried indefinitely without
    // consuming the finite aggregate scan budget.
    let actual = data.len() as u64;
    if !reconcile_read_budget(remaining_bytes, reserved, actual) {
        return Err(ArtifactReadError::BudgetExceeded);
    }

    match outcome {
        Ok(()) => Ok(data),
        Err(error) => Err(error),
    }
}

fn reconcile_read_budget(remaining_bytes: Option<&AtomicU64>, reserved: u64, actual: u64) -> bool {
    if actual < reserved {
        refund_budget(remaining_bytes, reserved - actual);
        true
    } else if actual == reserved || charge_budget(remaining_bytes, actual - reserved) {
        true
    } else {
        if let Some(remaining) = remaining_bytes {
            remaining.store(0, Ordering::Release);
        }
        false
    }
}

fn charge_budget(remaining_bytes: Option<&AtomicU64>, amount: u64) -> bool {
    match remaining_bytes {
        None => true,
        Some(remaining) => remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_sub(amount)
            })
            .is_ok(),
    }
}

fn refund_budget(remaining_bytes: Option<&AtomicU64>, amount: u64) {
    if let Some(remaining) = remaining_bytes
        && amount > 0
    {
        remaining.fetch_add(amount, Ordering::AcqRel);
    }
}

#[cfg(windows)]
fn has_disallowed_path_prefix(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    let remote_or_device = matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                Prefix::UNC(_, _)
                    | Prefix::VerbatimUNC(_, _)
                    | Prefix::DeviceNS(_)
                    | Prefix::Verbatim(_)
            )
    );
    let reserved_name = path.components().any(|component| {
        let Component::Normal(component) = component else {
            return false;
        };
        let component = component.to_string_lossy();
        let stem = component
            .trim_end_matches([' ', '.'])
            .split(['.', ':'])
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        matches!(
            stem.as_str(),
            "CON"
                | "CONIN$"
                | "CONOUT$"
                | "PRN"
                | "AUX"
                | "NUL"
                | "CLOCK$"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
    });
    remote_or_device
        || !windows_drive_is_local(path)
        || reserved_name
        || windows_path_contains_reparse_point(path)
}

#[cfg(windows)]
fn windows_drive_letter_is_local(letter: u8) -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetDriveTypeW"]
        fn get_drive_type_w(root_path_name: *const u16) -> u32;
    }

    let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: `root` is a valid, nul-terminated `X:\` UTF-16 buffer.
    let drive_type = unsafe { get_drive_type_w(root.as_ptr()) };
    matches!(drive_type, 2 | 3 | 5 | 6)
}

#[cfg(windows)]
fn windows_drive_is_local(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let letter = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
        _ => return false,
    };
    windows_drive_letter_is_local(letter)
}

// Validates the volume the file handle actually resolved to, closing the gap between the
// pre-open path walk in `has_disallowed_path_prefix` and `File::open` (which re-resolves
// the path and follows any junction/symlink swapped in after the walk). The canonical path
// reported by the open handle is rejected if it targets a network (UNC) share or a
// non-local drive.
#[cfg(windows)]
fn windows_handle_target_is_local(file: &File) -> bool {
    windows_handle_target_path(file).is_some_and(|path| windows_handle_path_is_local(&path))
}

#[cfg(windows)]
fn windows_handle_target_path(file: &File) -> Option<String> {
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetFinalPathNameByHandleW"]
        fn get_final_path_name_by_handle_w(
            file: *mut core::ffi::c_void,
            path: *mut u16,
            path_len: u32,
            flags: u32,
        ) -> u32;
    }

    let handle = file.as_raw_handle();
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: `handle` is a live, open file handle for the duration of the call and
        // `buffer` is writable for `buffer.len()` UTF-16 code units.
        let length = unsafe {
            get_final_path_name_by_handle_w(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0)
        };
        if length == 0 {
            return None;
        }
        let length = length as usize;
        if length < buffer.len() {
            buffer.truncate(length);
            break;
        }
        if length > 66_000 {
            return None;
        }
        buffer = vec![0u16; length + 1];
    }

    Some(String::from_utf16_lossy(&buffer))
}

#[cfg(windows)]
fn windows_handle_path_is_local(path: &str) -> bool {
    let stripped = path.strip_prefix(r"\\?\").unwrap_or(path);
    if stripped
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC\\"))
    {
        return false;
    }
    let bytes = stripped.as_bytes();
    bytes.len() >= 2
        && bytes[1] == b':'
        && bytes[0].is_ascii_alphabetic()
        && windows_drive_letter_is_local(bytes[0])
}

#[cfg(windows)]
fn windows_path_contains_reparse_point(path: &Path) -> bool {
    use std::{
        os::windows::fs::MetadataExt,
        path::{Component, PathBuf},
    };

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return true;
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

#[cfg(not(windows))]
fn has_disallowed_path_prefix(_path: &Path) -> bool {
    true
}
fn parse_document(text: &str) -> Result<Vec<CookieMeta>, ()> {
    if text.starts_with('[') || text.starts_with('{') || text.starts_with('"') {
        return parse_json(text);
    }
    if let Some(token) = token_only_value(text) {
        return Ok(vec![CookieMeta {
            domain: "chatgpt.com".to_string(),
            path: "/".to_string(),
            secure: true,
            host_only: true,
            scope_valid: true,
            name: SESSION_COOKIE.to_string(),
            expires_at: None,
            expiry_valid: true,
            value: token.to_string(),
            value_valid: true,
        }]);
    }
    parse_netscape(text)
}

fn token_only_value(text: &str) -> Option<&str> {
    if text.contains('\t') || text.lines().count() != 1 {
        return None;
    }
    let text = text.trim();
    let value = text
        .strip_prefix(SESSION_COOKIE)
        .and_then(|value| value.strip_prefix('='))
        .or_else(|| {
            text.strip_prefix(LEGACY_SESSION_COOKIE)
                .and_then(|value| value.strip_prefix('='))
        })
        .unwrap_or(text)
        .trim();
    let valid_chars = value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'/' | b'%' | b'=' | b'|')
    });
    (value.len() > 40 && value.len() <= MAX_TOKEN_BYTES && valid_chars).then_some(value)
}

fn first_seven<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<[&'a str; 7]> {
    Some([
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    ])
}

fn parse_netscape(text: &str) -> Result<Vec<CookieMeta>, ()> {
    let mut cookies = Vec::new();
    for raw_line in text.lines() {
        let mut line = raw_line.trim_end_matches('\r').trim_start();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#HttpOnly_") {
            line = rest;
        } else if line.starts_with('#') {
            continue;
        }

        let Some(parts) =
            first_seven(line.split('\t')).or_else(|| first_seven(line.split_whitespace()))
        else {
            continue;
        };
        let name = parts[5].trim();
        if name.is_empty() {
            continue;
        }
        let (expires_at, expiry_valid) = match parse_expiry_text(parts[4].trim()) {
            Ok(expires_at) => (expires_at, true),
            Err(()) => (None, false),
        };
        let include_subdomains = parse_cookie_bool(parts[1]);
        let secure = parse_cookie_bool(parts[3]);
        let path = if parts[2].trim().is_empty() {
            "/"
        } else {
            parts[2].trim()
        };
        cookies.push(CookieMeta {
            domain: normalize_domain(parts[0]),
            path: path.to_string(),
            secure: secure.unwrap_or(false),
            host_only: !include_subdomains.unwrap_or(false),
            scope_valid: include_subdomains.is_some() && secure.is_some() && safe_cookie_path(path),
            name: name.to_string(),
            expires_at,
            expiry_valid,
            value: parts[6].trim().to_string(),
            value_valid: true,
        });
        if cookies.len() > MAX_COOKIES {
            return Err(());
        }
    }
    (!cookies.is_empty()).then_some(cookies).ok_or(())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonDocument {
    Array(Vec<JsonCookie>),
    Object(JsonObject),
    Token(String),
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JsonObject {
    cookies: Vec<JsonCookie>,
    #[serde(
        alias = "expirationDate",
        alias = "expiry",
        alias = "expiresAt",
        alias = "expires_at"
    )]
    expires: Option<JsonScalar>,
    token: Option<String>,
    session_token: Option<String>,
    #[serde(rename = "sessionToken")]
    session_token_camel: Option<String>,
    #[serde(rename = "__Secure-next-auth.session-token")]
    secure_session_token: Option<String>,
    #[serde(rename = "next-auth.session-token")]
    legacy_session_token: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JsonCookie {
    #[serde(alias = "Name", alias = "Name raw", alias = "key")]
    name: String,
    #[serde(alias = "Value", alias = "Content raw", alias = "content")]
    value: Option<JsonScalar>,
    #[serde(alias = "Domain", alias = "Host raw", alias = "host")]
    domain: String,
    #[serde(alias = "Path", alias = "Path raw")]
    path: String,
    #[serde(alias = "Secure")]
    secure: Option<bool>,
    #[serde(alias = "hostOnly", alias = "HostOnly")]
    host_only: Option<bool>,
    #[serde(alias = "expirationDate", alias = "expiry", alias = "Expires raw")]
    expires: Option<JsonScalar>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonScalar {
    String(String),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
}

impl JsonScalar {
    fn string_value(&self) -> Result<String, ()> {
        match self {
            Self::String(value) => Ok(value.trim().to_string()),
            Self::Signed(_) | Self::Unsigned(_) | Self::Float(_) => Err(()),
        }
    }

    fn expiry(&self) -> Result<Option<i64>, ()> {
        let value = match self {
            Self::String(value) => value.trim().parse::<f64>().map_err(|_| ())?,
            Self::Signed(value) => *value as f64,
            Self::Unsigned(value) => *value as f64,
            Self::Float(value) => *value,
        };
        normalize_expiry(value)
    }
}

fn parse_expiry_text(value: &str) -> Result<Option<i64>, ()> {
    let value = value.trim().parse::<f64>().map_err(|_| ())?;
    normalize_expiry(value)
}

fn normalize_expiry(mut value: f64) -> Result<Option<i64>, ()> {
    if value == -1.0 {
        return Ok(None);
    }
    if !(0.0..=100_000_000_000_000_000_000.0).contains(&value) {
        return Err(());
    }
    if value == 0.0 {
        return Ok(None);
    }
    while value >= 100_000_000_000.0 {
        value /= 1_000.0;
    }
    if value < 1.0 || value > i64::MAX as f64 {
        return Err(());
    }
    Ok(Some(value.floor() as i64))
}

fn optional_expiry(value: Option<&JsonScalar>) -> (Option<i64>, bool) {
    match value.map(JsonScalar::expiry) {
        None => (None, true),
        Some(Ok(expires_at)) => (expires_at, true),
        Some(Err(())) => (None, false),
    }
}

fn optional_string_value(value: Option<&JsonScalar>) -> (String, bool) {
    match value.map(JsonScalar::string_value) {
        None => (String::new(), true),
        Some(Ok(value)) => (value, true),
        Some(Err(())) => (String::new(), false),
    }
}

#[derive(Clone, Copy)]
enum JsonContainer {
    Object,
    Array { items: usize, has_value: bool },
}

fn mark_json_array_value(stack: &mut [JsonContainer]) -> bool {
    let Some(JsonContainer::Array { items, has_value }) = stack.last_mut() else {
        return true;
    };
    if !*has_value {
        *items = items.saturating_add(1);
        *has_value = true;
    }
    *items <= MAX_COOKIES
}

fn json_arrays_within_limits(text: &str) -> bool {
    let mut stack = [JsonContainer::Object; MAX_JSON_DEPTH];
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut structural_tokens = 0usize;

    for byte in text.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        if matches!(byte, b'"' | b'[' | b']' | b'{' | b'}' | b',' | b':') {
            structural_tokens += 1;
            if structural_tokens > MAX_JSON_STRUCTURAL_TOKENS {
                return false;
            }
        }

        match byte {
            b'"' => {
                if !mark_json_array_value(&mut stack[..depth]) {
                    return false;
                }
                in_string = true;
            }
            b'[' | b'{' => {
                if !mark_json_array_value(&mut stack[..depth]) || depth == MAX_JSON_DEPTH {
                    return false;
                }
                stack[depth] = if byte == b'[' {
                    JsonContainer::Array {
                        items: 0,
                        has_value: false,
                    }
                } else {
                    JsonContainer::Object
                };
                depth += 1;
            }
            b']' => {
                if depth == 0 || !matches!(stack[depth - 1], JsonContainer::Array { .. }) {
                    return false;
                }
                depth -= 1;
            }
            b'}' => {
                if depth == 0 || !matches!(stack[depth - 1], JsonContainer::Object) {
                    return false;
                }
                depth -= 1;
            }
            b',' => {
                if let Some(JsonContainer::Array { has_value, .. }) = stack[..depth].last_mut() {
                    *has_value = false;
                }
            }
            byte if byte.is_ascii_whitespace() => {}
            _ => {
                if !mark_json_array_value(&mut stack[..depth]) {
                    return false;
                }
            }
        }
    }

    depth == 0 && !in_string
}

fn parse_json(text: &str) -> Result<Vec<CookieMeta>, ()> {
    if !json_arrays_within_limits(text) {
        return Err(());
    }
    let document: JsonDocument = serde_json::from_str(text).map_err(|_| ())?;
    let mut cookies = Vec::new();
    match document {
        JsonDocument::Array(items) => append_json_cookies(&mut cookies, items)?,
        JsonDocument::Token(token) => {
            let token = token_only_value(&token).ok_or(())?;
            cookies.push(CookieMeta {
                domain: "chatgpt.com".to_string(),
                path: "/".to_string(),
                secure: true,
                host_only: true,
                scope_valid: true,
                name: SESSION_COOKIE.to_string(),
                expires_at: None,
                expiry_valid: true,
                value: token.to_string(),
                value_valid: true,
            });
        }
        JsonDocument::Object(object) => {
            let (expires_at, expiry_valid) = optional_expiry(object.expires.as_ref());
            append_json_cookies(&mut cookies, object.cookies)?;
            for token in [
                object.token,
                object.session_token,
                object.session_token_camel,
                object.secure_session_token,
                object.legacy_session_token,
            ]
            .into_iter()
            .flatten()
            {
                let trimmed = token.trim();
                // Mirror the plaintext and top-level JSON-string paths: store the value
                // returned by token_only_value (with any `name=` prefix stripped) rather
                // than the raw field, so an embedded cookie name is not sent twice.
                let (value, value_valid) = match token_only_value(trimmed) {
                    Some(stripped) => (stripped.to_string(), true),
                    None => (trimmed.to_string(), trimmed.is_empty()),
                };
                cookies.push(CookieMeta {
                    domain: "chatgpt.com".to_string(),
                    path: "/".to_string(),
                    secure: true,
                    host_only: true,
                    scope_valid: true,
                    name: SESSION_COOKIE.to_string(),
                    expires_at,
                    expiry_valid,
                    value,
                    value_valid,
                });
                if cookies.len() > MAX_COOKIES {
                    return Err(());
                }
            }
        }
    }
    (!cookies.is_empty()).then_some(cookies).ok_or(())
}

fn append_json_cookies(target: &mut Vec<CookieMeta>, items: Vec<JsonCookie>) -> Result<(), ()> {
    for item in items {
        let name = item.name.trim();
        if name.is_empty() {
            continue;
        }
        let (expires_at, expiry_valid) = optional_expiry(item.expires.as_ref());
        let (value, value_valid) = optional_string_value(item.value.as_ref());
        let raw_domain = item.domain.trim();
        let path = if item.path.trim().is_empty() {
            "/"
        } else {
            item.path.trim()
        };
        target.push(CookieMeta {
            domain: normalize_domain(raw_domain),
            path: path.to_string(),
            secure: item.secure.unwrap_or_else(|| name.starts_with("__Secure-")),
            host_only: item
                .host_only
                .unwrap_or_else(|| !raw_domain.starts_with('.')),
            scope_valid: safe_cookie_path(path),
            name: name.to_string(),
            expires_at,
            expiry_valid,
            value,
            value_valid,
        });
        if target.len() > MAX_COOKIES {
            return Err(());
        }
    }
    Ok(())
}

fn classify_chatgpt(cookies: &[CookieMeta], now: i64) -> InspectionResult {
    let mut direct = Vec::new();
    let mut chunk_groups =
        BTreeMap::<(String, String, bool, bool, &'static str), ChunkGroup>::new();
    let mut identities = BTreeMap::<(String, String, String), &CookieMeta>::new();

    for cookie in cookies {
        let auth_name = cookie.name == SESSION_COOKIE
            || cookie.name == LEGACY_SESSION_COOKIE
            || cookie.name.starts_with("__Secure-next-auth.session-token.")
            || cookie.name.starts_with("next-auth.session-token.");
        if !auth_name || !chatgpt_domain(&cookie.domain) || !cookie.scope_valid {
            continue;
        }
        if let Some(existing) = identities.insert(
            (
                cookie.domain.clone(),
                cookie.path.clone(),
                cookie.name.clone(),
            ),
            cookie,
        ) {
            let identical = existing.secure == cookie.secure
                && existing.host_only == cookie.host_only
                && existing.expires_at == cookie.expires_at
                && existing.expiry_valid == cookie.expiry_valid
                && existing.value == cookie.value
                && existing.value_valid == cookie.value_valid;
            if identical {
                continue;
            }
            return InspectionResult::Invalid;
        }
        if !cookie_matches_scope(
            cookie,
            ChatGptEndpoint::Session.host(),
            ChatGptEndpoint::Session.path(),
            ChatGptEndpoint::Session.is_https(),
        ) {
            continue;
        }
        // A cookie whose name or value cannot be safely placed in a Cookie header is not
        // usable, so classification treats it as Invalid. This keeps classify_chatgpt and
        // prepared_auth in agreement about which credentials are eligible, so a Ready
        // verdict can never rest on a cookie that prepared_auth would later drop.
        let state = if safe_cookie_name(&cookie.name) && safe_cookie_value(&cookie.value) {
            credential_state(cookie, now)
        } else {
            CredentialState::Invalid
        };
        if cookie.name == SESSION_COOKIE || cookie.name == LEGACY_SESSION_COOKIE {
            direct.push(state);
            continue;
        }
        match session_chunk(&cookie.name) {
            Ok(Some((family, index))) if index < MAX_TOKEN_CHUNKS => {
                let group = chunk_groups
                    .entry((
                        cookie.domain.clone(),
                        cookie.path.clone(),
                        cookie.secure,
                        cookie.host_only,
                        family,
                    ))
                    .or_default();
                if group.chunks[index].replace(state).is_some() {
                    group.ambiguous = true;
                }
                group.highest = Some(group.highest.map_or(index, |highest| highest.max(index)));
            }
            Ok(Some(_)) | Err(()) => return InspectionResult::Invalid,
            Ok(None) => {}
        }
    }

    if direct.contains(&CredentialState::Ready) {
        return InspectionResult::Ready;
    }
    let chunk_states = chunk_groups
        .values()
        .map(ChunkGroup::state)
        .collect::<Vec<_>>();
    if chunk_states.contains(&CredentialState::Ready) {
        return InspectionResult::Ready;
    }
    if !chunk_states.is_empty() {
        return if chunk_states.contains(&CredentialState::Empty) {
            InspectionResult::MissingAuth
        } else if chunk_states.contains(&CredentialState::Invalid) {
            InspectionResult::Invalid
        } else if chunk_states.contains(&CredentialState::Expired) {
            InspectionResult::Expired
        } else {
            InspectionResult::MissingAuth
        };
    }
    if direct.contains(&CredentialState::Invalid) {
        InspectionResult::Invalid
    } else if direct.contains(&CredentialState::Expired) {
        InspectionResult::Expired
    } else {
        InspectionResult::MissingAuth
    }
}

#[derive(Default)]
struct ChunkGroup {
    chunks: [Option<CredentialState>; MAX_TOKEN_CHUNKS],
    highest: Option<usize>,
    ambiguous: bool,
}

impl ChunkGroup {
    fn state(&self) -> CredentialState {
        if self.ambiguous {
            return CredentialState::Invalid;
        }
        let Some(highest) = self.highest else {
            return CredentialState::Empty;
        };
        let selected = &self.chunks[..=highest];
        if selected.iter().any(Option::is_none) || selected.contains(&Some(CredentialState::Empty))
        {
            CredentialState::Empty
        } else if selected.contains(&Some(CredentialState::Invalid)) {
            CredentialState::Invalid
        } else if selected.contains(&Some(CredentialState::Expired)) {
            CredentialState::Expired
        } else {
            CredentialState::Ready
        }
    }
}

fn prepared_auth(cookies: &[CookieMeta], now: i64) -> Option<PreparedChatGptAuth> {
    if classify_chatgpt(cookies, now) != InspectionResult::Ready {
        return None;
    }
    let mut prepared = Vec::<CookieMeta>::new();
    for cookie in cookies.iter().filter(|cookie| {
        chatgpt_domain(&cookie.domain)
            && cookie.scope_valid
            && safe_cookie_name(&cookie.name)
            && safe_cookie_value(&cookie.value)
            && (is_session_cookie_name(&cookie.name) || cookie.name == "oai-did")
    }) {
        if prepared.iter().any(|existing| {
            existing.domain == cookie.domain
                && existing.path == cookie.path
                && existing.name == cookie.name
        }) {
            continue;
        }
        prepared.push(cookie.clone());
    }
    Some(PreparedChatGptAuth {
        cookies: prepared,
        artifact_bytes: Vec::new(),
    })
}

fn cookie_header_for_request<F>(
    cookies: &[CookieMeta],
    host: &str,
    path: &str,
    is_https: bool,
    now: i64,
    allowed: F,
) -> Result<Option<String>, ChatGptCookieError>
where
    F: Fn(&str) -> bool,
{
    let matching = matching_cookies(cookies, host, path, is_https, now)
        .into_iter()
        .filter(|cookie| allowed(&cookie.name))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(None);
    }

    let mut required_bytes = 0usize;
    for (index, cookie) in matching.iter().enumerate() {
        required_bytes = required_bytes
            .checked_add(cookie.name.len())
            .and_then(|size| size.checked_add(1))
            .and_then(|size| size.checked_add(cookie.value.len()))
            .and_then(|size| size.checked_add(if index == 0 { 0 } else { 2 }))
            .ok_or(ChatGptCookieError::HeaderTooLarge)?;
        if required_bytes > MAX_COOKIE_HEADER_BYTES {
            return Err(ChatGptCookieError::HeaderTooLarge);
        }
    }

    let mut header = String::with_capacity(required_bytes);
    for (index, cookie) in matching.into_iter().enumerate() {
        if index > 0 {
            header.push_str("; ");
        }
        header.push_str(&cookie.name);
        header.push('=');
        header.push_str(&cookie.value);
    }
    Ok(Some(header))
}

fn matching_cookies<'a>(
    cookies: &'a [CookieMeta],
    host: &str,
    path: &str,
    is_https: bool,
    now: i64,
) -> Vec<&'a CookieMeta> {
    let mut matching = cookies
        .iter()
        .filter(|cookie| {
            cookie_matches_scope(cookie, host, path, is_https)
                && credential_state(cookie, now) == CredentialState::Ready
                && safe_cookie_name(&cookie.name)
                && safe_cookie_value(&cookie.value)
        })
        .collect::<Vec<_>>();
    // Stable sorting preserves source order for equal paths, matching browser jars.
    matching.sort_by(|left, right| right.path.len().cmp(&left.path.len()));
    matching
}

fn cookie_matches_scope(cookie: &CookieMeta, host: &str, path: &str, is_https: bool) -> bool {
    let domain_matches = if cookie.host_only {
        host == cookie.domain
    } else {
        host == cookie.domain || host.ends_with(&format!(".{}", cookie.domain))
    };
    cookie.scope_valid
        && chatgpt_domain(&cookie.domain)
        && (!cookie.secure || is_https)
        && domain_matches
        && cookie_path_matches(&cookie.path, path)
}

fn cookie_path_matches(cookie_path: &str, request_path: &str) -> bool {
    cookie_path == request_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

fn safe_cookie_name(value: &str) -> bool {
    !value.is_empty()
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b';' | b'='))
}

fn safe_cookie_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4 * 1024
        && value.starts_with('/')
        && !value.chars().any(char::is_control)
        && !value.contains(';')
}

fn parse_cookie_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn safe_cookie_value(value: &str) -> bool {
    !value
        .bytes()
        .any(|byte| matches!(byte, b'\r' | b'\n' | b';'))
}

fn credential_state(cookie: &CookieMeta, now: i64) -> CredentialState {
    if !cookie.scope_valid || !cookie.expiry_valid || !cookie.value_valid {
        CredentialState::Invalid
    } else if cookie.value.is_empty() {
        CredentialState::Empty
    } else if cookie.expires_at.is_some_and(|expires| expires <= now) {
        CredentialState::Expired
    } else {
        CredentialState::Ready
    }
}

fn session_chunk(name: &str) -> Result<Option<(&'static str, usize)>, ()> {
    let (family, suffix) =
        if let Some(suffix) = name.strip_prefix(concat!("__Secure-next-auth.session-token", ".")) {
            (SESSION_COOKIE, suffix)
        } else if let Some(suffix) = name.strip_prefix(concat!("next-auth.session-token", ".")) {
            (LEGACY_SESSION_COOKIE, suffix)
        } else {
            return Ok(None);
        };
    suffix
        .parse::<usize>()
        .map(|index| Some((family, index)))
        .map_err(|_| ())
}

fn is_session_cookie_name(name: &str) -> bool {
    name == SESSION_COOKIE
        || name == LEGACY_SESSION_COOKIE
        || session_chunk(name)
            .is_ok_and(|chunk| chunk.is_some_and(|(_, index)| index < MAX_TOKEN_CHUNKS))
}

fn normalize_domain(domain: &str) -> String {
    let domain = domain.trim().to_ascii_lowercase();
    let domain = domain
        .strip_prefix("https://")
        .or_else(|| domain.strip_prefix("http://"))
        .unwrap_or(&domain);
    let host = domain
        .split('/')
        .next()
        .unwrap_or_default()
        .trim_start_matches('.')
        .trim_end_matches('.');
    // Strip a trailing `:port` (single colon, numeric) without disturbing IPv6 literals,
    // which carry multiple colons in the host portion.
    match host.rsplit_once(':') {
        Some((head, port))
            if !head.contains(':')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head.to_string()
        }
        _ => host.to_string(),
    }
}

fn chatgpt_domain(domain: &str) -> bool {
    domain == "chatgpt.com"
        || domain.ends_with(".chatgpt.com")
        || domain == "openai.com"
        || domain.ends_with(".openai.com")
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt_client::{ChatGptPlan, ChatGptProbePool, ChatGptProbeStatus, ChatGptProber};
    use crate::module_probe::ProbeControl;
    use std::fs;

    struct NeverCancelled;

    impl ProbeControl for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn wait_cancelled(&self, _duration: std::time::Duration) -> bool {
            false
        }
    }

    const NOW: i64 = 2_000_000_000;
    const FUTURE: i64 = 2_100_000_000;
    const PAST: i64 = 1_900_000_000;
    const SYNTHETIC_TOKEN: &str = "synthetic_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn accepts_http_only_netscape_and_rejects_expired_cookie() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let ready = directory.path().join("ready.txt");
        fs::write(
            &ready,
            format!(
                "# Netscape HTTP Cookie File\n#HttpOnly_.chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}\t{SYNTHETIC_TOKEN}\n"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&ready, NOW) == InspectionResult::Ready);

        let expired = directory.path().join("expired.txt");
        fs::write(
            &expired,
            format!(".chatgpt.com\tTRUE\t/\tTRUE\t{PAST}\t{SESSION_COOKIE}\t{SYNTHETIC_TOKEN}\n"),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&expired, NOW) == InspectionResult::Expired);
    }

    #[test]
    fn accepts_json_array_object_and_contiguous_chunks() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let array = directory.path().join("array.json");
        fs::write(
            &array,
            format!(
                r#"[{{"domain":".chatgpt.com","name":"{SESSION_COOKIE}","value":"{SYNTHETIC_TOKEN}","expirationDate":{FUTURE}}}]"#
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&array, NOW) == InspectionResult::Ready);

        let object = directory.path().join("object.json");
        fs::write(
            &object,
            format!(r#"{{"sessionToken":"{SYNTHETIC_TOKEN}"}}"#),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&object, NOW) == InspectionResult::Ready);

        let chunks = directory.path().join("chunks.txt");
        fs::write(
            &chunks,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.0\tsynthetic_AAAA\n.chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.1\tBBBB\n"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&chunks, NOW) == InspectionResult::Ready);
    }

    #[test]
    fn accepts_token_only_without_exposing_it() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("token.txt");
        fs::write(&path, SYNTHETIC_TOKEN).expect("write fixture");
        let result = inspect_chatgpt_path_at(&path, NOW);
        assert!(result == InspectionResult::Ready);
        assert_eq!(std::mem::size_of_val(&result), 1);

        let bytes = fs::metadata(&path).expect("fixture metadata").len();
        let insufficient = AtomicU64::new(bytes - 1);
        assert!(
            inspect_chatgpt_path_at_with_budget(&path, NOW, Some(&insufficient))
                == InspectionResult::TooLarge
        );
        assert_eq!(insufficient.load(Ordering::Acquire), bytes - 1);

        let exact = AtomicU64::new(bytes);
        assert!(
            inspect_chatgpt_path_at_with_budget(&path, NOW, Some(&exact))
                == InspectionResult::Ready
        );
        assert_eq!(exact.load(Ordering::Acquire), 0);
    }

    #[test]
    fn rejects_missing_auth_invalid_and_oversized_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing.txt");
        fs::write(
            &missing,
            ".chatgpt.com\tTRUE\t/\tTRUE\t2100000000\tunrelated\tvalue\n",
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&missing, NOW) == InspectionResult::MissingAuth);

        let invalid = directory.path().join("invalid.txt");
        fs::write(&invalid, "not a cookie export").expect("write fixture");
        assert!(inspect_chatgpt_path_at(&invalid, NOW) == InspectionResult::Invalid);

        let large = directory.path().join("large.txt");
        fs::write(&large, vec![b'x'; MAX_ARTIFACT_BYTES as usize + 1]).expect("write fixture");
        assert!(inspect_chatgpt_path_at(&large, NOW) == InspectionResult::TooLarge);

        assert!(
            inspect_chatgpt_path_at(&directory.path().join("absent.txt"), NOW)
                == InspectionResult::Unreadable
        );
    }
    #[test]
    fn expiration_domain_and_value_types_fail_closed() {
        let directory = tempfile::tempdir().expect("temporary directory");

        let invalid_expiry = directory.path().join("invalid-expiry.txt");
        fs::write(
            &invalid_expiry,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\tnot-a-date\t{SESSION_COOKIE}\t{SYNTHETIC_TOKEN}\n"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&invalid_expiry, NOW) == InspectionResult::Invalid);

        let expired_milliseconds = directory.path().join("expired-ms.json");
        fs::write(
            &expired_milliseconds,
            format!(
                r#"[{{"domain":"chatgpt.com","name":"{SESSION_COOKIE}","value":"{SYNTHETIC_TOKEN}","expirationDate":{}}}]"#,
                PAST * 1_000
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&expired_milliseconds, NOW) == InspectionResult::Expired);

        let session_expiry = directory.path().join("session-expiry.json");
        fs::write(
            &session_expiry,
            format!(
                r#"[{{"domain":"chatgpt.com","name":"{SESSION_COOKIE}","value":"{SYNTHETIC_TOKEN}","expirationDate":-1}}]"#
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&session_expiry, NOW) == InspectionResult::Ready);

        let expired_object = directory.path().join("expired-object.json");
        fs::write(
            &expired_object,
            format!(r#"{{"token":"{SYNTHETIC_TOKEN}","expires":{PAST}}}"#),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&expired_object, NOW) == InspectionResult::Expired);

        let numeric_value = directory.path().join("numeric-value.json");
        fs::write(
            &numeric_value,
            format!(
                r#"[{{"domain":"chatgpt.com","name":"{SESSION_COOKIE}","value":123,"expires":{FUTURE}}}]"#
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&numeric_value, NOW) == InspectionResult::Invalid);

        let missing_domain = directory.path().join("missing-domain.json");
        fs::write(
            &missing_domain,
            format!(r#"[{{"name":"{SESSION_COOKIE}","value":"{SYNTHETIC_TOKEN}"}}]"#),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&missing_domain, NOW) == InspectionResult::MissingAuth);
    }

    #[test]
    fn chunks_must_be_bounded_contiguous_unique_and_nonempty() {
        let directory = tempfile::tempdir().expect("temporary directory");

        let overflow = directory.path().join("overflow.txt");
        fs::write(
            &overflow,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.0\tAAAA\n.chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.20\tBBBB\n"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&overflow, NOW) == InspectionResult::Invalid);

        let gap = directory.path().join("gap.txt");
        fs::write(
            &gap,
            format!(".chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.1\tBBBB\n"),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&gap, NOW) == InspectionResult::MissingAuth);

        let empty = directory.path().join("empty.txt");
        fs::write(
            &empty,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.0\t\n.chatgpt.com\tTRUE\t/\tTRUE\t{PAST}\t{SESSION_COOKIE}.1\tBBBB\n"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&empty, NOW) == InspectionResult::MissingAuth);

        let extra_field = directory.path().join("empty-extra-field.txt");
        fs::write(
            &extra_field,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.0\t\tignored\n.chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.1\tBBBB\n"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&extra_field, NOW) == InspectionResult::MissingAuth);

        let duplicate = directory.path().join("duplicate.txt");
        fs::write(
            &duplicate,
            format!(
                ".chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.0\tAAAA\n.chatgpt.com\tTRUE\t/\tTRUE\t{FUTURE}\t{SESSION_COOKIE}.0\tBBBB\n"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&duplicate, NOW) == InspectionResult::Invalid);
    }

    #[test]
    fn accepts_quoted_json_token_and_rejects_nonlocal_windows_paths() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let quoted = directory.path().join("quoted.json");
        fs::write(
            &quoted,
            serde_json::to_string(SYNTHETIC_TOKEN).expect("serialize fixture"),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&quoted, NOW) == InspectionResult::Ready);

        #[cfg(windows)]
        for path in [
            Path::new(r"\\example.invalid\share\artifact.txt"),
            Path::new(r"\\.\pipe\ayla-artifact"),
            Path::new(r"C:\NUL"),
            Path::new(r"C:\COM1:stream"),
        ] {
            assert!(inspect_chatgpt_path_at(path, NOW) == InspectionResult::Unreadable);
        }
    }

    #[test]
    fn rejects_json_fanout_depth_and_oversized_single_tokens_before_serde() {
        let too_many_items = format!("[{}]", vec!["{}"; MAX_COOKIES + 1].join(","));
        assert!(too_many_items.len() < MAX_ARTIFACT_BYTES as usize);
        assert!(!json_arrays_within_limits(&too_many_items));

        let member_count = MAX_JSON_STRUCTURAL_TOKENS / 4 + 1;
        let too_many_members = format!("{{{}}}", vec![r#""x":{}"#; member_count].join(","));
        assert!(too_many_members.len() < MAX_ARTIFACT_BYTES as usize);
        assert!(!json_arrays_within_limits(&too_many_members));

        let too_deep = format!(
            "{}0{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(!json_arrays_within_limits(&too_deep));

        let oversized_token = "a".repeat(MAX_TOKEN_BYTES + 1);
        assert!(token_only_value(&oversized_token).is_none());

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("fanout.json");
        fs::write(&path, too_many_items).expect("write fixture");
        assert!(inspect_chatgpt_path_at(&path, NOW) == InspectionResult::Invalid);
    }

    #[test]
    fn json_object_token_strips_embedded_session_cookie_prefix() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("object-token.json");
        fs::write(
            &path,
            format!(r#"{{"token":"{SESSION_COOKIE}={SYNTHETIC_TOKEN}"}}"#),
        )
        .expect("write fixture");

        let auth = prepare_chatgpt_path_at_with_budget(&path, NOW, None)
            .unwrap_or_else(|_| panic!("prepared auth from prefixed object token"));
        let expected = format!("{SESSION_COOKIE}={SYNTHETIC_TOKEN}");
        assert_eq!(
            auth.cookie_header_for(ChatGptEndpoint::Session, NOW)
                .expect("bounded cookie header")
                .as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn request_headers_preserve_duplicate_names_paths_host_only_and_secure_scope() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("scoped-cookies.json");
        fs::write(
            &path,
            format!(
                r#"[
                  {{"domain":".chatgpt.com","path":"/","hostOnly":false,"secure":true,"name":"{SESSION_COOKIE}","value":"root-token","expirationDate":{FUTURE}}},
                  {{"domain":".chatgpt.com","path":"/backend-api","hostOnly":false,"secure":true,"name":"{SESSION_COOKIE}","value":"backend-token","expirationDate":{FUTURE}}},
                  {{"domain":"api.openai.com","path":"/","hostOnly":true,"secure":true,"name":"{SESSION_COOKIE}","value":"wrong-host-token","expirationDate":{FUTURE}}},
                  {{"domain":"auth.chatgpt.com","path":"/","hostOnly":true,"secure":true,"name":"{SESSION_COOKIE}","value":"wrong-subdomain-token","expirationDate":{FUTURE}}},
                  {{"domain":"chatgpt.com","path":"/","hostOnly":true,"secure":true,"name":"oai-did","value":"scoped-device","expirationDate":{FUTURE}}}
                ]"#
            ),
        )
        .expect("write scoped cookie fixture");

        let auth = prepare_chatgpt_path_at_with_budget(&path, NOW, None)
            .unwrap_or_else(|_| panic!("prepare scoped auth"));
        let session = auth
            .cookie_header_for(ChatGptEndpoint::Session, NOW)
            .expect("session header")
            .expect("session cookie");
        assert_eq!(session, format!("{SESSION_COOKIE}=root-token"));

        let accounts = auth
            .cookie_header_for(ChatGptEndpoint::Accounts, NOW)
            .expect("accounts header")
            .expect("accounts cookie");
        assert_eq!(
            accounts,
            format!("{SESSION_COOKIE}=backend-token; {SESSION_COOKIE}=root-token")
        );
        assert_eq!(
            auth.device_id_for(ChatGptEndpoint::Accounts, NOW),
            Some("scoped-device")
        );

        let insecure = cookie_header_for_request(
            &auth.cookies,
            "chatgpt.com",
            "/api/auth/session",
            false,
            NOW,
            is_session_cookie_name,
        )
        .expect("bounded insecure header");
        assert_eq!(insecure, None);
    }

    #[test]
    fn host_only_cookie_never_expands_to_a_subdomain() {
        let mut cookie = CookieMeta {
            domain: "chatgpt.com".to_string(),
            path: "/".to_string(),
            secure: true,
            host_only: true,
            scope_valid: true,
            name: SESSION_COOKIE.to_string(),
            expires_at: Some(FUTURE),
            expiry_valid: true,
            value: "host-only-token".to_string(),
            value_valid: true,
        };
        assert_eq!(
            cookie_header_for_request(
                std::slice::from_ref(&cookie),
                "sub.chatgpt.com",
                "/api/auth/session",
                true,
                NOW,
                is_session_cookie_name,
            )
            .expect("host-only header"),
            None
        );

        cookie.host_only = false;
        assert_eq!(
            cookie_header_for_request(
                std::slice::from_ref(&cookie),
                "sub.chatgpt.com",
                "/api/auth/session",
                true,
                NOW,
                is_session_cookie_name,
            )
            .expect("domain-cookie header")
            .as_deref(),
            Some("__Secure-next-auth.session-token=host-only-token")
        );
    }

    #[test]
    fn bom_after_leading_whitespace_is_still_stripped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("bom.json");
        fs::write(
            &path,
            format!(
                "\n\u{feff}[{{\"domain\":\".chatgpt.com\",\"name\":\"{SESSION_COOKIE}\",\"value\":\"{SYNTHETIC_TOKEN}\",\"expirationDate\":{FUTURE}}}]"
            ),
        )
        .expect("write fixture");
        assert!(inspect_chatgpt_path_at(&path, NOW) == InspectionResult::Ready);
    }

    #[test]
    fn unsafe_direct_token_does_not_mask_expired_chunks() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("unsafe-direct.json");
        fs::write(
            &path,
            format!(
                r#"[{{"domain":"chatgpt.com","name":"{SESSION_COOKIE}","value":"real;token","expirationDate":{FUTURE}}},{{"domain":"chatgpt.com","name":"{SESSION_COOKIE}.0","value":"AAAA","expirationDate":{FUTURE}}},{{"domain":"chatgpt.com","name":"{SESSION_COOKIE}.1","value":"BBBB","expirationDate":{PAST}}}]"#
            ),
        )
        .expect("write fixture");
        // The direct token carries a ';' that prepared_auth would drop, so it must not
        // short-circuit classification to Ready while a later chunk is expired.
        assert!(inspect_chatgpt_path_at(&path, NOW) == InspectionResult::Expired);
    }

    #[test]
    fn domain_with_port_or_trailing_dot_is_accepted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for domain in [
            "chatgpt.com:443",
            ".chatgpt.com.",
            "https://chatgpt.com:443/",
        ] {
            let path = directory.path().join("domain.json");
            fs::write(
                &path,
                format!(
                    r#"[{{"domain":"{domain}","name":"{SESSION_COOKIE}","value":"{SYNTHETIC_TOKEN}","expirationDate":{FUTURE}}}]"#
                ),
            )
            .expect("write fixture");
            assert!(
                inspect_chatgpt_path_at(&path, NOW) == InspectionResult::Ready,
                "domain {domain} should classify as Ready"
            );
        }
    }

    #[test]
    fn budget_charges_exactly_the_bytes_read() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ready.json");
        fs::write(
            &path,
            format!(
                r#"[{{"domain":".chatgpt.com","name":"{SESSION_COOKIE}","value":"{SYNTHETIC_TOKEN}","expirationDate":{FUTURE}}}]"#
            ),
        )
        .expect("write fixture");
        let bytes = fs::metadata(&path).expect("fixture metadata").len();
        let budget = AtomicU64::new(bytes + 100);
        assert!(
            inspect_chatgpt_path_at_with_budget(&path, NOW, Some(&budget))
                == InspectionResult::Ready
        );
        assert_eq!(budget.load(Ordering::Acquire), 100);
    }

    #[test]
    fn failed_or_growing_reads_cannot_refund_bytes_already_consumed() {
        // The declared size was already reserved before the read. If the file grows beyond
        // the remaining allowance, every available byte stays consumed instead of refunding
        // the reservation and allowing an unbounded retry loop.
        let exhausted = AtomicU64::new(10);
        assert!(!reconcile_read_budget(Some(&exhausted), 100, 111));
        assert_eq!(exhausted.load(Ordering::Acquire), 0);

        // A partial read refunds only the portion which was reserved but never consumed.
        let partial = AtomicU64::new(25);
        assert!(reconcile_read_budget(Some(&partial), 100, 60));
        assert_eq!(partial.load(Ordering::Acquire), 65);
    }

    #[test]
    #[ignore = "requires AYLA_AUTH_EXAMPLES and explicit local authorization"]
    fn validates_authorized_external_examples_without_identifiers() {
        let Some(root) = std::env::var_os("AYLA_AUTH_EXAMPLES") else {
            return;
        };
        let mut directories = vec![std::path::PathBuf::from(root)];
        let mut visited_directories = 0usize;
        let mut counts = [0usize; 6];
        let probe = ChatGptProbePool::new(10_000, 1, &[]).expect("build direct probe");
        let remaining = AtomicU64::new(512 * 1024 * 1024);
        let mut remote_counts = [0usize; 4];
        let mut plan_counts = [0usize; 6];

        while let Some(directory) = directories.pop() {
            visited_directories += 1;
            assert!(
                visited_directories <= 100,
                "authorized directory limit exceeded"
            );
            let entries =
                fs::read_dir(directory).expect("authorized examples directory unavailable");
            for entry in entries {
                let entry = entry.expect("authorized example entry unavailable");
                let file_type = entry
                    .file_type()
                    .expect("authorized example metadata unavailable");
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    directories.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                assert!(
                    counts.iter().sum::<usize>() < 1_000,
                    "authorized file limit exceeded"
                );
                match prepare_chatgpt_path_with_budget(&entry.path(), &remaining) {
                    Ok(auth) => {
                        counts[0] += 1;
                        let result = probe.check(&auth, &NeverCancelled);
                        match result.status {
                            ChatGptProbeStatus::Active(plan) => {
                                remote_counts[0] += 1;
                                let index = match plan {
                                    ChatGptPlan::Free => 0,
                                    ChatGptPlan::Go => 1,
                                    ChatGptPlan::Plus => 2,
                                    ChatGptPlan::Pro => 3,
                                    ChatGptPlan::Team => 4,
                                    ChatGptPlan::Enterprise => 5,
                                };
                                plan_counts[index] += 1;
                            }
                            ChatGptProbeStatus::Authenticated(_) => remote_counts[0] += 1,
                            ChatGptProbeStatus::Dead => remote_counts[1] += 1,
                            ChatGptProbeStatus::RateLimited => remote_counts[2] += 1,
                            ChatGptProbeStatus::Error => remote_counts[3] += 1,
                        }
                    }
                    Err(result) => {
                        let index = match result {
                            InspectionResult::Ready => 0,
                            InspectionResult::MissingAuth => 1,
                            InspectionResult::Expired => 2,
                            InspectionResult::Invalid => 3,
                            InspectionResult::Unreadable => 4,
                            InspectionResult::TooLarge => 5,
                        };
                        counts[index] += 1;
                    }
                }
            }
        }

        let inspected: usize = counts.iter().sum();
        assert!(inspected > 0, "no regular examples were inspected");
        assert!(counts[0] > 0, "no structurally ready ChatGPT example found");
        println!(
            "authorized aggregate only: inspected={inspected}, ready={}, missing={}, expired={}, invalid={}, unreadable={}, too_large={}; remote_active={}, remote_dead={}, remote_limited={}, remote_error={}; plans_free={}, plans_go={}, plans_plus={}, plans_pro={}, plans_team={}, plans_enterprise={}",
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4],
            counts[5],
            remote_counts[0],
            remote_counts[1],
            remote_counts[2],
            remote_counts[3],
            plan_counts[0],
            plan_counts[1],
            plan_counts[2],
            plan_counts[3],
            plan_counts[4],
            plan_counts[5]
        );
    }
}
