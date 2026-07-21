//! Bounded, module-scoped parsing for browser cookie exports.
//!
//! This module deliberately does not perform network I/O. It turns an owned byte
//! artifact into validated cookie metadata and only renders a `Cookie` header after
//! checking the destination host, path, scheme, and current time against that metadata.

use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const MAX_COOKIE_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_COOKIE_COUNT: usize = 4_096;
pub(crate) const MAX_COOKIE_JSON_DEPTH: usize = 32;
pub(crate) const MAX_COOKIE_HEADER_BYTES: usize = 64 * 1024;

const MAX_JSON_STRUCTURAL_TOKENS: usize = MAX_COOKIE_COUNT * 64;
const MAX_COOKIE_NAME_BYTES: usize = 256;
const MAX_COOKIE_VALUE_BYTES: usize = 64 * 1024;
const MAX_COOKIE_PATH_BYTES: usize = 4 * 1024;
const MAX_REQUEST_PATH_BYTES: usize = 16 * 1024;
const MAX_DOMAIN_BYTES: usize = 253;

/// A top-level JSON field that a particular module explicitly treats as one cookie.
///
/// This is intentionally policy data rather than a generic "every string is a cookie"
/// fallback. A module therefore cannot accidentally reinterpret unrelated JSON fields as
/// credentials.
#[derive(Clone, Copy)]
pub(crate) struct DirectCookieAlias {
    json_key: &'static str,
    cookie_name: &'static str,
    domain: &'static str,
    path: &'static str,
    secure: bool,
    http_only: bool,
    host_only: bool,
    requires_module_context: bool,
    strip_value_prefixes: &'static [&'static str],
}

impl DirectCookieAlias {
    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn new(
        json_key: &'static str,
        cookie_name: &'static str,
        domain: &'static str,
        path: &'static str,
        secure: bool,
        http_only: bool,
        host_only: bool,
        requires_module_context: bool,
        strip_value_prefixes: &'static [&'static str],
    ) -> Self {
        Self {
            json_key,
            cookie_name,
            domain,
            path,
            secure,
            http_only,
            host_only,
            requires_module_context,
            strip_value_prefixes,
        }
    }
}

/// Static security boundary for one module's cookie artifacts.
///
/// Allowed domains are DNS suffix roots. Matching always observes a label boundary, so
/// `twitch.tv.evil.invalid` and `nottwitch.tv` do not match `twitch.tv`.
#[derive(Clone, Copy)]
pub(crate) struct CookiePolicy {
    module_id: &'static str,
    default_domain: &'static str,
    default_host_only: bool,
    allowed_domains: &'static [&'static str],
    direct_aliases: &'static [DirectCookieAlias],
    required_any_cookie_names: &'static [&'static str],
    required_cookie_targets: &'static [CookieTarget],
    outbound_cookie_names: &'static [&'static str],
}

impl CookiePolicy {
    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn new(
        module_id: &'static str,
        default_domain: &'static str,
        default_host_only: bool,
        allowed_domains: &'static [&'static str],
        direct_aliases: &'static [DirectCookieAlias],
        required_any_cookie_names: &'static [&'static str],
        required_cookie_targets: &'static [CookieTarget],
        outbound_cookie_names: &'static [&'static str],
    ) -> Self {
        Self {
            module_id,
            default_domain,
            default_host_only,
            allowed_domains,
            direct_aliases,
            required_any_cookie_names,
            required_cookie_targets,
            outbound_cookie_names,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CookieTarget {
    host: &'static str,
    path: &'static str,
    is_https: bool,
}

impl CookieTarget {
    pub(crate) const fn new(host: &'static str, path: &'static str, is_https: bool) -> Self {
        Self {
            host,
            path,
            is_https,
        }
    }
}

const TWITCH_VALUE_PREFIXES: &[&str] = &["OAuth ", "Bearer "];
const TWITCH_ALLOWED_DOMAINS: &[&str] = &["twitch.tv"];
const TWITCH_REQUIRED_COOKIES: &[&str] = &["auth-token"];
const TWITCH_AUTH_TARGETS: &[CookieTarget] = &[CookieTarget::new("gql.twitch.tv", "/gql", true)];
const TWITCH_OUTBOUND_COOKIES: &[&str] = &["auth-token"];
const TWITCH_DIRECT_ALIASES: &[DirectCookieAlias] = &[
    DirectCookieAlias::new(
        "auth-token",
        "auth-token",
        "twitch.tv",
        "/",
        true,
        false,
        false,
        false,
        TWITCH_VALUE_PREFIXES,
    ),
    DirectCookieAlias::new(
        "auth_token",
        "auth-token",
        "twitch.tv",
        "/",
        true,
        false,
        false,
        true,
        TWITCH_VALUE_PREFIXES,
    ),
    DirectCookieAlias::new(
        "token",
        "auth-token",
        "twitch.tv",
        "/",
        true,
        false,
        false,
        true,
        TWITCH_VALUE_PREFIXES,
    ),
    DirectCookieAlias::new(
        "oauth",
        "auth-token",
        "twitch.tv",
        "/",
        true,
        false,
        false,
        true,
        TWITCH_VALUE_PREFIXES,
    ),
];

/// Strict Twitch policy corresponding to the explicit aliases supported by the Go input
/// parser. Alias-created cookies are scoped to the `twitch.tv` domain and never to an
/// arbitrary host supplied by the artifact.
pub(crate) const TWITCH_COOKIE_POLICY: CookiePolicy = CookiePolicy::new(
    "twitch",
    "twitch.tv",
    false,
    TWITCH_ALLOWED_DOMAINS,
    TWITCH_DIRECT_ALIASES,
    TWITCH_REQUIRED_COOKIES,
    TWITCH_AUTH_TARGETS,
    TWITCH_OUTBOUND_COOKIES,
);

const MAX_VALUE_PREFIXES: &[&str] = &["Bearer ", "Cookie "];
const MAX_ALLOWED_DOMAINS: &[&str] = &["api.hbomax.com", "api.max.com", "api.discomax.com"];
const MAX_REQUIRED_COOKIES: &[&str] = &["st"];
const MAX_AUTH_TARGETS: &[CookieTarget] = &[
    CookieTarget::new("default.any-any.prd.api.hbomax.com", "/", true),
    CookieTarget::new("default.any-any.prd.api.max.com", "/", true),
    CookieTarget::new("default.any-any.prd.api.discomax.com", "/", true),
];
const MAX_OUTBOUND_COOKIES: &[&str] = &["st"];
const MAX_DIRECT_ALIASES: &[DirectCookieAlias] = &[
    DirectCookieAlias::new(
        "st",
        "st",
        "api.hbomax.com",
        "/",
        true,
        true,
        false,
        false,
        MAX_VALUE_PREFIXES,
    ),
    DirectCookieAlias::new(
        "token",
        "st",
        "api.hbomax.com",
        "/",
        true,
        true,
        false,
        true,
        MAX_VALUE_PREFIXES,
    ),
    DirectCookieAlias::new(
        "access_token",
        "st",
        "api.hbomax.com",
        "/",
        true,
        true,
        false,
        true,
        MAX_VALUE_PREFIXES,
    ),
];

/// Strict HBO Max policy for the first-party API cookie used by the official web client.
/// Legacy roots remain explicit so an older authorized export can be recognized without
/// widening the boundary to domains that merely contain the word "max".
pub(crate) const MAX_COOKIE_POLICY: CookiePolicy = CookiePolicy::new(
    "max",
    "api.hbomax.com",
    false,
    MAX_ALLOWED_DOMAINS,
    MAX_DIRECT_ALIASES,
    MAX_REQUIRED_COOKIES,
    MAX_AUTH_TARGETS,
    MAX_OUTBOUND_COOKIES,
);

/// Validated cookie metadata. Values remain private so callers use the request-aware API
/// for outbound requests instead of assembling an unscoped header themselves.
pub(crate) struct ArtifactCookie {
    #[cfg(test)]
    domain: String,
    canonical_domain: String,
    path: String,
    secure: bool,
    http_only: bool,
    host_only: bool,
    expires_at: Option<i64>,
    name: String,
    value: String,
    source_order: usize,
}

impl ArtifactCookie {
    #[cfg(test)]
    pub(crate) fn domain(&self) -> &str {
        &self.domain
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    #[cfg(test)]
    pub(crate) const fn secure(&self) -> bool {
        self.secure
    }

    #[cfg(test)]
    pub(crate) const fn http_only(&self) -> bool {
        self.http_only
    }

    #[cfg(test)]
    pub(crate) const fn host_only(&self) -> bool {
        self.host_only
    }

    #[cfg(test)]
    pub(crate) const fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }

    #[cfg(test)]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    #[cfg(test)]
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    fn same_cookie(&self, other: &Self) -> bool {
        self.canonical_domain == other.canonical_domain
            && self.path == other.path
            && self.secure == other.secure
            && self.http_only == other.http_only
            && self.host_only == other.host_only
            && self.expires_at == other.expires_at
            && self.name == other.name
            && self.value == other.value
    }
}

/// An owned, validated artifact. `artifact_bytes` are byte-for-byte identical to the
/// input and are suitable for the existing result exporter.
pub(crate) struct PreparedCookieArtifact {
    policy: CookiePolicy,
    cookies: Vec<ArtifactCookie>,
    artifact_bytes: Vec<u8>,
}

impl PreparedCookieArtifact {
    pub(crate) const fn module_id(&self) -> &'static str {
        self.policy.module_id
    }

    #[cfg(test)]
    pub(crate) fn cookies(&self) -> &[ArtifactCookie] {
        &self.cookies
    }

    #[cfg(test)]
    pub(crate) fn artifact_bytes(&self) -> &[u8] {
        &self.artifact_bytes
    }

    pub(crate) fn into_artifact_bytes(self) -> Vec<u8> {
        self.artifact_bytes
    }

    /// Builds a header only from cookies compatible with this exact request.
    ///
    /// The destination host must first pass the module policy, which prevents a caller
    /// bug from forwarding credentials to an unrelated host. Matching cookies are
    /// ordered by longest path first, as browser cookie jars do.
    pub(crate) fn cookie_header_for(
        &self,
        request: CookieRequest<'_>,
    ) -> Result<Option<String>, CookieHeaderError> {
        let matching = self
            .matching_cookies(request)?
            .into_iter()
            .filter(|cookie| {
                self.policy
                    .outbound_cookie_names
                    .contains(&cookie.name.as_str())
            })
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
                .ok_or(CookieHeaderError::HeaderTooLarge)?;
            if required_bytes > MAX_COOKIE_HEADER_BYTES {
                return Err(CookieHeaderError::HeaderTooLarge);
            }
        }

        let mut header = String::with_capacity(required_bytes);
        for (index, cookie) in matching.into_iter().enumerate() {
            if index != 0 {
                header.push_str("; ");
            }
            header.push_str(&cookie.name);
            header.push('=');
            header.push_str(&cookie.value);
        }
        Ok(Some(header))
    }

    /// Returns one named value under the same scope checks used by header rendering.
    /// This is useful for modules such as Twitch that derive an `Authorization` header
    /// from a cookie, without bypassing domain/path/scheme/expiry validation.
    pub(crate) fn cookie_value_for<'artifact>(
        &'artifact self,
        name: &str,
        request: CookieRequest<'_>,
    ) -> Result<Option<&'artifact str>, CookieHeaderError> {
        if !safe_cookie_name(name) || !self.policy.outbound_cookie_names.contains(&name) {
            return Err(CookieHeaderError::InvalidCookieName);
        }
        Ok(self
            .matching_cookies(request)?
            .into_iter()
            .find(|cookie| cookie.name == name)
            .map(|cookie| cookie.value.as_str()))
    }

    fn matching_cookies(
        &self,
        request: CookieRequest<'_>,
    ) -> Result<Vec<&ArtifactCookie>, CookieHeaderError> {
        let host = canonical_request_host(request.host)?;
        if !domain_allowed(&host, self.policy.allowed_domains) {
            return Err(CookieHeaderError::TargetNotAllowed);
        }
        if !safe_request_path(request.path) {
            return Err(CookieHeaderError::InvalidPath);
        }

        let mut matching = self
            .cookies
            .iter()
            .filter(|cookie| {
                cookie_matches_request(
                    cookie,
                    &host,
                    request.path,
                    request.is_https,
                    request.now_unix,
                )
            })
            .collect::<Vec<_>>();
        matching.sort_by(|left, right| {
            right
                .path
                .len()
                .cmp(&left.path.len())
                .then_with(|| left.source_order.cmp(&right.source_order))
        });
        Ok(matching)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CookieRequest<'a> {
    host: &'a str,
    path: &'a str,
    is_https: bool,
    now_unix: i64,
}

impl<'a> CookieRequest<'a> {
    pub(crate) const fn new(host: &'a str, path: &'a str, is_https: bool, now_unix: i64) -> Self {
        Self {
            host,
            path,
            is_https,
            now_unix,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CookieArtifactError {
    Empty,
    TooLarge,
    NotUtf8,
    InvalidPolicy,
    UnsupportedFormat,
    InvalidJson,
    JsonTooDeep,
    JsonTooComplex,
    TooManyCookies,
    InvalidCookieShape,
    AmbiguousField,
    InvalidBoolean,
    InvalidExpiry,
    ExpiredCookie,
    InvalidDomain,
    InvalidPath,
    UnsafeCookieName,
    UnsafeCookieValue,
    AmbiguousCookie,
    NoCookies,
    NoAllowedCookies,
    MissingRequiredCookie,
    SystemClock,
}

impl fmt::Display for CookieArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "cookie artifact is empty",
            Self::TooLarge => "cookie artifact exceeds the byte limit",
            Self::NotUtf8 => "cookie artifact is not UTF-8",
            Self::InvalidPolicy => "cookie module policy is invalid",
            Self::UnsupportedFormat => "cookie artifact format is unsupported",
            Self::InvalidJson => "cookie JSON is malformed",
            Self::JsonTooDeep => "cookie JSON exceeds the nesting limit",
            Self::JsonTooComplex => "cookie JSON exceeds the structural limit",
            Self::TooManyCookies => "cookie artifact exceeds the cookie limit",
            Self::InvalidCookieShape => "cookie entry has an invalid shape",
            Self::AmbiguousField => "cookie entry contains ambiguous aliases",
            Self::InvalidBoolean => "cookie entry contains an invalid boolean",
            Self::InvalidExpiry => "cookie entry contains an invalid expiry",
            Self::ExpiredCookie => {
                "cookie artifact contains expired required auth and no valid replacement"
            }
            Self::InvalidDomain => "cookie entry contains an invalid domain",
            Self::InvalidPath => "cookie entry contains an invalid path",
            Self::UnsafeCookieName => "cookie entry contains an unsafe name",
            Self::UnsafeCookieValue => "cookie entry contains an unsafe value",
            Self::AmbiguousCookie => "cookie artifact contains conflicting duplicates",
            Self::NoCookies => "cookie artifact contains no cookie entries",
            Self::NoAllowedCookies => "cookie artifact contains no cookies allowed by the module",
            Self::MissingRequiredCookie => "cookie artifact is missing required module auth",
            Self::SystemClock => "system time is unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CookieArtifactError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CookieHeaderError {
    InvalidHost,
    TargetNotAllowed,
    InvalidPath,
    InvalidCookieName,
    HeaderTooLarge,
}

impl fmt::Display for CookieHeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidHost => "cookie request host is invalid",
            Self::TargetNotAllowed => "cookie request host is outside the module policy",
            Self::InvalidPath => "cookie request path is invalid",
            Self::InvalidCookieName => "requested cookie name is invalid",
            Self::HeaderTooLarge => "rendered Cookie header exceeds the byte limit",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CookieHeaderError {}

/// Parses at the current system time. The owned input is retained without normalization
/// so result export can reproduce the source artifact exactly.
pub(crate) fn prepare_cookie_artifact(
    artifact_bytes: Vec<u8>,
    policy: CookiePolicy,
) -> Result<PreparedCookieArtifact, CookieArtifactError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CookieArtifactError::SystemClock)?;
    let now = i64::try_from(now.as_secs()).map_err(|_| CookieArtifactError::SystemClock)?;
    prepare_cookie_artifact_at(artifact_bytes, policy, now)
}

/// Time-injected entry point for deterministic task-engine classification and tests.
pub(crate) fn prepare_cookie_artifact_at(
    artifact_bytes: Vec<u8>,
    policy: CookiePolicy,
    now_unix: i64,
) -> Result<PreparedCookieArtifact, CookieArtifactError> {
    validate_policy(policy)?;
    if artifact_bytes.len() > MAX_COOKIE_ARTIFACT_BYTES {
        return Err(CookieArtifactError::TooLarge);
    }
    let text = std::str::from_utf8(&artifact_bytes).map_err(|_| CookieArtifactError::NotUtf8)?;
    let text = text.trim();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text).trim();
    if text.is_empty() {
        return Err(CookieArtifactError::Empty);
    }

    let mut accumulator = CookieAccumulator::default();
    match text.as_bytes().first() {
        Some(b'[' | b'{') => parse_json(text, policy, now_unix, &mut accumulator)?,
        _ => parse_netscape(text, policy, now_unix, &mut accumulator)?,
    }
    accumulator.finish(policy, artifact_bytes, now_unix)
}

#[derive(Default)]
struct CookieAccumulator {
    candidates: usize,
    allowed_candidates: usize,
    expired_required_cookie: bool,
    cookies: Vec<ArtifactCookie>,
    identities: BTreeMap<(String, String, String), usize>,
}

impl CookieAccumulator {
    fn note_candidate(&mut self) -> Result<usize, CookieArtifactError> {
        self.candidates = self
            .candidates
            .checked_add(1)
            .ok_or(CookieArtifactError::TooManyCookies)?;
        if self.candidates > MAX_COOKIE_COUNT {
            return Err(CookieArtifactError::TooManyCookies);
        }
        Ok(self.candidates - 1)
    }

    fn insert(
        &mut self,
        candidate: CookieCandidate,
        policy: CookiePolicy,
        now_unix: i64,
    ) -> Result<(), CookieArtifactError> {
        let source_order = self.note_candidate()?;
        let original_domain = candidate.domain.trim();
        let canonical_domain = canonical_cookie_domain(original_domain)?;

        // A multi-site browser export is allowed as input, but only cookies inside this
        // module's DNS boundary survive. No cross-domain cookie can satisfy required auth.
        if !domain_allowed(&canonical_domain, policy.allowed_domains) {
            return Ok(());
        }
        self.allowed_candidates = self.allowed_candidates.saturating_add(1);
        if !safe_cookie_path(&candidate.path) {
            return Err(CookieArtifactError::InvalidPath);
        }
        if !safe_cookie_name(&candidate.name) {
            return Err(CookieArtifactError::UnsafeCookieName);
        }
        // Browser exports commonly retain deletion tombstones. They are not credentials
        // and must not satisfy required auth, but neither should they poison an otherwise
        // usable export.
        if is_deleted_cookie_value(&candidate.value) {
            return Ok(());
        }
        if !safe_cookie_value(&candidate.value) {
            return Err(CookieArtifactError::UnsafeCookieValue);
        }
        if candidate
            .expires_at
            .is_some_and(|expiry| expiry <= now_unix)
        {
            if policy
                .required_any_cookie_names
                .contains(&candidate.name.as_str())
            {
                self.expired_required_cookie = true;
            }
            return Ok(());
        }

        let cookie = ArtifactCookie {
            #[cfg(test)]
            domain: original_domain.to_string(),
            canonical_domain: canonical_domain.clone(),
            path: candidate.path,
            secure: candidate.secure,
            http_only: candidate.http_only,
            host_only: candidate.host_only,
            expires_at: candidate.expires_at,
            name: candidate.name,
            value: candidate.value,
            source_order,
        };
        let identity = (canonical_domain, cookie.path.clone(), cookie.name.clone());
        if let Some(existing) = self.identities.get(&identity).copied() {
            if self.cookies[existing].same_cookie(&cookie) {
                return Ok(());
            }
            return Err(CookieArtifactError::AmbiguousCookie);
        }
        self.identities.insert(identity, self.cookies.len());
        self.cookies.push(cookie);
        Ok(())
    }

    fn finish(
        self,
        policy: CookiePolicy,
        artifact_bytes: Vec<u8>,
        now_unix: i64,
    ) -> Result<PreparedCookieArtifact, CookieArtifactError> {
        if self.candidates == 0 {
            return Err(CookieArtifactError::NoCookies);
        }
        if self.allowed_candidates == 0 {
            return Err(CookieArtifactError::NoAllowedCookies);
        }
        let has_required_cookie = self.cookies.iter().any(|cookie| {
            !cookie.value.is_empty()
                && policy
                    .required_any_cookie_names
                    .contains(&cookie.name.as_str())
                && (policy.required_cookie_targets.is_empty()
                    || policy.required_cookie_targets.iter().any(|target| {
                        canonical_request_host(target.host).is_ok_and(|host| {
                            cookie_matches_request(
                                cookie,
                                &host,
                                target.path,
                                target.is_https,
                                now_unix,
                            )
                        })
                    }))
        });
        if !policy.required_any_cookie_names.is_empty() && !has_required_cookie {
            return if self.expired_required_cookie {
                Err(CookieArtifactError::ExpiredCookie)
            } else {
                Err(CookieArtifactError::MissingRequiredCookie)
            };
        }
        if self.cookies.is_empty() {
            return Err(CookieArtifactError::NoAllowedCookies);
        }
        Ok(PreparedCookieArtifact {
            policy,
            cookies: self.cookies,
            artifact_bytes,
        })
    }
}

struct CookieCandidate {
    domain: String,
    path: String,
    secure: bool,
    http_only: bool,
    host_only: bool,
    expires_at: Option<i64>,
    name: String,
    value: String,
}

fn validate_policy(policy: CookiePolicy) -> Result<(), CookieArtifactError> {
    if policy.module_id.is_empty()
        || policy.allowed_domains.is_empty()
        || policy.module_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CookieArtifactError::InvalidPolicy);
    }

    let default_domain = canonical_cookie_domain(policy.default_domain)
        .map_err(|_| CookieArtifactError::InvalidPolicy)?;
    for domain in policy.allowed_domains {
        canonical_cookie_domain(domain).map_err(|_| CookieArtifactError::InvalidPolicy)?;
    }
    if !domain_allowed(&default_domain, policy.allowed_domains) {
        return Err(CookieArtifactError::InvalidPolicy);
    }
    for required in policy.required_any_cookie_names {
        if !safe_cookie_name(required) {
            return Err(CookieArtifactError::InvalidPolicy);
        }
    }
    for outbound in policy.outbound_cookie_names {
        if !safe_cookie_name(outbound) {
            return Err(CookieArtifactError::InvalidPolicy);
        }
    }
    if policy
        .required_any_cookie_names
        .iter()
        .any(|required| !policy.outbound_cookie_names.contains(required))
    {
        return Err(CookieArtifactError::InvalidPolicy);
    }
    for target in policy.required_cookie_targets {
        let host =
            canonical_request_host(target.host).map_err(|_| CookieArtifactError::InvalidPolicy)?;
        if !domain_allowed(&host, policy.allowed_domains) || !safe_request_path(target.path) {
            return Err(CookieArtifactError::InvalidPolicy);
        }
    }
    for alias in policy.direct_aliases {
        if alias.json_key.is_empty()
            || alias.json_key.bytes().any(|byte| byte.is_ascii_control())
            || !safe_cookie_name(alias.cookie_name)
            || !safe_cookie_path(alias.path)
            || alias
                .strip_value_prefixes
                .iter()
                .any(|prefix| prefix.bytes().any(|byte| byte.is_ascii_control()))
        {
            return Err(CookieArtifactError::InvalidPolicy);
        }
        let domain = canonical_cookie_domain(alias.domain)
            .map_err(|_| CookieArtifactError::InvalidPolicy)?;
        if !domain_allowed(&domain, policy.allowed_domains) {
            return Err(CookieArtifactError::InvalidPolicy);
        }
    }
    Ok(())
}

fn parse_netscape(
    text: &str,
    policy: CookiePolicy,
    now_unix: i64,
    accumulator: &mut CookieAccumulator,
) -> Result<(), CookieArtifactError> {
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() {
            continue;
        }
        let (line, http_only) = if let Some(cookie) = line.strip_prefix("#HttpOnly_") {
            (cookie, true)
        } else if line.starts_with('#') {
            continue;
        } else {
            (line, false)
        };

        let parts = netscape_fields(line)?;
        let include_subdomains = parse_bool_text(parts[1])?;
        let secure = parse_bool_text(parts[3])?;
        let expires_at = parse_expiry_text(parts[4])?;
        let path = parts[2].trim();
        let name = parts[5].trim();
        let value = parts[6];
        accumulator.insert(
            CookieCandidate {
                domain: parts[0].trim().to_string(),
                path: if path.is_empty() { "/" } else { path }.to_string(),
                secure,
                http_only,
                host_only: !include_subdomains,
                expires_at,
                name: name.to_string(),
                value: value.to_string(),
            },
            policy,
            now_unix,
        )?;
    }
    Ok(())
}

fn netscape_fields(line: &str) -> Result<[&str; 7], CookieArtifactError> {
    let fields = if line.contains('\t') {
        line.splitn(7, '\t').collect::<Vec<_>>()
    } else {
        line.split_ascii_whitespace().collect::<Vec<_>>()
    };
    fields
        .try_into()
        .map_err(|_| CookieArtifactError::UnsupportedFormat)
}

fn parse_json(
    text: &str,
    policy: CookiePolicy,
    now_unix: i64,
    accumulator: &mut CookieAccumulator,
) -> Result<(), CookieArtifactError> {
    validate_json_limits(text)?;
    let document: Value =
        serde_json::from_str(text).map_err(|_| CookieArtifactError::InvalidJson)?;
    match document {
        Value::Array(cookies) => parse_json_cookie_array(&cookies, policy, now_unix, accumulator)?,
        Value::Object(object) => parse_json_object(&object, policy, now_unix, accumulator)?,
        _ => return Err(CookieArtifactError::UnsupportedFormat),
    }
    Ok(())
}

fn parse_json_object(
    object: &Map<String, Value>,
    policy: CookiePolicy,
    now_unix: i64,
    accumulator: &mut CookieAccumulator,
) -> Result<(), CookieArtifactError> {
    parse_json_object_with_context(object, policy, now_unix, accumulator, false)
}

fn parse_json_object_with_context(
    object: &Map<String, Value>,
    policy: CookiePolicy,
    now_unix: i64,
    accumulator: &mut CookieAccumulator,
    inherited_module_context: bool,
) -> Result<(), CookieArtifactError> {
    let mut recognized = false;
    let module_context = inherited_module_context || has_module_context(object, policy.module_id);

    if let Some(nested) = object.get(policy.module_id) {
        recognized = true;
        let nested = nested
            .as_object()
            .ok_or(CookieArtifactError::InvalidCookieShape)?;
        parse_json_object_with_context(nested, policy, now_unix, accumulator, true)?;
    }
    if let Some(cookies) = object.get("cookies") {
        recognized = true;
        let cookies = cookies
            .as_array()
            .ok_or(CookieArtifactError::InvalidCookieShape)?;
        parse_json_cookie_array(cookies, policy, now_unix, accumulator)?;
    }

    if contains_any(object, NAME_FIELDS) {
        recognized = true;
        parse_json_cookie(object, policy, now_unix, accumulator)?;
    }

    for alias in policy.direct_aliases {
        if alias.requires_module_context && !module_context {
            continue;
        }
        let Some(raw_value) = object.get(alias.json_key) else {
            continue;
        };
        recognized = true;
        let raw_value = raw_value
            .as_str()
            .ok_or(CookieArtifactError::InvalidCookieShape)?;
        let mut value = raw_value.trim();
        if let Some(prefix) = alias
            .strip_value_prefixes
            .iter()
            .find(|prefix| value.starts_with(**prefix))
        {
            value = value[prefix.len()..].trim();
        }
        accumulator.insert(
            CookieCandidate {
                domain: alias.domain.to_string(),
                path: alias.path.to_string(),
                secure: alias.secure,
                http_only: alias.http_only,
                host_only: alias.host_only,
                expires_at: None,
                name: alias.cookie_name.to_string(),
                value: value.to_string(),
            },
            policy,
            now_unix,
        )?;
    }

    if recognized {
        Ok(())
    } else {
        Err(CookieArtifactError::UnsupportedFormat)
    }
}

fn has_module_context(object: &Map<String, Value>, module_id: &str) -> bool {
    ["module", "service", "provider"].iter().any(|field| {
        object
            .get(*field)
            .and_then(Value::as_str)
            .is_some_and(|value| value.trim().eq_ignore_ascii_case(module_id))
    })
}

fn parse_json_cookie_array(
    cookies: &[Value],
    policy: CookiePolicy,
    now_unix: i64,
    accumulator: &mut CookieAccumulator,
) -> Result<(), CookieArtifactError> {
    if cookies.len() > MAX_COOKIE_COUNT {
        return Err(CookieArtifactError::TooManyCookies);
    }
    for cookie in cookies {
        let object = cookie
            .as_object()
            .ok_or(CookieArtifactError::InvalidCookieShape)?;
        parse_json_cookie(object, policy, now_unix, accumulator)?;
    }
    Ok(())
}

const NAME_FIELDS: &[&str] = &["name", "Name", "Name raw", "key"];
const VALUE_FIELDS: &[&str] = &["value", "Value", "Content raw", "content"];
const DOMAIN_FIELDS: &[&str] = &["domain", "Domain", "Host raw", "host"];
const PATH_FIELDS: &[&str] = &["path", "Path", "Path raw"];
const SECURE_FIELDS: &[&str] = &["secure", "Secure"];
const HTTP_ONLY_FIELDS: &[&str] = &["httpOnly", "HttpOnly", "http_only"];
const HOST_ONLY_FIELDS: &[&str] = &["hostOnly", "HostOnly", "host_only"];
const EXPIRY_FIELDS: &[&str] = &[
    "expirationDate",
    "expiry",
    "expires",
    "expiresAt",
    "expires_at",
    "Expires raw",
    "Expiration Date",
];

fn parse_json_cookie(
    object: &Map<String, Value>,
    policy: CookiePolicy,
    now_unix: i64,
    accumulator: &mut CookieAccumulator,
) -> Result<(), CookieArtifactError> {
    let name = unique_field(object, NAME_FIELDS)?
        .and_then(Value::as_str)
        .ok_or(CookieArtifactError::InvalidCookieShape)?;
    let value = unique_field(object, VALUE_FIELDS)?
        .and_then(Value::as_str)
        .ok_or(CookieArtifactError::InvalidCookieShape)?;

    let domain_field = unique_field(object, DOMAIN_FIELDS)?;
    let domain = match domain_field {
        None | Some(Value::Null) => policy.default_domain,
        Some(value) => value
            .as_str()
            .ok_or(CookieArtifactError::InvalidCookieShape)?,
    };
    let path = match unique_field(object, PATH_FIELDS)? {
        None | Some(Value::Null) => "/",
        Some(value) => value
            .as_str()
            .ok_or(CookieArtifactError::InvalidCookieShape)?,
    };
    let secure = parse_optional_json_bool(unique_field(object, SECURE_FIELDS)?)?.unwrap_or(false);
    let http_only =
        parse_optional_json_bool(unique_field(object, HTTP_ONLY_FIELDS)?)?.unwrap_or(false);
    let host_only = match parse_optional_json_bool(unique_field(object, HOST_ONLY_FIELDS)?)? {
        Some(host_only) => host_only,
        None if domain_field.is_none() || matches!(domain_field, Some(Value::Null)) => {
            policy.default_host_only
        }
        None => !domain.trim().starts_with('.'),
    };
    let expires_at = parse_optional_json_expiry(unique_field(object, EXPIRY_FIELDS)?)?;

    accumulator.insert(
        CookieCandidate {
            domain: domain.to_string(),
            path: path.to_string(),
            secure,
            http_only,
            host_only,
            expires_at,
            name: name.to_string(),
            value: value.to_string(),
        },
        policy,
        now_unix,
    )
}

fn contains_any(object: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| object.contains_key(*key))
}

fn unique_field<'a>(
    object: &'a Map<String, Value>,
    keys: &[&str],
) -> Result<Option<&'a Value>, CookieArtifactError> {
    let mut found = None;
    for key in keys {
        if let Some(value) = object.get(*key) {
            if found.is_some() {
                return Err(CookieArtifactError::AmbiguousField);
            }
            found = Some(value);
        }
    }
    Ok(found)
}

fn parse_optional_json_bool(value: Option<&Value>) -> Result<Option<bool>, CookieArtifactError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::String(value)) => parse_bool_text(value).map(Some),
        Some(Value::Number(value)) if value.as_i64() == Some(0) => Ok(Some(false)),
        Some(Value::Number(value)) if value.as_i64() == Some(1) => Ok(Some(true)),
        _ => Err(CookieArtifactError::InvalidBoolean),
    }
}

fn parse_bool_text(value: &str) -> Result<bool, CookieArtifactError> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("true") || value == "1" {
        Ok(true)
    } else if value.eq_ignore_ascii_case("false") || value == "0" {
        Ok(false)
    } else {
        Err(CookieArtifactError::InvalidBoolean)
    }
}

fn parse_optional_json_expiry(value: Option<&Value>) -> Result<Option<i64>, CookieArtifactError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => parse_expiry_text(value),
        Some(Value::Number(value)) => value
            .as_f64()
            .ok_or(CookieArtifactError::InvalidExpiry)
            .and_then(normalize_expiry),
        _ => Err(CookieArtifactError::InvalidExpiry),
    }
}

fn parse_expiry_text(value: &str) -> Result<Option<i64>, CookieArtifactError> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|_| CookieArtifactError::InvalidExpiry)
        .and_then(normalize_expiry)
}

fn normalize_expiry(mut value: f64) -> Result<Option<i64>, CookieArtifactError> {
    if value == -1.0 || value == 0.0 {
        return Ok(None);
    }
    if !value.is_finite() || !(1.0..=100_000_000_000_000_000_000.0).contains(&value) {
        return Err(CookieArtifactError::InvalidExpiry);
    }
    // Browser exporters variously emit seconds, milliseconds, microseconds, or
    // nanoseconds. Bring those representations back into Unix seconds without allowing
    // unbounded magnitudes.
    while value >= 100_000_000_000.0 {
        value /= 1_000.0;
    }
    if value < 1.0 || value > i64::MAX as f64 {
        return Err(CookieArtifactError::InvalidExpiry);
    }
    Ok(Some(value.floor() as i64))
}

fn validate_json_limits(text: &str) -> Result<(), CookieArtifactError> {
    let mut stack = [0u8; MAX_COOKIE_JSON_DEPTH];
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

        if matches!(byte, b'[' | b']' | b'{' | b'}' | b',' | b':') {
            structural_tokens = structural_tokens.saturating_add(1);
            if structural_tokens > MAX_JSON_STRUCTURAL_TOKENS {
                return Err(CookieArtifactError::JsonTooComplex);
            }
        }

        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                if depth == MAX_COOKIE_JSON_DEPTH {
                    return Err(CookieArtifactError::JsonTooDeep);
                }
                stack[depth] = byte;
                depth += 1;
            }
            b']' | b'}' => {
                if depth == 0 {
                    return Err(CookieArtifactError::InvalidJson);
                }
                let expected = if byte == b']' { b'[' } else { b'{' };
                if stack[depth - 1] != expected {
                    return Err(CookieArtifactError::InvalidJson);
                }
                depth -= 1;
            }
            _ => {}
        }
    }

    if depth != 0 || in_string || escaped {
        Err(CookieArtifactError::InvalidJson)
    } else {
        Ok(())
    }
}

fn canonical_cookie_domain(domain: &str) -> Result<String, CookieArtifactError> {
    canonical_domain(domain, false).map_err(|_| CookieArtifactError::InvalidDomain)
}

fn canonical_request_host(host: &str) -> Result<String, CookieHeaderError> {
    canonical_domain(host, true).map_err(|_| CookieHeaderError::InvalidHost)
}

fn canonical_domain(domain: &str, allow_trailing_dot: bool) -> Result<String, ()> {
    let domain = domain.trim();
    let domain = domain.strip_prefix('.').unwrap_or(domain);
    let domain = if allow_trailing_dot {
        domain.strip_suffix('.').unwrap_or(domain)
    } else {
        domain
    };
    if domain.is_empty()
        || domain.len() > MAX_DOMAIN_BYTES
        || !domain.is_ascii()
        || domain.contains(['/', '\\', ':'])
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err(());
    }
    for label in domain.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(());
        }
    }
    Ok(domain.to_ascii_lowercase())
}

fn domain_allowed(domain: &str, allowlist: &[&str]) -> bool {
    allowlist.iter().any(|allowed| {
        canonical_domain(allowed, false)
            .is_ok_and(|allowed| domain == allowed || domain.ends_with(&format!(".{allowed}")))
    })
}

fn cookie_domain_matches(cookie: &ArtifactCookie, host: &str) -> bool {
    if cookie.host_only {
        host == cookie.canonical_domain
    } else {
        host == cookie.canonical_domain || host.ends_with(&format!(".{}", cookie.canonical_domain))
    }
}

fn cookie_matches_request(
    cookie: &ArtifactCookie,
    host: &str,
    path: &str,
    is_https: bool,
    now_unix: i64,
) -> bool {
    !cookie.expires_at.is_some_and(|expiry| expiry <= now_unix)
        && (!cookie.secure || is_https)
        && cookie_domain_matches(cookie, host)
        && cookie_path_matches(&cookie.path, path)
}

fn cookie_path_matches(cookie_path: &str, request_path: &str) -> bool {
    if cookie_path == request_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

fn safe_cookie_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_COOKIE_PATH_BYTES
        && path.starts_with('/')
        && !path.chars().any(char::is_control)
        && !path.contains(';')
}

fn safe_request_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_REQUEST_PATH_BYTES
        && path.starts_with('/')
        && !path.chars().any(char::is_control)
        && !path.contains(['?', '#'])
}

fn safe_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_COOKIE_NAME_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii()
                && byte > 0x20
                && byte < 0x7f
                && !matches!(
                    byte,
                    b'(' | b')'
                        | b'<'
                        | b'>'
                        | b'@'
                        | b','
                        | b';'
                        | b':'
                        | b'\\'
                        | b'"'
                        | b'/'
                        | b'['
                        | b']'
                        | b'?'
                        | b'='
                        | b'{'
                        | b'}'
                )
        })
}

fn safe_cookie_value(value: &str) -> bool {
    value.len() <= MAX_COOKIE_VALUE_BYTES
        && value.bytes().all(|byte| {
            matches!(
                byte,
                0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e
            )
        })
}

fn is_deleted_cookie_value(value: &str) -> bool {
    let value = value.trim();
    value.eq_ignore_ascii_case("deleted")
        || value.eq_ignore_ascii_case("delete")
        || value.eq_ignore_ascii_case("null")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 2_000_000_000;
    const FUTURE: i64 = NOW + 86_400;
    const SYNTHETIC_TOKEN: &str = "synthetic-twitch-token-0001";
    const SYNTHETIC_MAX_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJhbm9ueW1vdXMiOmZhbHNlfQ.signature";

    fn prepare(bytes: Vec<u8>) -> Result<PreparedCookieArtifact, CookieArtifactError> {
        prepare_cookie_artifact_at(bytes, TWITCH_COOKIE_POLICY, NOW)
    }

    fn prepare_max(bytes: Vec<u8>) -> Result<PreparedCookieArtifact, CookieArtifactError> {
        prepare_cookie_artifact_at(bytes, MAX_COOKIE_POLICY, NOW)
    }

    #[test]
    fn max_cookie_is_scoped_to_explicit_first_party_api_hosts() {
        let bytes =
            format!(".api.hbomax.com\tTRUE\t/\tTRUE\t{FUTURE}\tst\t{SYNTHETIC_MAX_TOKEN}\n")
                .into_bytes();
        let artifact = prepare_max(bytes.clone()).expect("prepare synthetic HBO Max artifact");

        assert_eq!(artifact.module_id(), "max");
        assert_eq!(artifact.artifact_bytes(), bytes);
        assert_eq!(
            artifact
                .cookie_header_for(CookieRequest::new(
                    "default.beam-emea.prd.api.hbomax.com",
                    "/users/me",
                    true,
                    NOW,
                ))
                .expect("allowed HBO Max API target"),
            Some(format!("st={SYNTHETIC_MAX_TOKEN}"))
        );
        assert!(matches!(
            artifact.cookie_header_for(CookieRequest::new(
                "api.hbomax.com.evil.invalid",
                "/users/me",
                true,
                NOW,
            )),
            Err(CookieHeaderError::TargetNotAllowed)
        ));
    }

    #[test]
    fn max_direct_token_alias_requires_module_context() {
        let unrelated = format!(r#"{{"token":"{SYNTHETIC_MAX_TOKEN}"}}"#).into_bytes();
        assert!(matches!(
            prepare_max(unrelated),
            Err(CookieArtifactError::UnsupportedFormat)
        ));

        let scoped =
            format!(r#"{{"module":"max","token":"Bearer {SYNTHETIC_MAX_TOKEN}"}}"#).into_bytes();
        let artifact = prepare_max(scoped).expect("prepare scoped HBO Max alias");
        assert_eq!(artifact.cookies()[0].name(), "st");
        assert_eq!(artifact.cookies()[0].value(), SYNTHETIC_MAX_TOKEN);
    }

    #[test]
    fn parses_http_only_netscape_and_preserves_original_bytes() {
        let bytes = format!(
            "# Netscape HTTP Cookie File\r\n#HttpOnly_.twitch.tv\tTRUE\t/\tTRUE\t{FUTURE}\tauth-token\t{SYNTHETIC_TOKEN}\r\n"
        )
        .into_bytes();
        let artifact = prepare(bytes.clone()).expect("prepare synthetic Twitch artifact");

        assert_eq!(artifact.module_id(), "twitch");
        assert_eq!(artifact.artifact_bytes(), bytes);
        assert_eq!(artifact.cookies().len(), 1);
        let cookie = &artifact.cookies()[0];
        assert_eq!(cookie.domain(), ".twitch.tv");
        assert_eq!(cookie.path(), "/");
        assert!(cookie.secure());
        assert!(cookie.http_only());
        assert!(!cookie.host_only());
        assert_eq!(cookie.expires_at(), Some(FUTURE));
        assert_eq!(cookie.name(), "auth-token");
        assert_eq!(cookie.value(), SYNTHETIC_TOKEN);

        let request = CookieRequest::new("gql.twitch.tv", "/", true, NOW);
        assert_eq!(
            artifact.cookie_header_for(request).expect("safe header"),
            Some(format!("auth-token={SYNTHETIC_TOKEN}"))
        );
        assert_eq!(
            artifact
                .cookie_value_for("auth-token", request)
                .expect("scoped value"),
            Some(SYNTHETIC_TOKEN)
        );
    }

    #[test]
    fn direct_twitch_alias_is_explicit_and_strips_known_authorization_prefix() {
        let bytes = br#"{"module":"twitch","oauth":"OAuth synthetic-twitch-token-0001"}"#.to_vec();
        let artifact = prepare(bytes.clone()).expect("prepare direct alias");
        assert_eq!(artifact.artifact_bytes(), bytes);
        assert_eq!(artifact.cookies()[0].name(), "auth-token");
        assert_eq!(artifact.cookies()[0].value(), SYNTHETIC_TOKEN);
        assert!(artifact.cookies()[0].secure());
        assert!(!artifact.cookies()[0].host_only());
    }

    #[test]
    fn generic_token_alias_requires_explicit_twitch_context() {
        for alias in ["token", "auth_token", "oauth"] {
            let bytes = format!(r#"{{"{alias}":"credential-from-an-unrelated-service"}}"#);
            assert!(matches!(
                prepare(bytes.into_bytes()),
                Err(CookieArtifactError::UnsupportedFormat)
            ));
        }

        prepare(br#"{"auth-token":"synthetic-twitch-token-0001"}"#.to_vec())
            .expect("the Twitch-specific cookie name remains directly supported");

        let artifact =
            prepare(br#"{"twitch":{"token":"Bearer synthetic-twitch-token-0001"}}"#.to_vec())
                .expect("prepare namespaced Twitch token");
        assert_eq!(artifact.cookies()[0].value(), SYNTHETIC_TOKEN);
    }

    #[test]
    fn json_preserves_scope_flags_and_expiry() {
        let bytes = format!(
            r#"{{"cookies":[{{"Name":"auth-token","Value":"{SYNTHETIC_TOKEN}","Domain":"gql.twitch.tv","Path":"/gql","Secure":false,"httpOnly":true,"hostOnly":true,"expirationDate":{FUTURE}}}]}}"#
        )
        .into_bytes();
        let artifact = prepare(bytes).expect("prepare JSON array wrapper");
        let cookie = &artifact.cookies()[0];
        assert_eq!(cookie.domain(), "gql.twitch.tv");
        assert_eq!(cookie.path(), "/gql");
        assert!(!cookie.secure());
        assert!(cookie.http_only());
        assert!(cookie.host_only());
        assert_eq!(cookie.expires_at(), Some(FUTURE));

        assert!(
            artifact
                .cookie_header_for(CookieRequest::new(
                    "gql.twitch.tv",
                    "/gql/channel",
                    false,
                    NOW,
                ))
                .expect("path match")
                .is_some()
        );
        assert_eq!(
            artifact
                .cookie_header_for(CookieRequest::new(
                    "api.gql.twitch.tv",
                    "/gql/channel",
                    true,
                    NOW,
                ))
                .expect("host-only mismatch"),
            None
        );
        assert_eq!(
            artifact
                .cookie_header_for(CookieRequest::new("gql.twitch.tv", "/gqlx", true, NOW,))
                .expect("path boundary mismatch"),
            None
        );
    }

    #[test]
    fn required_auth_must_be_usable_for_the_module_endpoint() {
        for bytes in [
            format!("help.twitch.tv\tFALSE\t/\tTRUE\t{FUTURE}\tauth-token\t{SYNTHETIC_TOKEN}\n"),
            format!(".twitch.tv\tTRUE\t/settings\tTRUE\t{FUTURE}\tauth-token\t{SYNTHETIC_TOKEN}\n"),
        ] {
            assert!(matches!(
                prepare(bytes.into_bytes()),
                Err(CookieArtifactError::MissingRequiredCookie)
            ));
        }
    }

    #[test]
    fn outbound_header_only_contains_policy_allowlisted_cookies() {
        let bytes = format!(
            ".twitch.tv\tTRUE\t/\tTRUE\t{FUTURE}\tauth-token\t{SYNTHETIC_TOKEN}\n.twitch.tv\tTRUE\t/\tTRUE\t{FUTURE}\tlogin\tsynthetic-login\n"
        )
        .into_bytes();
        let artifact = prepare(bytes).expect("prepare Twitch cookies");
        let request = CookieRequest::new("gql.twitch.tv", "/gql", true, NOW);

        assert_eq!(
            artifact.cookie_header_for(request).expect("safe header"),
            Some(format!("auth-token={SYNTHETIC_TOKEN}"))
        );
        assert!(matches!(
            artifact.cookie_value_for("login", request),
            Err(CookieHeaderError::InvalidCookieName)
        ));
    }

    #[test]
    fn secure_cookie_is_not_rendered_for_http() {
        let artifact = prepare(
            format!(".twitch.tv\tTRUE\t/\tTRUE\t{FUTURE}\tauth-token\t{SYNTHETIC_TOKEN}\n")
                .into_bytes(),
        )
        .expect("prepare secure cookie");
        assert_eq!(
            artifact
                .cookie_header_for(CookieRequest::new("twitch.tv", "/", false, NOW))
                .expect("HTTP filtering"),
            None
        );
    }

    #[test]
    fn rejects_expired_allowed_cookie() {
        let expired = NOW - 1;
        let bytes =
            format!(".twitch.tv\tTRUE\t/\tTRUE\t{expired}\tauth-token\t{SYNTHETIC_TOKEN}\n")
                .into_bytes();
        assert!(matches!(
            prepare(bytes),
            Err(CookieArtifactError::ExpiredCookie)
        ));
    }

    #[test]
    fn deleted_auth_sentinel_cannot_satisfy_required_cookie() {
        for sentinel in ["deleted", " DELETE ", "NuLl"] {
            let bytes = format!(".twitch.tv\tTRUE\t/\tTRUE\t{FUTURE}\tauth-token\t{sentinel}\n")
                .into_bytes();
            assert!(matches!(
                prepare(bytes),
                Err(CookieArtifactError::MissingRequiredCookie)
            ));
        }
    }

    #[test]
    fn optional_expired_cookie_does_not_invalidate_valid_auth() {
        let expired = NOW - 1;
        let bytes = format!(
            ".twitch.tv\tTRUE\t/\tTRUE\t{FUTURE}\tauth-token\t{SYNTHETIC_TOKEN}\n.twitch.tv\tTRUE\t/\tTRUE\t{expired}\tlogin\tsynthetic-login-hint\n"
        )
        .into_bytes();
        let artifact = prepare(bytes).expect("valid auth survives optional expired cookie");
        assert_eq!(artifact.cookies().len(), 1);
        assert_eq!(artifact.cookies()[0].name(), "auth-token");
        assert_eq!(artifact.cookies()[0].value(), SYNTHETIC_TOKEN);
    }

    #[test]
    fn rejects_cookie_header_injection() {
        let encoded_newline = format!(
            r#"[{{"name":"auth-token","value":"{SYNTHETIC_TOKEN}\r\nX-Injected: yes","domain":".twitch.tv"}}]"#
        )
        .into_bytes();
        assert!(matches!(
            prepare(encoded_newline),
            Err(CookieArtifactError::UnsafeCookieValue)
        ));

        let semicolon = format!(
            r#"[{{"name":"auth-token","value":"{SYNTHETIC_TOKEN};injected=yes","domain":".twitch.tv"}}]"#
        )
        .into_bytes();
        assert!(matches!(
            prepare(semicolon),
            Err(CookieArtifactError::UnsafeCookieValue)
        ));
    }

    #[test]
    fn enforces_dns_label_boundaries_for_artifacts_and_requests() {
        let evil_domain = format!(
            r#"[{{"name":"auth-token","value":"{SYNTHETIC_TOKEN}","domain":"twitch.tv.evil.invalid"}}]"#
        )
        .into_bytes();
        assert!(matches!(
            prepare(evil_domain),
            Err(CookieArtifactError::NoAllowedCookies)
        ));

        let artifact = prepare(
            format!(".twitch.tv\tTRUE\t/\tTRUE\t{FUTURE}\tauth-token\t{SYNTHETIC_TOKEN}\n")
                .into_bytes(),
        )
        .expect("prepare allowed cookie");
        assert!(matches!(
            artifact.cookie_header_for(CookieRequest::new("nottwitch.tv", "/", true, NOW,)),
            Err(CookieHeaderError::TargetNotAllowed)
        ));
    }

    #[test]
    fn rejects_conflicting_aliases_instead_of_guessing() {
        let bytes = br#"{"module":"twitch","auth-token":"synthetic-twitch-token-0001","oauth":"OAuth synthetic-twitch-token-0002"}"#.to_vec();
        assert!(matches!(
            prepare(bytes),
            Err(CookieArtifactError::AmbiguousCookie)
        ));
    }

    #[test]
    fn enforces_byte_cookie_and_depth_limits() {
        assert!(matches!(
            prepare(vec![b'x'; MAX_COOKIE_ARTIFACT_BYTES + 1]),
            Err(CookieArtifactError::TooLarge)
        ));

        let deeply_nested = format!(
            "{}0{}",
            "[".repeat(MAX_COOKIE_JSON_DEPTH + 1),
            "]".repeat(MAX_COOKIE_JSON_DEPTH + 1)
        )
        .into_bytes();
        assert!(matches!(
            prepare(deeply_nested),
            Err(CookieArtifactError::JsonTooDeep)
        ));

        let cookie =
            format!(r#"{{"name":"auth-token","value":"{SYNTHETIC_TOKEN}","domain":".twitch.tv"}}"#);
        let too_many = format!("[{}]", vec![cookie; MAX_COOKIE_COUNT + 1].join(","));
        assert!(matches!(
            prepare(too_many.into_bytes()),
            Err(CookieArtifactError::TooManyCookies)
        ));
    }

    #[test]
    fn keeps_bom_and_surrounding_whitespace_in_export_bytes() {
        let bytes = format!(
            "\u{feff}  [{{\"name\":\"auth-token\",\"value\":\"{SYNTHETIC_TOKEN}\",\"domain\":\".twitch.tv\"}}]  \r\n"
        )
        .into_bytes();
        let artifact = prepare(bytes.clone()).expect("prepare BOM JSON");
        assert_eq!(artifact.into_artifact_bytes(), bytes);
    }
}
