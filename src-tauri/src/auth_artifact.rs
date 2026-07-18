use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_COOKIES: usize = 10_000;
const MAX_TOKEN_CHUNKS: usize = 20;
const MAX_JSON_DEPTH: usize = 128;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_JSON_STRUCTURAL_TOKENS: usize = 50_000;

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

pub(crate) struct PreparedChatGptAuth {
    cookie_header: String,
    device_id: Option<String>,
}

impl PreparedChatGptAuth {
    pub(crate) fn cookie_header(&self) -> &str {
        &self.cookie_header
    }

    pub(crate) fn device_id(&self) -> Option<&str> {
        self.device_id.as_deref()
    }
}

struct CookieMeta {
    domain: String,
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

pub(crate) fn prepare_chatgpt_path_with_budget(
    path: &Path,
    remaining_bytes: &AtomicU64,
) -> Result<PreparedChatGptAuth, InspectionResult> {
    prepare_chatgpt_path_at_with_budget(path, now_seconds(), Some(remaining_bytes))
}

pub(crate) fn inspect_chatgpt_path_with_budget(
    path: &Path,
    remaining_bytes: &AtomicU64,
) -> InspectionResult {
    match prepare_chatgpt_path_at_with_budget(path, now_seconds(), Some(remaining_bytes)) {
        Ok(_) => InspectionResult::Ready,
        Err(result) => result,
    }
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

fn prepare_chatgpt_path_at_with_budget(
    path: &Path,
    now: i64,
    remaining_bytes: Option<&AtomicU64>,
) -> Result<PreparedChatGptAuth, InspectionResult> {
    let data = read_limited(path, remaining_bytes)?;
    let text = match std::str::from_utf8(&data) {
        Ok(text) => text.trim_start_matches('\u{feff}').trim(),
        Err(_) => return Err(InspectionResult::Invalid),
    };
    if text.is_empty() {
        return Err(InspectionResult::Invalid);
    }

    let cookies = match parse_document(text) {
        Ok(cookies) if !cookies.is_empty() => cookies,
        _ => return Err(InspectionResult::Invalid),
    };
    let result = classify_chatgpt(&cookies, now);
    if result != InspectionResult::Ready {
        return Err(result);
    }
    prepared_auth(&cookies, now).ok_or(InspectionResult::Invalid)
}

fn read_limited(
    path: &Path,
    remaining_bytes: Option<&AtomicU64>,
) -> Result<Vec<u8>, InspectionResult> {
    if !path.is_absolute() || has_disallowed_path_prefix(path) {
        return Err(InspectionResult::Unreadable);
    }

    let file = File::open(path).map_err(|_| InspectionResult::Unreadable)?;
    let metadata = file.metadata().map_err(|_| InspectionResult::Unreadable)?;
    if !metadata.is_file() {
        return Err(InspectionResult::Unreadable);
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(InspectionResult::TooLarge);
    }
    if remaining_bytes.is_some_and(|remaining| {
        remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_sub(metadata.len())
            })
            .is_err()
    }) {
        return Err(InspectionResult::TooLarge);
    }

    let mut reader = file.take(MAX_ARTIFACT_BYTES + 1);
    let mut data = Vec::with_capacity(metadata.len().min(MAX_ARTIFACT_BYTES) as usize);
    reader
        .read_to_end(&mut data)
        .map_err(|_| InspectionResult::Unreadable)?;
    if data.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(InspectionResult::TooLarge);
    }
    Ok(data)
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
fn windows_drive_is_local(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetDriveTypeW"]
        fn get_drive_type_w(root_path_name: *const u16) -> u32;
    }

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let letter = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
        _ => return false,
    };
    let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: `root` is a valid, nul-terminated `X:\\` UTF-16 buffer.
    let drive_type = unsafe { get_drive_type_w(root.as_ptr()) };
    matches!(drive_type, 2 | 3 | 5 | 6)
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
        cookies.push(CookieMeta {
            domain: normalize_domain(parts[0]),
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
                cookies.push(CookieMeta {
                    domain: "chatgpt.com".to_string(),
                    name: SESSION_COOKIE.to_string(),
                    expires_at,
                    expiry_valid,
                    value: trimmed.to_string(),
                    value_valid: trimmed.is_empty() || token_only_value(trimmed).is_some(),
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
        target.push(CookieMeta {
            domain: normalize_domain(&item.domain),
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
    let mut chunks = [None; MAX_TOKEN_CHUNKS];
    let mut highest_chunk = None;

    for cookie in cookies {
        if !chatgpt_domain(&cookie.domain) {
            continue;
        }
        let state = credential_state(cookie, now);
        if cookie.name == SESSION_COOKIE || cookie.name == LEGACY_SESSION_COOKIE {
            direct.push(state);
            continue;
        }
        match session_chunk_index(&cookie.name) {
            Ok(Some(index)) if index < MAX_TOKEN_CHUNKS && chunks[index].is_none() => {
                chunks[index] = Some(state);
                highest_chunk =
                    Some(highest_chunk.map_or(index, |highest: usize| highest.max(index)));
            }
            Ok(Some(_)) | Err(()) => return InspectionResult::Invalid,
            Ok(None) => {}
        }
    }

    if direct.contains(&CredentialState::Ready) {
        return InspectionResult::Ready;
    }
    if let Some(highest) = highest_chunk {
        let selected = &chunks[..=highest];
        if selected.iter().any(Option::is_none) || selected.contains(&Some(CredentialState::Empty))
        {
            return InspectionResult::MissingAuth;
        }
        if selected.contains(&Some(CredentialState::Invalid)) {
            return InspectionResult::Invalid;
        }
        if selected.contains(&Some(CredentialState::Expired)) {
            return InspectionResult::Expired;
        }
        return InspectionResult::Ready;
    }
    if direct.contains(&CredentialState::Invalid) {
        InspectionResult::Invalid
    } else if direct.contains(&CredentialState::Expired) {
        InspectionResult::Expired
    } else {
        InspectionResult::MissingAuth
    }
}
fn prepared_auth(cookies: &[CookieMeta], now: i64) -> Option<PreparedChatGptAuth> {
    let mut values = BTreeMap::<String, String>::new();
    let mut secure_token = None;
    let mut legacy_token = None;
    let mut chunks = [None::<&str>; MAX_TOKEN_CHUNKS];

    for cookie in cookies {
        if !chatgpt_domain(&cookie.domain)
            || credential_state(cookie, now) != CredentialState::Ready
            || !safe_cookie_name(&cookie.name)
            || !safe_cookie_value(&cookie.value)
        {
            continue;
        }
        values.insert(cookie.name.clone(), cookie.value.clone());
        if cookie.name == SESSION_COOKIE {
            secure_token = Some(cookie.value.as_str());
        } else if cookie.name == LEGACY_SESSION_COOKIE {
            legacy_token = Some(cookie.value.as_str());
        } else if let Ok(Some(index)) = session_chunk_index(&cookie.name)
            && index < MAX_TOKEN_CHUNKS
        {
            chunks[index] = Some(cookie.value.as_str());
        }
    }

    let session_token = secure_token
        .or(legacy_token)
        .map(str::to_string)
        .or_else(|| {
            let mut joined = String::new();
            for chunk in chunks {
                match chunk {
                    Some(value) => joined.push_str(value),
                    None => break,
                }
            }
            (!joined.is_empty()).then_some(joined)
        })?;
    values.insert(SESSION_COOKIE.to_string(), session_token);

    let device_id = values.get("oai-did").cloned();
    let cookie_header = values
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    (!cookie_header.is_empty()).then_some(PreparedChatGptAuth {
        cookie_header,
        device_id,
    })
}

fn safe_cookie_name(value: &str) -> bool {
    !value.is_empty()
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b';' | b'='))
}

fn safe_cookie_value(value: &str) -> bool {
    !value
        .bytes()
        .any(|byte| matches!(byte, b'\r' | b'\n' | b';'))
}

fn credential_state(cookie: &CookieMeta, now: i64) -> CredentialState {
    if !cookie.expiry_valid || !cookie.value_valid {
        CredentialState::Invalid
    } else if cookie.value.is_empty() {
        CredentialState::Empty
    } else if cookie.expires_at.is_some_and(|expires| expires <= now) {
        CredentialState::Expired
    } else {
        CredentialState::Ready
    }
}

fn session_chunk_index(name: &str) -> Result<Option<usize>, ()> {
    let suffix = name
        .strip_prefix("__Secure-next-auth.session-token.")
        .or_else(|| name.strip_prefix("next-auth.session-token."));
    let Some(suffix) = suffix else {
        return Ok(None);
    };
    suffix.parse::<usize>().map(Some).map_err(|_| ())
}

fn normalize_domain(domain: &str) -> String {
    let domain = domain.trim().to_ascii_lowercase();
    let domain = domain
        .strip_prefix("https://")
        .or_else(|| domain.strip_prefix("http://"))
        .unwrap_or(&domain);
    domain
        .split('/')
        .next()
        .unwrap_or_default()
        .trim_start_matches('.')
        .to_string()
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
    use std::fs;

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
                        let result = probe.check(&auth);
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
