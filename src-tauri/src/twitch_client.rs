use crate::{
    cookie_artifact::{CookieRequest, PreparedCookieArtifact},
    module_probe::{
        CookieModuleProber, ModulePlan, ModuleProbeResult, ModuleProbeStatus, ProbeControl,
        TwitchPlan, TwitchRole,
    },
    proxy_store::StoredProxy,
};
use futures_util::StreamExt;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Runtime;
use wreq::{Client, Proxy, header::OrigHeaderMap, redirect};
use wreq_util::{Emulation, Platform, Profile};

const GQL_URL: &str = "https://gql.twitch.tv/gql";
const GQL_HOST: &str = "gql.twitch.tv";
const WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROXY_ATTEMPTS: usize = 3;
// A task can use every imported route without constructing one browser client per proxy.
// This bound covers the current 32 workers plus failover headroom while keeping memory flat.
const MAX_CACHED_PROXY_CLIENTS: usize = 128;
const CANCELLATION_POLL: Duration = Duration::from_millis(50);

const CURRENT_USER_QUERY: &str = r#"query CurrentUser {
  currentUser {
    id
    login
    hasPrime
    hasTurbo
    roles { isPartner isAffiliate }
  }
}"#;

pub(crate) struct TwitchProbePool {
    runtime: Runtime,
    direct_client: Option<Client>,
    proxy_routes: Vec<Proxy>,
    client_cache: Mutex<ClientCache>,
    timeout: Duration,
    proxy_count: usize,
    retries: u8,
    next_route: AtomicUsize,
    limiter: RequestLimiter,
}

#[derive(Default)]
struct ClientCache {
    clients: HashMap<usize, Client>,
    recency: VecDeque<usize>,
    failed_routes: HashSet<usize>,
}

impl ClientCache {
    fn touch(&mut self, index: usize) {
        if let Some(position) = self.recency.iter().position(|cached| *cached == index) {
            self.recency.remove(position);
        }
        self.recency.push_back(index);
    }

    fn insert(&mut self, index: usize, client: Client) {
        self.failed_routes.remove(&index);
        if let std::collections::hash_map::Entry::Occupied(mut entry) = self.clients.entry(index) {
            entry.insert(client);
            self.touch(index);
            return;
        }
        if self.clients.len() >= MAX_CACHED_PROXY_CLIENTS
            && let Some(evicted) = self.recency.pop_front()
        {
            self.clients.remove(&evicted);
        }
        self.clients.insert(index, client);
        self.touch(index);
    }

    fn mark_failed(&mut self, index: usize) {
        self.clients.remove(&index);
        if let Some(position) = self.recency.iter().position(|cached| *cached == index) {
            self.recency.remove(position);
        }
        self.failed_routes.insert(index);
    }
}

impl TwitchProbePool {
    pub(crate) fn new(
        timeout_ms: u64,
        retries: u8,
        delay_ms: u64,
        proxies: &[StoredProxy],
    ) -> Result<Self, String> {
        let timeout = Duration::from_millis(timeout_ms.clamp(3_000, 120_000));
        let verified_transport = build_client(timeout, None)?;
        let (direct_client, proxy_routes) = if proxies.is_empty() {
            (Some(verified_transport), Vec::new())
        } else {
            let mut routes = Vec::with_capacity(proxies.len());
            let mut last_error = None;
            for stored in proxies {
                match build_proxy(stored) {
                    Ok(proxy) => routes.push(proxy),
                    Err(error) => last_error = Some(error),
                }
            }
            if routes.is_empty() {
                return Err(last_error
                    .unwrap_or_else(|| "no compatible active proxy is available".to_string()));
            }
            (None, routes)
        };
        let proxy_count = proxy_routes.len();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("ayla-twitch-http")
            .worker_threads(
                std::thread::available_parallelism()
                    .map(|value| value.get())
                    .unwrap_or(2)
                    .clamp(2, 4),
            )
            .build()
            .map_err(|_| "unable to initialize the Twitch network runtime".to_string())?;

        Ok(Self {
            runtime,
            direct_client,
            proxy_routes,
            client_cache: Mutex::new(ClientCache::default()),
            timeout,
            proxy_count,
            retries: retries.min(5),
            next_route: AtomicUsize::new(0),
            limiter: RequestLimiter::new(Duration::from_millis(delay_ms.min(60_000))),
        })
    }

    pub(crate) const fn proxy_count(&self) -> usize {
        self.proxy_count
    }

    fn route_count(&self) -> usize {
        if self.direct_client.is_some() {
            1
        } else {
            self.proxy_routes.len()
        }
    }

    fn client_for_route(&self, index: usize) -> Result<Client, String> {
        if let Some(client) = self.direct_client.as_ref() {
            return Ok(client.clone());
        }
        let proxy = self
            .proxy_routes
            .get(index)
            .cloned()
            .ok_or_else(|| "the selected Twitch proxy route is unavailable".to_string())?;

        {
            let mut cache = lock_unpoison(&self.client_cache);
            if cache.failed_routes.contains(&index) {
                return Err("the selected Twitch proxy route is unavailable".to_string());
            }
            if let Some(client) = cache.clients.get(&index).cloned() {
                cache.touch(index);
                return Ok(client);
            }
        }

        let client = match build_client(self.timeout, Some(proxy)) {
            Ok(client) => client,
            Err(error) => {
                lock_unpoison(&self.client_cache).mark_failed(index);
                return Err(error);
            }
        };
        let mut cache = lock_unpoison(&self.client_cache);
        if let Some(existing) = cache.clients.get(&index).cloned() {
            cache.touch(index);
            return Ok(existing);
        }
        cache.insert(index, client.clone());
        Ok(client)
    }

    fn check_with_client(
        &self,
        client: &Client,
        artifact: &PreparedCookieArtifact,
        control: &dyn ProbeControl,
    ) -> Result<ModuleProbeResult, ProbeFailure> {
        self.query_with_retry(client, artifact, CURRENT_USER_QUERY, control)
            .map(|(plan, retries)| ModuleProbeResult {
                status: ModuleProbeStatus::Active(ModulePlan::Twitch(plan)),
                retries,
            })
    }

    fn query_with_retry(
        &self,
        client: &Client,
        artifact: &PreparedCookieArtifact,
        query: &'static str,
        control: &dyn ProbeControl,
    ) -> Result<(TwitchPlan, usize), ProbeFailure> {
        let attempts = usize::from(self.retries) + 1;
        for attempt in 0..attempts {
            if control.is_cancelled() {
                return Err(ProbeFailure {
                    kind: FailureKind::Cancelled,
                    retries: attempt,
                });
            }
            if attempt > 0
                && control
                    .wait_cancelled(Duration::from_secs((1_u64 << (attempt - 1).min(3)).min(12)))
            {
                return Err(ProbeFailure {
                    kind: FailureKind::Cancelled,
                    retries: attempt,
                });
            }

            let result =
                self.runtime
                    .block_on(query_once(client, artifact, query, &self.limiter, control));
            match result {
                Ok(plan) => return Ok((plan, attempt)),
                Err(kind) if attempt + 1 < attempts && kind.retryable() => {}
                Err(kind) => {
                    return Err(ProbeFailure {
                        kind,
                        retries: attempt,
                    });
                }
            }
        }

        Err(ProbeFailure {
            kind: FailureKind::Transient,
            retries: usize::from(self.retries),
        })
    }
}

impl CookieModuleProber for TwitchProbePool {
    fn check(
        &self,
        artifact: &PreparedCookieArtifact,
        control: &dyn ProbeControl,
    ) -> ModuleProbeResult {
        let route_count = self.route_count();
        if artifact.module_id() != "twitch" || route_count == 0 {
            return ModuleProbeResult {
                status: ModuleProbeStatus::Error,
                retries: 0,
            };
        }

        let start = self.next_route.fetch_add(1, Ordering::Relaxed) % route_count;
        let mut retries = 0usize;
        let mut route_offset = 0usize;
        let mut network_attempts = 0usize;
        let mut last_failure = None;
        while route_offset < route_count && network_attempts < MAX_PROXY_ATTEMPTS {
            let route_index = (start + route_offset) % route_count;
            route_offset = route_offset.saturating_add(1);
            let client = match self.client_for_route(route_index) {
                Ok(client) => client,
                Err(_) => {
                    retries = retries.saturating_add(1);
                    continue;
                }
            };
            network_attempts = network_attempts.saturating_add(1);
            match self.check_with_client(&client, artifact, control) {
                Ok(mut result) => {
                    result.retries = result.retries.saturating_add(retries);
                    return result;
                }
                Err(failure) => {
                    retries = retries.saturating_add(failure.retries);
                    last_failure = Some(failure.kind);
                    if failure.kind.can_failover()
                        && network_attempts < MAX_PROXY_ATTEMPTS
                        && route_offset < route_count
                    {
                        retries = retries.saturating_add(1);
                        continue;
                    }
                    return ModuleProbeResult {
                        status: failure.kind.status(),
                        retries,
                    };
                }
            }
        }

        ModuleProbeResult {
            status: last_failure.map_or(ModuleProbeStatus::Error, FailureKind::status),
            retries,
        }
    }
}

async fn query_once(
    client: &Client,
    artifact: &PreparedCookieArtifact,
    query: &'static str,
    limiter: &RequestLimiter,
    control: &dyn ProbeControl,
) -> Result<TwitchPlan, FailureKind> {
    if limiter.wait(control) {
        return Err(FailureKind::Cancelled);
    }
    let now = now_unix().ok_or(FailureKind::Permanent)?;
    let token = twitch_auth_token(artifact, now)?.ok_or(FailureKind::Permanent)?;
    let cookie_header = artifact
        .cookie_header_for(CookieRequest::new(GQL_HOST, "/gql", true, now))
        .map_err(|_| FailureKind::Permanent)?;
    let payload = serde_json::json!({ "query": query });

    let mut request = client
        .post(GQL_URL)
        .orig_headers(twitch_header_order())
        .header("accept", "*/*")
        .header("authorization", format!("OAuth {token}"))
        .header("client-id", WEB_CLIENT_ID)
        .header("content-type", "application/json")
        .header("origin", "https://www.twitch.tv")
        .header("referer", "https://www.twitch.tv/")
        .header("user-agent", CHROME_USER_AGENT)
        .json(&payload);
    if let Some(cookie_header) = cookie_header {
        request = request.header("cookie", cookie_header);
    }

    let response = cancellable(control, request.send())
        .await?
        .map_err(|_| FailureKind::Transient)?;
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(FailureKind::Permanent);
    }
    let body = cancellable(control, read_bounded_body(response)).await??;
    classify_response(status, &body)
}

async fn cancellable<F, T>(control: &dyn ProbeControl, future: F) -> Result<T, FailureKind>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(future);
    tokio::select! {
        output = &mut future => Ok(output),
        _ = cancellation_watcher(control) => Err(FailureKind::Cancelled),
    }
}

async fn cancellation_watcher(control: &dyn ProbeControl) {
    while !control.is_cancelled() {
        tokio::time::sleep(CANCELLATION_POLL).await;
    }
}

async fn read_bounded_body(response: wreq::Response) -> Result<Vec<u8>, FailureKind> {
    let stream = response.bytes_stream();
    futures_util::pin_mut!(stream);
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| FailureKind::Transient)?;
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .filter(|length| *length <= MAX_RESPONSE_BYTES)
            .ok_or(FailureKind::Permanent)?;
        body.reserve(next_len.saturating_sub(body.len()));
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn twitch_auth_token(
    artifact: &PreparedCookieArtifact,
    now: i64,
) -> Result<Option<&str>, FailureKind> {
    artifact
        .cookie_value_for(
            "auth-token",
            CookieRequest::new(GQL_HOST, "/gql", true, now),
        )
        .map(|token| token.filter(|token| !token.is_empty()))
        .map_err(|_| FailureKind::Permanent)
}

fn twitch_header_order() -> OrigHeaderMap {
    let mut order = OrigHeaderMap::with_capacity(7);
    for header in [
        "accept",
        "authorization",
        "client-id",
        "content-type",
        "origin",
        "referer",
        "user-agent",
    ] {
        order.insert(header);
    }
    order
}

fn classify_response(status: u16, body: &[u8]) -> Result<TwitchPlan, FailureKind> {
    match status {
        401 => return Err(FailureKind::Dead),
        400 => {
            let message = String::from_utf8_lossy(body).to_ascii_lowercase();
            return if is_auth_error(&message) {
                Err(FailureKind::Dead)
            } else if is_gateway_error(&message) {
                Err(FailureKind::RateLimited)
            } else {
                Err(FailureKind::Permanent)
            };
        }
        403 => {
            let message = String::from_utf8_lossy(body).to_ascii_lowercase();
            return if is_auth_error(&message) {
                Err(FailureKind::Dead)
            } else {
                Err(FailureKind::RateLimited)
            };
        }
        429 => return Err(FailureKind::RateLimited),
        408 | 425 | 500..=599 => return Err(FailureKind::Transient),
        200 => {}
        _ => return Err(FailureKind::Permanent),
    }

    let root: serde_json::Value = serde_json::from_slice(body).map_err(|_| {
        let message = String::from_utf8_lossy(body).to_ascii_lowercase();
        if is_gateway_error(&message) {
            FailureKind::RateLimited
        } else {
            FailureKind::Permanent
        }
    })?;
    let had_current_user_field = root
        .get("data")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|data| data.contains_key("currentUser"));
    let envelope: GqlEnvelope = serde_json::from_value(root).map_err(|_| FailureKind::Permanent)?;
    let messages = envelope
        .errors
        .iter()
        .map(|error| error.message.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    if is_auth_error(&messages) {
        return Err(FailureKind::Dead);
    }
    if is_gateway_error(&messages) {
        return Err(FailureKind::RateLimited);
    }
    if !envelope.errors.is_empty() {
        return Err(FailureKind::Permanent);
    }

    if let Some(user) = envelope.data.and_then(|data| data.current_user) {
        if user.id.trim().is_empty() || user.login.trim().is_empty() {
            return Err(FailureKind::Permanent);
        }
        let roles = user.roles.ok_or(FailureKind::Permanent)?;
        let role = if roles.is_partner {
            TwitchRole::Partner
        } else if roles.is_affiliate {
            TwitchRole::Affiliate
        } else {
            TwitchRole::Viewer
        };
        return Ok(TwitchPlan {
            has_prime: user.has_prime.ok_or(FailureKind::Permanent)?,
            has_turbo: user.has_turbo.ok_or(FailureKind::Permanent)?,
            role,
        });
    }

    if had_current_user_field {
        Err(FailureKind::Dead)
    } else {
        let message = String::from_utf8_lossy(body).to_ascii_lowercase();
        if is_auth_error(&message) {
            Err(FailureKind::Dead)
        } else if is_gateway_error(&message) {
            Err(FailureKind::RateLimited)
        } else {
            Err(FailureKind::Permanent)
        }
    }
}

fn is_auth_error(message: &str) -> bool {
    contains_any(
        message,
        &[
            "unauthorized",
            "invalid oauth",
            "invalid auth",
            "authentication failed",
        ],
    )
}

fn is_gateway_error(message: &str) -> bool {
    contains_any(
        message,
        &[
            "failed integrity",
            "rate limit",
            "too many requests",
            "blocked",
            "captcha",
            "cf-chl",
            "cloudflare",
            "access denied",
        ],
    )
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GqlEnvelope {
    data: Option<GqlData>,
    errors: Vec<GqlError>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GqlData {
    #[serde(rename = "currentUser")]
    current_user: Option<GqlUser>,
}

#[derive(Deserialize)]
struct GqlUser {
    id: String,
    login: String,
    #[serde(rename = "hasPrime")]
    has_prime: Option<bool>,
    #[serde(rename = "hasTurbo")]
    has_turbo: Option<bool>,
    roles: Option<GqlRoles>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GqlRoles {
    #[serde(rename = "isPartner")]
    is_partner: bool,
    #[serde(rename = "isAffiliate")]
    is_affiliate: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GqlError {
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    Dead,
    RateLimited,
    Transient,
    Permanent,
    Cancelled,
}

impl FailureKind {
    fn retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::Transient)
    }

    fn can_failover(self) -> bool {
        matches!(self, Self::RateLimited | Self::Transient)
    }

    fn status(self) -> ModuleProbeStatus {
        match self {
            Self::Dead => ModuleProbeStatus::Dead,
            Self::RateLimited => ModuleProbeStatus::RateLimited,
            Self::Transient | Self::Permanent | Self::Cancelled => ModuleProbeStatus::Error,
        }
    }
}

struct ProbeFailure {
    kind: FailureKind,
    retries: usize,
}

struct RequestLimiter {
    interval: Duration,
    next: Mutex<Instant>,
}

impl RequestLimiter {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: Mutex::new(Instant::now()),
        }
    }

    fn wait(&self, control: &dyn ProbeControl) -> bool {
        if self.interval.is_zero() {
            return control.is_cancelled();
        }
        let wait = {
            let mut next = lock_unpoison(&self.next);
            let now = Instant::now();
            let slot = (*next).max(now);
            *next = slot.checked_add(self.interval).unwrap_or(slot);
            slot.saturating_duration_since(now)
        };
        control.wait_cancelled(wait)
    }
}

fn build_client(timeout: Duration, proxy: Option<Proxy>) -> Result<Client, String> {
    let emulation = Emulation::builder()
        .profile(Profile::Chrome131)
        .platform(Platform::Windows)
        .build();
    let mut builder = Client::builder()
        .emulation(emulation)
        .orig_headers(twitch_header_order())
        .cookie_store(false)
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(20)))
        .read_timeout(timeout)
        .redirect(redirect::Policy::none())
        .https_only(true)
        .no_proxy();
    if let Some(proxy) = proxy {
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|_| "unable to initialize the Twitch browser transport".to_string())
}

fn build_proxy(stored: &StoredProxy) -> Result<Proxy, String> {
    if stored.protocol == "socks4" && stored.username.is_some() {
        return Err(
            "authenticated SOCKS4 is not supported by the Twitch browser transport".to_string(),
        );
    }
    let scheme = match stored.protocol.as_str() {
        "http" => "http",
        "socks4" => "socks4a",
        "socks5" => "socks5h",
        _ => return Err("unsupported proxy protocol".to_string()),
    };
    let host = if stored.host.contains(':') && !stored.host.starts_with('[') {
        format!("[{}]", stored.host)
    } else {
        stored.host.clone()
    };
    let mut proxy = Proxy::all(format!("{scheme}://{host}:{}", stored.port))
        .map_err(|_| "the configured proxy is invalid".to_string())?;
    if let Some(username) = stored.username.as_deref() {
        proxy = proxy.basic_auth(username, stored.password.as_deref().unwrap_or(""));
    }
    Ok(proxy)
}

fn now_unix() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

fn lock_unpoison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_proxy(protocol: &str, with_auth: bool) -> StoredProxy {
        StoredProxy {
            protocol: protocol.to_string(),
            host: "127.0.0.1".to_string(),
            port: 9_050,
            username: with_auth.then(|| "synthetic-user".to_string()),
            password: with_auth.then(|| "synthetic-password".to_string()),
            ..StoredProxy::default()
        }
    }

    #[test]
    fn active_response_extracts_twitch_entitlements() {
        let body = br#"{
          "data": {"currentUser": {
            "id": "synthetic-user-id",
            "login": "synthetic_login",
            "hasPrime": true,
            "hasTurbo": true,
            "roles": {"isPartner": true, "isAffiliate": true}
          }}
        }"#;
        let plan = classify_response(200, body).expect("active synthetic profile");
        assert_eq!(
            plan,
            TwitchPlan {
                has_prime: true,
                has_turbo: true,
                role: TwitchRole::Partner,
            }
        );
    }

    #[test]
    fn null_user_and_explicit_auth_failure_are_dead() {
        let body = br#"{"data":{"currentUser":null},"errors":[{"message":"Unauthorized"}]}"#;
        assert_eq!(classify_response(200, body), Err(FailureKind::Dead));
        assert_eq!(classify_response(401, b""), Err(FailureKind::Dead));
        assert_eq!(
            classify_response(403, b"unauthorized oauth token"),
            Err(FailureKind::Dead)
        );
    }

    #[test]
    fn gateway_blocks_never_become_dead_sessions() {
        assert_eq!(
            classify_response(200, br#"{"errors":[{"message":"failed integrity"}]}"#),
            Err(FailureKind::RateLimited)
        );
        assert_eq!(
            classify_response(403, b"access denied"),
            Err(FailureKind::RateLimited)
        );
        assert_eq!(
            classify_response(429, b"too many requests"),
            Err(FailureKind::RateLimited)
        );
        assert_eq!(
            classify_response(200, b"<html>Cloudflare access denied</html>"),
            Err(FailureKind::RateLimited)
        );
        assert_eq!(
            classify_response(200, br#"{"message":"failed integrity"}"#),
            Err(FailureKind::RateLimited)
        );
        assert_eq!(
            classify_response(400, b"failed integrity"),
            Err(FailureKind::RateLimited)
        );
    }

    #[test]
    fn malformed_or_partial_profiles_are_errors_not_false_dead() {
        assert_eq!(
            classify_response(200, b"not-json"),
            Err(FailureKind::Permanent)
        );
        assert_eq!(
            classify_response(
                200,
                br#"{"data":{"currentUser":{"id":"synthetic","login":""}}}"#,
            ),
            Err(FailureKind::Permanent)
        );
        assert_eq!(
            classify_response(
                200,
                br#"{"data":{"currentUser":{"id":"synthetic","login":"synthetic_login","hasPrime":true,"roles":{"isPartner":false,"isAffiliate":false}}}}"#,
            ),
            Err(FailureKind::Permanent)
        );
        assert_eq!(
            classify_response(503, b"upstream unavailable"),
            Err(FailureKind::Transient)
        );
    }

    #[test]
    fn graphql_errors_take_precedence_over_partial_user_data() {
        let body = br#"{
          "data":{"currentUser":{
            "id":"synthetic","login":"synthetic_login",
            "hasPrime":false,"hasTurbo":false,
            "roles":{"isPartner":false,"isAffiliate":false}
          }},
          "errors":[{"message":"Unauthorized"}]
        }"#;
        assert_eq!(classify_response(200, body), Err(FailureKind::Dead));
    }

    #[test]
    fn ignored_profile_text_cannot_trigger_gateway_classification() {
        let body = br#"{
          "data":{"currentUser":{
            "id":"synthetic","login":"synthetic_login",
            "description":"cloudflare rate limit captcha",
            "hasPrime":false,"hasTurbo":false,
            "roles":{"isPartner":false,"isAffiliate":false}
          }}
        }"#;
        assert_eq!(
            classify_response(200, body),
            Ok(TwitchPlan {
                has_prime: false,
                has_turbo: false,
                role: TwitchRole::Viewer,
            })
        );
    }

    #[test]
    fn proxy_builder_preserves_supported_auth_and_rejects_unsupported_socks4_auth() {
        assert!(build_proxy(&stored_proxy("http", true)).is_ok());
        assert!(build_proxy(&stored_proxy("socks5", true)).is_ok());
        assert!(build_proxy(&stored_proxy("socks4", false)).is_ok());
        assert_eq!(
            build_proxy(&stored_proxy("socks4", true)).err().as_deref(),
            Some("authenticated SOCKS4 is not supported by the Twitch browser transport")
        );
    }

    #[test]
    fn proxy_client_cache_stays_bounded() {
        let client = build_client(Duration::from_secs(3), None).expect("build synthetic client");
        let mut cache = ClientCache::default();
        for index in 0..=MAX_CACHED_PROXY_CLIENTS {
            cache.insert(index, client.clone());
        }

        assert_eq!(cache.clients.len(), MAX_CACHED_PROXY_CLIENTS);
        assert_eq!(cache.recency.len(), MAX_CACHED_PROXY_CLIENTS);
        assert!(!cache.clients.contains_key(&0));
        assert!(cache.clients.contains_key(&MAX_CACHED_PROXY_CLIENTS));

        cache.mark_failed(MAX_CACHED_PROXY_CLIENTS);
        assert!(cache.failed_routes.contains(&MAX_CACHED_PROXY_CLIENTS));
        assert!(!cache.clients.contains_key(&MAX_CACHED_PROXY_CLIENTS));
        cache.insert(MAX_CACHED_PROXY_CLIENTS, client);
        assert!(!cache.failed_routes.contains(&MAX_CACHED_PROXY_CLIENTS));
    }
}
