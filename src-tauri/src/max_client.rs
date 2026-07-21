use crate::{
    cookie_artifact::{CookieRequest, PreparedCookieArtifact},
    module_probe::{
        CookieModuleProber, MaxPlan, MaxSubscriptionState, MaxTier, ModulePlan, ModuleProbeResult,
        ModuleProbeStatus, ProbeControl,
    },
    proxy_store::StoredProxy,
};
use base64::{Engine as _, engine::general_purpose};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Runtime;
use url::Url;
use wreq::{Client, Proxy, header::OrigHeaderMap, redirect};
use wreq_util::{Emulation, Platform, Profile};

const USER_PATH: &str = "/users/me";
const SUBSCRIPTIONS_PATH: &str = "/monetization/subscriptions";
const BOOTSTRAP_PATH: &str = "/session-context/headwaiter/v1/bootstrap";
const WEB_CLIENT_ID: &str = "da0cdd94-5a39-42ef-aa68-54cbc1b852c3";
const DISCO_CLIENT: &str = "WEB:WINDOWS:hbomax:0.1.0";
const DISCO_PARAMS: &str = "realm=bolt,bid=beam,features=ar";
const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROXY_ATTEMPTS: usize = 3;
const MAX_CACHED_PROXY_CLIENTS: usize = 128;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const CANCELLATION_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy)]
struct ApiRoot {
    domain: &'static str,
    bootstrap_host: &'static str,
}

const API_ROOTS: [ApiRoot; 3] = [
    ApiRoot {
        domain: "api.hbomax.com",
        bootstrap_host: "default.any-any.prd.api.hbomax.com",
    },
    ApiRoot {
        domain: "api.max.com",
        bootstrap_host: "default.any-any.prd.api.max.com",
    },
    ApiRoot {
        domain: "api.discomax.com",
        bootstrap_host: "default.any-any.prd.api.discomax.com",
    },
];

pub(crate) struct MaxProbePool {
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

impl MaxProbePool {
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
            .thread_name("ayla-max-http")
            .worker_threads(
                std::thread::available_parallelism()
                    .map(|value| value.get())
                    .unwrap_or(2)
                    .clamp(2, 4),
            )
            .build()
            .map_err(|_| "unable to initialize the HBO Max network runtime".to_string())?;

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
            .ok_or_else(|| "the selected HBO Max proxy route is unavailable".to_string())?;

        {
            let mut cache = lock_unpoison(&self.client_cache);
            if cache.failed_routes.contains(&index) {
                return Err("the selected HBO Max proxy route is unavailable".to_string());
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
                    .block_on(probe_once(client, artifact, &self.limiter, control));
            match result {
                Ok(plan) => {
                    return Ok(ModuleProbeResult {
                        status: ModuleProbeStatus::Active(ModulePlan::Max(plan)),
                        retries: attempt,
                    });
                }
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

impl CookieModuleProber for MaxProbePool {
    fn check(
        &self,
        artifact: &PreparedCookieArtifact,
        control: &dyn ProbeControl,
    ) -> ModuleProbeResult {
        let route_count = self.route_count();
        if artifact.module_id() != "max" || route_count == 0 {
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

#[derive(Debug)]
struct MaxAuthContext {
    cookie_header: String,
    api_root: &'static str,
    device_id: String,
}

async fn probe_once(
    client: &Client,
    artifact: &PreparedCookieArtifact,
    limiter: &RequestLimiter,
    control: &dyn ProbeControl,
) -> Result<MaxPlan, FailureKind> {
    let now = now_unix().ok_or(FailureKind::Permanent)?;
    let auth = auth_context(artifact, now)?;
    let bootstrap_root = API_ROOTS
        .iter()
        .find(|root| root.domain == auth.api_root)
        .ok_or(FailureKind::Permanent)?;
    let bootstrap_url = format!(
        "https://{}{}",
        bootstrap_root.bootstrap_host, BOOTSTRAP_PATH
    );
    let (_, bootstrap_body) = request_json(
        client,
        RequestKind::Post,
        &bootstrap_url,
        bootstrap_root.bootstrap_host,
        BOOTSTRAP_PATH,
        &auth,
        limiter,
        control,
    )
    .await?;
    let bootstrap = parse_bootstrap(&bootstrap_body)?;
    let user_endpoint = resolve_endpoint(&bootstrap, USER_PATH, auth.api_root)?;
    let (_, user_body) = request_json(
        client,
        RequestKind::Get,
        &user_endpoint.url,
        &user_endpoint.host,
        USER_PATH,
        &auth,
        limiter,
        control,
    )
    .await?;
    validate_user_response(&user_body)?;

    let mut subscription_endpoint =
        resolve_endpoint(&bootstrap, SUBSCRIPTIONS_PATH, auth.api_root)?;
    let mut subscription_url =
        Url::parse(&subscription_endpoint.url).map_err(|_| FailureKind::Permanent)?;
    subscription_url
        .query_pairs_mut()
        .append_pair(
            "filter[status]",
            "ACTIVE,IN_GRACE_PERIOD,PRE_ACTIVE,PAUSED,CANCELLED,EXPIRED",
        )
        .append_pair("include", "pricePlan,product,nextPaymentPricePlan");
    subscription_endpoint.url = subscription_url.to_string();
    let (_, subscription_body) = request_json(
        client,
        RequestKind::Get,
        &subscription_endpoint.url,
        &subscription_endpoint.host,
        SUBSCRIPTIONS_PATH,
        &auth,
        limiter,
        control,
    )
    .await?;
    parse_subscription_plan(&subscription_body)
}

fn auth_context(
    artifact: &PreparedCookieArtifact,
    now_unix: i64,
) -> Result<MaxAuthContext, FailureKind> {
    for root in API_ROOTS {
        let request = CookieRequest::new(root.bootstrap_host, BOOTSTRAP_PATH, true, now_unix);
        let Some(raw_token) = artifact
            .cookie_value_for("st", request)
            .map_err(|_| FailureKind::Permanent)?
        else {
            continue;
        };
        let token = normalize_token(raw_token)?;
        let claims = decode_token_claims(token)?;
        let device_id = claims
            .device_id
            .filter(|value| safe_device_id(value))
            .unwrap_or_else(|| "ayla-desktop".to_string());
        return Ok(MaxAuthContext {
            cookie_header: format!("st={token}"),
            api_root: root.domain,
            device_id,
        });
    }
    Err(FailureKind::Permanent)
}

fn normalize_token(value: &str) -> Result<&str, FailureKind> {
    let token = value
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| value.trim().strip_prefix("Cookie "))
        .unwrap_or(value.trim());
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err(FailureKind::Permanent);
    }
    let mut segments = token.split('.');
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'='))
    };
    let (Some(header), Some(payload), Some(signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return Err(FailureKind::Permanent);
    };
    if !valid_segment(header) || !valid_segment(payload) || !valid_segment(signature) {
        return Err(FailureKind::Permanent);
    }
    Ok(token)
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TokenClaims {
    #[serde(rename = "deviceId")]
    device_id: Option<String>,
}

fn decode_token_claims(token: &str) -> Result<TokenClaims, FailureKind> {
    let payload = token.split('.').nth(1).ok_or(FailureKind::Permanent)?;
    let decoded = general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| general_purpose::URL_SAFE.decode(payload))
        .map_err(|_| FailureKind::Permanent)?;
    if decoded.len() > 16 * 1024 {
        return Err(FailureKind::Permanent);
    }
    serde_json::from_slice(&decoded).map_err(|_| FailureKind::Permanent)
}

fn safe_device_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[derive(Clone, Copy)]
enum RequestKind {
    Get,
    Post,
}

#[allow(clippy::too_many_arguments)]
async fn request_json(
    client: &Client,
    kind: RequestKind,
    url: &str,
    host: &str,
    path: &str,
    auth: &MaxAuthContext,
    limiter: &RequestLimiter,
    control: &dyn ProbeControl,
) -> Result<(u16, Vec<u8>), FailureKind> {
    if !host_allowed(host, auth.api_root) || limiter.wait(control) {
        return Err(if control.is_cancelled() {
            FailureKind::Cancelled
        } else {
            FailureKind::Permanent
        });
    }
    let device_info = format!(
        "hbomax/0.1.0 (Ayla/Desktop; Windows/10; {}/{WEB_CLIENT_ID})",
        auth.device_id
    );
    let request = match kind {
        RequestKind::Get => client.get(url),
        RequestKind::Post => client.post(url),
    }
    .orig_headers(max_header_order())
    .header("accept", "application/json, text/plain, */*")
    .header("content-type", "application/json")
    .header("cookie", &auth.cookie_header)
    .header("user-agent", CHROME_USER_AGENT)
    .header("x-device-info", device_info)
    .header("x-disco-client", DISCO_CLIENT)
    .header("x-disco-params", DISCO_PARAMS)
    .header("x-wbd-time-zone", "UTC");
    let request = match kind {
        RequestKind::Get => request,
        RequestKind::Post => request.body("{}"),
    };

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
    classify_http(status, &body)?;

    // The resolved path is checked before every credentialed request. Keeping this
    // assertion after I/O also prevents future callers from silently passing a URL whose
    // path differs from the scoped operation they intended.
    let parsed = Url::parse(url).map_err(|_| FailureKind::Permanent)?;
    if parsed.path() != path {
        return Err(FailureKind::Permanent);
    }
    Ok((status, body))
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

fn classify_http(status: u16, body: &[u8]) -> Result<(), FailureKind> {
    if status == 200 {
        return Ok(());
    }
    if status == 429 {
        return Err(FailureKind::RateLimited);
    }
    if matches!(status, 408 | 425 | 500..=599) {
        return Err(FailureKind::Transient);
    }

    let signals = error_signals(body);
    if signals.iter().any(|signal| exact_auth_error(signal)) {
        return Err(FailureKind::Dead);
    }
    if signals.iter().any(|signal| gateway_error(signal)) {
        return Err(FailureKind::RateLimited);
    }
    Err(FailureKind::Permanent)
}

fn error_signals(body: &[u8]) -> Vec<String> {
    let Ok(root) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let mut signals = Vec::new();
    if let Some(errors) = root.get("errors").and_then(Value::as_array) {
        for error in errors.iter().take(16) {
            for key in ["code", "title", "message", "detail"] {
                if let Some(value) = error.get(key).and_then(Value::as_str) {
                    signals.push(value.trim().to_ascii_lowercase());
                }
            }
        }
    }
    signals
}

fn exact_auth_error(signal: &str) -> bool {
    matches!(
        signal,
        "invalid.token"
            | "invalid_token"
            | "token.invalid"
            | "token.expired"
            | "authentication.invalid"
            | "authentication.required"
            | "not.authenticated"
            | "unauthorized"
    ) || ((signal.contains("token") || signal.contains("authentication"))
        && ["invalid", "expired", "missing", "not valid"]
            .iter()
            .any(|needle| signal.contains(needle)))
}

fn gateway_error(signal: &str) -> bool {
    [
        "rate limit",
        "too many requests",
        "captcha",
        "cloudflare",
        "access denied",
        "blocked request",
    ]
    .iter()
    .any(|needle| signal.contains(needle))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapConfig {
    endpoints: Vec<BootstrapEndpoint>,
    api_groups: HashMap<String, BootstrapApiGroup>,
    routing: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapEndpoint {
    path: String,
    api_group: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapApiGroup {
    base_url: String,
}

fn parse_bootstrap(body: &[u8]) -> Result<BootstrapConfig, FailureKind> {
    let root: Value = serde_json::from_slice(body).map_err(|_| FailureKind::Permanent)?;
    let candidate = root
        .get("data")
        .and_then(|data| data.get("attributes"))
        .unwrap_or(&root);
    let config: BootstrapConfig =
        serde_json::from_value(candidate.clone()).map_err(|_| FailureKind::Permanent)?;
    if config.endpoints.is_empty() || config.api_groups.is_empty() {
        return Err(FailureKind::Permanent);
    }
    Ok(config)
}

#[derive(Debug)]
struct ResolvedEndpoint {
    url: String,
    host: String,
}

fn resolve_endpoint(
    config: &BootstrapConfig,
    request_path: &str,
    expected_root: &str,
) -> Result<ResolvedEndpoint, FailureKind> {
    if !request_path.starts_with('/') || request_path.len() > 4096 {
        return Err(FailureKind::Permanent);
    }
    if config.routing.get("env").map(String::as_str) != Some("prd")
        || config.routing.get("domain").map(String::as_str) != Some(expected_root)
    {
        return Err(FailureKind::Permanent);
    }
    for key in ["tenant", "homeMarket"] {
        if let Some(value) = config.routing.get(key)
            && !safe_dns_label(value)
        {
            return Err(FailureKind::Permanent);
        }
    }

    let endpoint = config
        .endpoints
        .iter()
        .find(|endpoint| request_path.starts_with(&endpoint.path))
        .ok_or(FailureKind::Permanent)?;
    let group = config
        .api_groups
        .get(&endpoint.api_group)
        .ok_or(FailureKind::Permanent)?;
    let mut base_url = group.base_url.clone();
    for (key, value) in &config.routing {
        base_url = base_url.replace(&format!("{{{key}}}"), value);
    }
    if base_url.contains('{') || base_url.contains('}') {
        return Err(FailureKind::Permanent);
    }
    let mut parsed = Url::parse(&base_url).map_err(|_| FailureKind::Permanent)?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some_and(|port| port != 443)
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(FailureKind::Permanent);
    }
    let host = parsed
        .host_str()
        .filter(|host| host_allowed(host, expected_root))
        .ok_or(FailureKind::Permanent)?
        .to_string();
    let base_path = parsed.path().trim_end_matches('/');
    let joined_path = format!("{base_path}{request_path}");
    parsed.set_path(&joined_path);
    Ok(ResolvedEndpoint {
        url: parsed.to_string(),
        host,
    })
}

fn safe_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn host_allowed(host: &str, expected_root: &str) -> bool {
    API_ROOTS.iter().any(|root| root.domain == expected_root)
        && (host == expected_root || host.ends_with(&format!(".{expected_root}")))
}

fn validate_user_response(body: &[u8]) -> Result<(), FailureKind> {
    let root: Value = serde_json::from_slice(body).map_err(|_| FailureKind::Permanent)?;
    let user = root
        .get("data")
        .and_then(Value::as_object)
        .ok_or(FailureKind::Permanent)?;
    let user_type = user.get("type").and_then(Value::as_str);
    let user_id = user.get("id").and_then(Value::as_str);
    if user_type == Some("user")
        && user_id.is_some_and(|id| !id.trim().is_empty() && id.len() <= 512)
    {
        Ok(())
    } else {
        Err(FailureKind::Permanent)
    }
}

fn parse_subscription_plan(body: &[u8]) -> Result<MaxPlan, FailureKind> {
    let root: Value = serde_json::from_slice(body).map_err(|_| FailureKind::Permanent)?;
    let data = root.get("data").ok_or(FailureKind::Permanent)?;
    let records: Vec<&Value> = match data {
        Value::Array(records) => records.iter().collect(),
        Value::Object(_) => vec![data],
        Value::Null => Vec::new(),
        _ => return Err(FailureKind::Permanent),
    };
    if records.is_empty() {
        return Ok(MaxPlan {
            tier: MaxTier::Unknown,
            state: MaxSubscriptionState::NoSubscription,
        });
    }

    let included = root
        .get("included")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut best = None;
    for record in records {
        if record.get("type").and_then(Value::as_str) != Some("subscription") {
            continue;
        }
        let attributes = record.get("attributes").unwrap_or(&Value::Null);
        let status = ["status", "state", "subscriptionStatus"]
            .into_iter()
            .find_map(|key| attributes.get(key).and_then(Value::as_str))
            .map(classify_subscription_state)
            .unwrap_or(MaxSubscriptionState::Unknown);
        let mut plan_text = Vec::new();
        collect_plan_text(attributes, &mut plan_text);
        collect_relationship_plan_text(record, included, &mut plan_text);
        if let Some(id) = record.get("id").and_then(Value::as_str) {
            plan_text.push(id);
        }
        let plan = MaxPlan {
            tier: classify_tier(&plan_text),
            state: status,
        };
        if best.is_none_or(|current: MaxPlan| {
            state_priority(plan.state) > state_priority(current.state)
        }) {
            best = Some(plan);
        }
    }
    best.ok_or(FailureKind::Permanent)
}

fn collect_relationship_plan_text<'a>(
    record: &'a Value,
    included: &'a [Value],
    output: &mut Vec<&'a str>,
) {
    let Some(relationships) = record.get("relationships").and_then(Value::as_object) else {
        return;
    };
    for name in ["pricePlan", "product", "nextPaymentPricePlan"] {
        let Some(data) = relationships.get(name).and_then(|value| value.get("data")) else {
            continue;
        };
        let references: Vec<&Value> = match data {
            Value::Array(values) => values.iter().collect(),
            Value::Object(_) => vec![data],
            _ => Vec::new(),
        };
        for reference in references {
            let reference_type = reference.get("type").and_then(Value::as_str);
            let reference_id = reference.get("id").and_then(Value::as_str);
            if let Some(id) = reference_id {
                output.push(id);
            }
            if let Some(item) = included.iter().find(|item| {
                item.get("type").and_then(Value::as_str) == reference_type
                    && item.get("id").and_then(Value::as_str) == reference_id
            }) {
                collect_plan_text(item.get("attributes").unwrap_or(&Value::Null), output);
            }
        }
    }
}

fn collect_plan_text<'a>(attributes: &'a Value, output: &mut Vec<&'a str>) {
    for key in [
        "name",
        "displayName",
        "planName",
        "productName",
        "code",
        "alias",
        "sku",
        "tier",
    ] {
        if let Some(value) = attributes.get(key).and_then(Value::as_str) {
            output.push(value);
        }
    }
}

fn classify_tier(values: &[&str]) -> MaxTier {
    let normalized = values
        .iter()
        .map(|value| normalize_label(value))
        .collect::<Vec<_>>()
        .join(" ");
    if (normalized.contains("basic") && normalized.contains("ad"))
        || normalized.contains("ad lite")
        || normalized.contains("with ads")
    {
        MaxTier::BasicWithAds
    } else if normalized.contains("platinum") {
        MaxTier::Platinum
    } else if normalized.contains("premium") || normalized.contains("ultimate ad free") {
        MaxTier::Premium
    } else if normalized.contains("mobile") {
        MaxTier::Mobile
    } else if normalized.contains("standard") {
        MaxTier::Standard
    } else if normalized.contains("ad free") || normalized.contains("adfree") {
        MaxTier::AdFree
    } else if normalized.contains("legacy") || normalized.contains("hbo max") {
        MaxTier::Legacy
    } else {
        MaxTier::Unknown
    }
}

fn normalize_label(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn classify_subscription_state(value: &str) -> MaxSubscriptionState {
    match normalize_label(value).replace(' ', "_").as_str() {
        "active" => MaxSubscriptionState::Active,
        "in_grace_period" | "grace_period" | "grace" => MaxSubscriptionState::InGracePeriod,
        "pre_active" | "preactive" => MaxSubscriptionState::PreActive,
        "paused" | "on_hold" => MaxSubscriptionState::Paused,
        "cancelled" | "canceled" => MaxSubscriptionState::Cancelled,
        "expired" | "ended" => MaxSubscriptionState::Expired,
        _ => MaxSubscriptionState::Unknown,
    }
}

fn state_priority(state: MaxSubscriptionState) -> u8 {
    match state {
        MaxSubscriptionState::Active => 7,
        MaxSubscriptionState::InGracePeriod => 6,
        MaxSubscriptionState::PreActive => 5,
        MaxSubscriptionState::Paused => 4,
        MaxSubscriptionState::Cancelled => 3,
        MaxSubscriptionState::Expired => 2,
        MaxSubscriptionState::Unknown => 1,
        MaxSubscriptionState::NoSubscription => 0,
    }
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

fn max_header_order() -> OrigHeaderMap {
    let mut order = OrigHeaderMap::with_capacity(8);
    for header in [
        "accept",
        "content-type",
        "cookie",
        "user-agent",
        "x-device-info",
        "x-disco-client",
        "x-disco-params",
        "x-wbd-time-zone",
    ] {
        order.insert(header);
    }
    order
}

fn build_client(timeout: Duration, proxy: Option<Proxy>) -> Result<Client, String> {
    let emulation = Emulation::builder()
        .profile(Profile::Chrome131)
        .platform(Platform::Windows)
        .build();
    let mut builder = Client::builder()
        .emulation(emulation)
        .orig_headers(max_header_order())
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
        .map_err(|_| "unable to initialize the HBO Max browser transport".to_string())
}

fn build_proxy(stored: &StoredProxy) -> Result<Proxy, String> {
    if stored.protocol == "socks4" && stored.username.is_some() {
        return Err(
            "authenticated SOCKS4 is not supported by the HBO Max browser transport".to_string(),
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
    use crate::cookie_artifact::{MAX_COOKIE_POLICY, prepare_cookie_artifact_at};

    const NOW: i64 = 2_000_000_000;

    fn synthetic_token(device_id: &str) -> String {
        let header = general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#);
        let payload = general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "anonymous": false,
                "deviceId": device_id,
                "exp": NOW + 3600,
            })
            .to_string(),
        );
        format!("{header}.{payload}.synthetic-signature")
    }

    fn prepared_artifact(token: &str) -> PreparedCookieArtifact {
        prepare_cookie_artifact_at(
            format!(
                ".api.hbomax.com\tTRUE\t/\tTRUE\t{}\tst\t{token}\n",
                NOW + 3600
            )
            .into_bytes(),
            MAX_COOKIE_POLICY,
            NOW,
        )
        .expect("prepare synthetic Max cookie")
    }

    fn bootstrap_fixture(domain: &str) -> Vec<u8> {
        serde_json::json!({
            "endpoints": [
                {"path": "/monetization/subscriptions", "apiGroup": "commerce"},
                {"path": "/users", "apiGroup": "identity"},
                {"path": "/", "apiGroup": "bolt"}
            ],
            "apiGroups": {
                "commerce": {"baseUrl": "https://default.{tenant}-{homeMarket}.euc1.{env}.{domain}"},
                "identity": {"baseUrl": "https://default.{tenant}-{homeMarket}.{env}.{domain}"},
                "bolt": {"baseUrl": "https://default.any-any.{env}.{domain}"}
            },
            "routing": {
                "tenant": "beam",
                "homeMarket": "emea",
                "env": "prd",
                "domain": domain
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn extracts_scoped_jwt_and_device_without_exposing_account_data() {
        let token = synthetic_token("device-1234");
        let artifact = prepared_artifact(&token);
        let context = auth_context(&artifact, NOW).expect("extract Max auth context");
        assert_eq!(context.api_root, "api.hbomax.com");
        assert_eq!(context.device_id, "device-1234");
        assert_eq!(context.cookie_header, format!("st={token}"));
    }

    #[test]
    fn regional_routes_are_resolved_from_bootstrap_and_stay_allowlisted() {
        let config =
            parse_bootstrap(&bootstrap_fixture("api.hbomax.com")).expect("parse bootstrap fixture");
        let user =
            resolve_endpoint(&config, USER_PATH, "api.hbomax.com").expect("resolve user endpoint");
        let subscriptions = resolve_endpoint(&config, SUBSCRIPTIONS_PATH, "api.hbomax.com")
            .expect("resolve subscription endpoint");
        assert_eq!(
            user.url,
            "https://default.beam-emea.prd.api.hbomax.com/users/me"
        );
        assert_eq!(
            subscriptions.url,
            "https://default.beam-emea.euc1.prd.api.hbomax.com/monetization/subscriptions"
        );

        let malicious = parse_bootstrap(&bootstrap_fixture("api.hbomax.com.evil.invalid"))
            .expect("parse structurally valid malicious fixture");
        assert!(matches!(
            resolve_endpoint(&malicious, USER_PATH, "api.hbomax.com"),
            Err(FailureKind::Permanent)
        ));
    }

    #[test]
    fn response_classification_does_not_turn_region_or_server_failures_into_dead_sessions() {
        let invalid =
            br#"{"errors":[{"code":"invalid.token","message":"Token is missing or not valid"}]}"#;
        assert_eq!(classify_http(400, invalid), Err(FailureKind::Dead));
        assert_eq!(
            classify_http(403, br#"{"errors":[{"code":"geo.blocked"}]}"#),
            Err(FailureKind::Permanent)
        );
        assert_eq!(classify_http(404, b"{}"), Err(FailureKind::Permanent));
        assert_eq!(classify_http(500, b"{}"), Err(FailureKind::Transient));
        assert_eq!(classify_http(429, b"{}"), Err(FailureKind::RateLimited));
    }

    #[test]
    fn validates_user_without_deserializing_pii() {
        assert_eq!(
            validate_user_response(
                br#"{"data":{"type":"user","id":"USERID:synthetic","attributes":{"email":"fixture@example.invalid"}}}"#
            ),
            Ok(())
        );
        assert_eq!(
            validate_user_response(br#"{"data":null}"#),
            Err(FailureKind::Permanent)
        );
    }

    #[test]
    fn detects_plan_and_status_from_synthetic_json_api_relationships() {
        let body = serde_json::json!({
            "data": [{
                "type": "subscription",
                "id": "synthetic-subscription",
                "attributes": {"status": "IN_GRACE_PERIOD"},
                "relationships": {
                    "pricePlan": {"data": {"type": "pricePlan", "id": "plan-1"}}
                }
            }],
            "included": [{
                "type": "pricePlan",
                "id": "plan-1",
                "attributes": {"displayName": "Premium Monthly"}
            }]
        })
        .to_string();
        assert_eq!(
            parse_subscription_plan(body.as_bytes()),
            Ok(MaxPlan {
                tier: MaxTier::Premium,
                state: MaxSubscriptionState::InGracePeriod,
            })
        );
        assert_eq!(
            parse_subscription_plan(br#"{"data":[],"included":[]}"#),
            Ok(MaxPlan {
                tier: MaxTier::Unknown,
                state: MaxSubscriptionState::NoSubscription,
            })
        );
    }
}
