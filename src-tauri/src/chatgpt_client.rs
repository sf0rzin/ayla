use crate::{
    auth_artifact::{ChatGptEndpoint, PreparedChatGptAuth},
    module_probe::ProbeControl,
    proxy_store::StoredProxy,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Runtime;
use wreq::{Client, Proxy, redirect};

const SESSION_URL: &str = "https://chatgpt.com/api/auth/session";
const ACCOUNTS_URL: &str = "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const ME_URL: &str = "https://chatgpt.com/backend-api/me";
const HOME_URL: &str = "https://chatgpt.com/";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROXY_ATTEMPTS: usize = 4;
const CANCELLATION_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatGptPlan {
    Free,
    Go,
    Plus,
    Pro,
    Team,
    Enterprise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatGptPlanLookup {
    Known(ChatGptPlan),
    /// A valid response described an entitlement or tier Ayla does not recognize.
    Unknown,
    /// The plan endpoints could not provide a usable response within the probe budget.
    Unavailable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatGptProbeStatus {
    Active(ChatGptPlan),
    Authenticated(ChatGptPlanLookup),
    Dead,
    RateLimited,
    Error,
}

#[derive(Clone, Copy)]
pub(crate) struct ChatGptProbeResult {
    pub(crate) status: ChatGptProbeStatus,
    pub(crate) retries: usize,
}

pub(crate) trait ChatGptProber: Send + Sync + 'static {
    fn check(&self, auth: &PreparedChatGptAuth, control: &dyn ProbeControl) -> ChatGptProbeResult;
}

pub(crate) struct ChatGptProbePool {
    runtime: Runtime,
    clients: Vec<Client>,
    proxy_count: usize,
    retries: u8,
    timeout: Duration,
    next_client: AtomicUsize,
}

impl ChatGptProbePool {
    pub(crate) fn new(
        timeout_ms: u64,
        retries: u8,
        proxies: &[StoredProxy],
    ) -> Result<Self, String> {
        let timeout = Duration::from_millis(timeout_ms.clamp(3_000, 120_000));
        let clients = if proxies.is_empty() {
            vec![build_client(timeout, None)?]
        } else {
            let mut clients = Vec::with_capacity(proxies.len());
            let mut last_error = None;
            for stored in proxies {
                match build_proxy(stored).and_then(|proxy| build_client(timeout, Some(proxy))) {
                    Ok(client) => clients.push(client),
                    Err(error) => last_error = Some(error),
                }
            }
            if clients.is_empty() {
                return Err(
                    last_error.unwrap_or_else(|| "no usable proxy is available".to_string())
                );
            }
            clients
        };
        let proxy_count = if proxies.is_empty() { 0 } else { clients.len() };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("ayla-chatgpt-http")
            .worker_threads(
                std::thread::available_parallelism()
                    .map(|value| value.get())
                    .unwrap_or(2)
                    .clamp(2, 4),
            )
            .build()
            .map_err(|_| "unable to initialize the ChatGPT network runtime".to_string())?;
        Ok(Self {
            runtime,
            clients,
            proxy_count,
            retries: retries.min(5),
            timeout,
            next_client: AtomicUsize::new(0),
        })
    }

    pub(crate) fn proxy_count(&self) -> usize {
        self.proxy_count
    }
}

impl ChatGptProber for ChatGptProbePool {
    fn check(&self, auth: &PreparedChatGptAuth, control: &dyn ProbeControl) -> ChatGptProbeResult {
        if control.is_cancelled() {
            return error_result(0);
        }
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        let start = self.next_client.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        let mut previous_retries = 0usize;

        for (attempt, index) in proxy_attempt_indices(start, self.clients.len())
            .into_iter()
            .enumerate()
        {
            if control.is_cancelled() || Instant::now() >= deadline {
                return error_result(previous_retries);
            }
            match self.runtime.block_on(self.check_with_client(
                &self.clients[index],
                auth,
                control,
                deadline,
            )) {
                Ok(mut result) => {
                    if control.is_cancelled() {
                        return error_result(previous_retries.saturating_add(result.retries));
                    }
                    result.retries = result.retries.saturating_add(previous_retries);
                    return result;
                }
                Err(failure) => {
                    previous_retries = previous_retries.saturating_add(failure.retries);
                    if failure.kind.can_failover()
                        && attempt + 1 < self.clients.len().min(MAX_PROXY_ATTEMPTS)
                        && !control.is_cancelled()
                        && Instant::now() < deadline
                    {
                        previous_retries = previous_retries.saturating_add(1);
                        continue;
                    }
                    return ChatGptProbeResult {
                        status: failure.kind.status(),
                        retries: previous_retries,
                    };
                }
            }
        }

        ChatGptProbeResult {
            status: ChatGptProbeStatus::Error,
            retries: previous_retries,
        }
    }
}

impl ChatGptProbePool {
    async fn check_with_client(
        &self,
        client: &Client,
        auth: &PreparedChatGptAuth,
        control: &dyn ProbeControl,
        deadline: Instant,
    ) -> Result<ChatGptProbeResult, HttpFailure> {
        let session = match get_with_retry(
            client,
            ChatGptEndpoint::Session,
            SESSION_URL,
            auth,
            None,
            self.retries,
            control,
            deadline,
        )
        .await
        {
            Ok(response) => response,
            Err(failure) if failure.kind.can_failover() => return Err(failure),
            Err(failure) => {
                return Ok(ChatGptProbeResult {
                    status: match failure.kind {
                        FailureKind::Dead => ChatGptProbeStatus::Dead,
                        FailureKind::RateLimited => ChatGptProbeStatus::RateLimited,
                        FailureKind::Transient
                        | FailureKind::Permanent
                        | FailureKind::Cancelled
                        | FailureKind::Deadline => ChatGptProbeStatus::Error,
                    },
                    retries: failure.retries,
                });
            }
        };

        let mut retries = session.retries;
        let trimmed = trim_ascii(&session.body);
        if trimmed.is_empty() || trimmed == b"{}" || trimmed == b"null" {
            return Ok(ChatGptProbeResult {
                status: ChatGptProbeStatus::Dead,
                retries,
            });
        }
        let Ok(session) = serde_json::from_slice::<Session>(trimmed) else {
            return Ok(ChatGptProbeResult {
                status: ChatGptProbeStatus::Error,
                retries,
            });
        };
        if session.access_token.is_empty()
            && session.user.email.is_empty()
            && session.user.id.is_empty()
        {
            return Ok(ChatGptProbeResult {
                status: ChatGptProbeStatus::Dead,
                retries,
            });
        }

        if control.is_cancelled() {
            return Err(HttpFailure {
                kind: FailureKind::Cancelled,
                retries,
            });
        }
        let (plan, plan_retries) = fetch_plan(
            client,
            auth,
            &session.access_token,
            self.retries,
            control,
            deadline,
        )
        .await?;
        retries = retries.saturating_add(plan_retries);
        Ok(ChatGptProbeResult {
            status: match plan {
                ChatGptPlanLookup::Known(plan) => ChatGptProbeStatus::Active(plan),
                unknown @ (ChatGptPlanLookup::Unknown | ChatGptPlanLookup::Unavailable) => {
                    ChatGptProbeStatus::Authenticated(unknown)
                }
            },
            retries,
        })
    }
}

fn proxy_attempt_indices(start: usize, pool_size: usize) -> Vec<usize> {
    (0..pool_size.min(MAX_PROXY_ATTEMPTS))
        .map(|offset| (start + offset) % pool_size)
        .collect()
}

fn error_result(retries: usize) -> ChatGptProbeResult {
    ChatGptProbeResult {
        status: ChatGptProbeStatus::Error,
        retries,
    }
}

fn build_client(timeout: Duration, proxy: Option<Proxy>) -> Result<Client, String> {
    let mut builder = Client::builder()
        .cookie_store(false)
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(20)))
        .read_timeout(timeout)
        // A pre-rendered Cookie header is valid only for the requested endpoint path.
        // Refuse redirects so the transport cannot reuse it for a different target.
        .redirect(redirect::Policy::none())
        .https_only(true)
        .no_proxy();
    if let Some(proxy) = proxy {
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|_| "unable to initialize the ChatGPT browser transport".to_string())
}

fn build_proxy(stored: &StoredProxy) -> Result<Proxy, String> {
    if stored.protocol == "socks4" && stored.username.is_some() {
        return Err(
            "authenticated SOCKS4 is not supported by the ChatGPT browser transport".to_string(),
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

#[derive(Default, Deserialize)]
#[serde(default)]
struct Session {
    user: SessionUser,
    #[serde(rename = "accessToken")]
    access_token: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SessionUser {
    id: String,
    email: String,
}

struct HttpResponse {
    body: Vec<u8>,
    retries: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    Dead,
    RateLimited,
    Transient,
    Permanent,
    Cancelled,
    Deadline,
}

impl FailureKind {
    fn can_failover(self) -> bool {
        matches!(self, Self::RateLimited | Self::Transient)
    }

    fn status(self) -> ChatGptProbeStatus {
        match self {
            Self::Dead => ChatGptProbeStatus::Dead,
            Self::RateLimited => ChatGptProbeStatus::RateLimited,
            Self::Transient | Self::Permanent | Self::Cancelled | Self::Deadline => {
                ChatGptProbeStatus::Error
            }
        }
    }
}

struct HttpFailure {
    kind: FailureKind,
    retries: usize,
}

async fn get_with_retry(
    client: &Client,
    endpoint: ChatGptEndpoint,
    url: &str,
    auth: &PreparedChatGptAuth,
    bearer: Option<&str>,
    retries: u8,
    control: &dyn ProbeControl,
    deadline: Instant,
) -> Result<HttpResponse, HttpFailure> {
    let attempts = usize::from(retries) + 1;
    for attempt in 0..attempts {
        if control.is_cancelled() {
            return Err(HttpFailure {
                kind: FailureKind::Cancelled,
                retries: attempt,
            });
        }
        if Instant::now() >= deadline {
            return Err(HttpFailure {
                kind: FailureKind::Deadline,
                retries: attempt,
            });
        }
        if attempt > 0 {
            let backoff = Duration::from_secs((1_u64 << (attempt - 1).min(3)).min(12));
            cancellable_until(control, deadline, tokio::time::sleep(backoff))
                .await
                .map_err(|kind| HttpFailure {
                    kind,
                    retries: attempt,
                })?;
        }
        match get_once(client, endpoint, url, auth, bearer, control, deadline).await {
            Ok(mut response) => {
                response.retries = attempt;
                return Ok(response);
            }
            Err(kind) if attempt + 1 < attempts && kind.can_failover() => {}
            Err(kind) => {
                return Err(HttpFailure {
                    kind,
                    retries: attempt,
                });
            }
        }
    }
    Err(HttpFailure {
        kind: FailureKind::Transient,
        retries: usize::from(retries),
    })
}

async fn get_once(
    client: &Client,
    endpoint: ChatGptEndpoint,
    url: &str,
    auth: &PreparedChatGptAuth,
    bearer: Option<&str>,
    control: &dyn ProbeControl,
    deadline: Instant,
) -> Result<HttpResponse, FailureKind> {
    let now = now_unix().ok_or(FailureKind::Permanent)?;
    let mut request = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json, text/plain, */*")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Referer", HOME_URL)
        .header("Origin", "https://chatgpt.com")
        .header("oai-language", "en-US");
    if let Some(cookie_header) = auth
        .cookie_header_for(endpoint, now)
        .map_err(|_| FailureKind::Permanent)?
    {
        request = request.header("Cookie", cookie_header);
    } else if bearer.is_none_or(str::is_empty) {
        return Err(FailureKind::Permanent);
    }
    if let Some(device_id) = auth.device_id_for(endpoint, now) {
        request = request.header("oai-device-id", device_id);
    }
    if let Some(token) = bearer.filter(|token| !token.is_empty()) {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let response = cancellable_until(control, deadline, request.send())
        .await?
        .map_err(|_| FailureKind::Transient)?;
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(FailureKind::Permanent);
    }
    let body = cancellable_until(control, deadline, read_bounded_body(response)).await??;
    if is_cloudflare_body(status, &body) || status == 429 {
        return Err(FailureKind::RateLimited);
    }
    match status {
        200 => Ok(HttpResponse { body, retries: 0 }),
        401 | 403 => Err(FailureKind::Dead),
        500..=599 => Err(FailureKind::Transient),
        _ => Err(FailureKind::Permanent),
    }
}

async fn fetch_plan(
    client: &Client,
    auth: &PreparedChatGptAuth,
    access_token: &str,
    retries: u8,
    control: &dyn ProbeControl,
    deadline: Instant,
) -> Result<(ChatGptPlanLookup, usize), HttpFailure> {
    let mut retried = 0usize;
    let mut saw_unknown = false;
    let attempts = [
        (
            ChatGptEndpoint::Accounts,
            ACCOUNTS_URL,
            (!access_token.is_empty()).then_some(access_token),
        ),
        (ChatGptEndpoint::Accounts, ACCOUNTS_URL, None),
        (
            ChatGptEndpoint::Me,
            ME_URL,
            (!access_token.is_empty()).then_some(access_token),
        ),
        (ChatGptEndpoint::Me, ME_URL, None),
    ];

    for (endpoint, url, bearer) in attempts {
        match get_with_retry(
            client, endpoint, url, auth, bearer, retries, control, deadline,
        )
        .await
        {
            Ok(response) => {
                retried = retried.saturating_add(response.retries);
                match classify_plan_body(&response.body) {
                    known @ ChatGptPlanLookup::Known(_) => return Ok((known, retried)),
                    ChatGptPlanLookup::Unknown => saw_unknown = true,
                    ChatGptPlanLookup::Unavailable => {}
                }
            }
            Err(error) if error.kind == FailureKind::Cancelled => return Err(error),
            Err(error) => {
                retried = retried.saturating_add(error.retries);
                if error.kind == FailureKind::Deadline {
                    break;
                }
            }
        }
    }
    Ok((
        if saw_unknown {
            ChatGptPlanLookup::Unknown
        } else {
            ChatGptPlanLookup::Unavailable
        },
        retried,
    ))
}

fn classify_plan_body(body: &[u8]) -> ChatGptPlanLookup {
    serde_json::from_slice::<Value>(body)
        .map(|root| extract_plan(&root))
        .unwrap_or(ChatGptPlanLookup::Unavailable)
}

#[cfg(test)]
fn classify_plan_response(status: u16, body: &[u8]) -> ChatGptPlanLookup {
    if status == 200 {
        classify_plan_body(body)
    } else {
        ChatGptPlanLookup::Unavailable
    }
}

fn extract_plan(root: &Value) -> ChatGptPlanLookup {
    let Some(root_object) = root.as_object() else {
        return ChatGptPlanLookup::Unknown;
    };
    if let Some(accounts) = root_object.get("accounts").and_then(Value::as_object) {
        let mut best = None;
        let mut saw_unknown = false;
        for account in accounts.values() {
            match plan_from_account(account) {
                ChatGptPlanLookup::Known(candidate)
                    if best.is_none_or(|current| plan_rank(candidate) > plan_rank(current)) =>
                {
                    best = Some(candidate);
                }
                ChatGptPlanLookup::Known(_) => {}
                ChatGptPlanLookup::Unknown | ChatGptPlanLookup::Unavailable => saw_unknown = true,
            }
        }
        return match best {
            Some(plan) if plan != ChatGptPlan::Free || !saw_unknown => {
                ChatGptPlanLookup::Known(plan)
            }
            _ => ChatGptPlanLookup::Unknown,
        };
    }
    if root_object.contains_key("entitlement") || root_object.contains_key("account") {
        return plan_from_account(root);
    }
    ChatGptPlanLookup::Unknown
}

fn plan_from_account(value: &Value) -> ChatGptPlanLookup {
    let entitlement = value.get("entitlement").and_then(Value::as_object);
    let account = value.get("account").and_then(Value::as_object);
    let subscription = entitlement
        .and_then(|node| first_string(node, &["subscription_plan", "subscription_plan_name"]));
    let active = entitlement
        .and_then(|node| node.get("has_active_subscription"))
        .and_then(Value::as_bool);
    let plan_type = account.and_then(|node| first_string(node, &["plan_type", "planType"]));

    if active == Some(false) {
        for candidate in [
            plan_type.and_then(normalize_plan),
            subscription.and_then(normalize_plan),
        ]
        .into_iter()
        .flatten()
        {
            if matches!(candidate, ChatGptPlan::Team | ChatGptPlan::Enterprise) {
                return ChatGptPlanLookup::Known(candidate);
            }
        }
        // An explicit negative entitlement confirms that a personal account currently
        // has no paid subscription. This is the only implicit route to a Free verdict.
        return ChatGptPlanLookup::Known(ChatGptPlan::Free);
    }
    if let Some(subscription) = subscription {
        return normalize_plan(subscription)
            .map(ChatGptPlanLookup::Known)
            .unwrap_or(ChatGptPlanLookup::Unknown);
    }
    plan_type
        .and_then(normalize_plan)
        .map(ChatGptPlanLookup::Known)
        .unwrap_or(ChatGptPlanLookup::Unknown)
}

fn first_string<'a>(object: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn normalize_plan(raw: &str) -> Option<ChatGptPlan> {
    let normalized: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !matches!(character, ' ' | '_' | '-'))
        .collect();
    // Paid tiers are matched before explicit Free so a paid free-trial is not demoted.
    if normalized.contains("team") {
        Some(ChatGptPlan::Team)
    } else if normalized.contains("enterprise") {
        Some(ChatGptPlan::Enterprise)
    } else if normalized == "go"
        || normalized.contains("chatgptgo")
        || normalized.ends_with("goplan")
    {
        Some(ChatGptPlan::Go)
    } else if normalized.contains("pro") && !normalized.contains("plus") {
        Some(ChatGptPlan::Pro)
    } else if normalized.contains("plus") || normalized == "paid" {
        Some(ChatGptPlan::Plus)
    } else if matches!(normalized.as_str(), "free" | "freeplan" | "chatgptfreeplan") {
        Some(ChatGptPlan::Free)
    } else {
        None
    }
}

fn plan_rank(plan: ChatGptPlan) -> u8 {
    match plan {
        ChatGptPlan::Enterprise => 40,
        ChatGptPlan::Team => 30,
        ChatGptPlan::Pro => 20,
        ChatGptPlan::Plus => 10,
        ChatGptPlan::Go => 5,
        ChatGptPlan::Free => 0,
    }
}

async fn cancellable_until<F, T>(
    control: &dyn ProbeControl,
    deadline: Instant,
    future: F,
) -> Result<T, FailureKind>
where
    F: std::future::Future<Output = T>,
{
    if control.is_cancelled() {
        return Err(FailureKind::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(FailureKind::Deadline);
    }
    tokio::pin!(future);
    let deadline = tokio::time::Instant::from_std(deadline);
    tokio::select! {
        biased;
        _ = cancellation_watcher(control) => Err(FailureKind::Cancelled),
        _ = tokio::time::sleep_until(deadline) => Err(FailureKind::Deadline),
        output = &mut future => {
            if control.is_cancelled() {
                Err(FailureKind::Cancelled)
            } else {
                Ok(output)
            }
        },
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

fn now_unix() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

fn is_cloudflare_body(status: u16, body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    text.contains("just a moment")
        || text.contains("cf-browser-verification")
        || text.contains("cf-challenge")
        || text.contains("enable javascript and cookies")
        || text.contains("_cf_chl")
        // Cloudflare edge/firewall blocks (e.g. "error code: 1020" for an IP/ASN block)
        // arrive as a minimal plaintext 403 body. Treat them as a block/rate-limit so the
        // check fails over to another proxy instead of condemning the account as Dead.
        || text.contains("error code: 10")
        || text.contains("attention required")
        || (text.contains("cloudflare") && text.contains("challenge"))
        || (status == 403 && text.contains("cloudflare"))
        || (status == 403
            && text.contains("<html")
            && (text.contains("scale-appear") || text.contains("cf-")))
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering as AtomicOrdering},
        },
        task::{Context, Poll},
        thread,
    };

    #[test]
    fn plan_extraction_matches_the_go_contract() {
        let plus = json!({
            "accounts": {
                "default": {
                    "account": {"plan_type": "plus"},
                    "entitlement": {
                        "has_active_subscription": true,
                        "subscription_plan": "chatgptplusplan"
                    }
                }
            }
        });
        assert_eq!(
            extract_plan(&plus),
            ChatGptPlanLookup::Known(ChatGptPlan::Plus)
        );

        let expired = json!({
            "accounts": {
                "default": {
                    "account": {"plan_type": "plus"},
                    "entitlement": {
                        "has_active_subscription": false,
                        "subscription_plan": "chatgptplusplan"
                    }
                }
            }
        });
        assert_eq!(
            extract_plan(&expired),
            ChatGptPlanLookup::Known(ChatGptPlan::Free)
        );
        assert_eq!(normalize_plan("chatgptprolite"), Some(ChatGptPlan::Pro));
        assert_eq!(normalize_plan("chatgptgoplan"), Some(ChatGptPlan::Go));
    }

    #[test]
    fn cloudflare_edge_block_is_treated_as_block_not_dead() {
        // Minimal plaintext firewall blocks (1020 IP/ASN, 1015 rate limit) must be caught
        // so the probe fails over instead of classifying the account Dead.
        assert!(is_cloudflare_body(403, b"error code: 1020"));
        assert!(is_cloudflare_body(403, b"error code: 1015"));
        assert!(is_cloudflare_body(403, b"Attention Required! | Cloudflare"));
        assert!(is_cloudflare_body(
            403,
            b"<html>Sorry, you have been blocked by Cloudflare"
        ));
        // A genuine authentication failure stays Dead rather than being misread as a block.
        assert!(!is_cloudflare_body(
            403,
            br#"{"detail":"Could not parse token"}"#
        ));
        assert!(!is_cloudflare_body(200, br#"{"user":{"id":"abc"}}"#));
    }

    #[test]
    fn paid_trial_plan_is_not_demoted_to_free() {
        assert_eq!(
            normalize_plan("chatgpt plus plan free trial"),
            Some(ChatGptPlan::Plus)
        );
        assert_eq!(normalize_plan("pro_free_trial"), Some(ChatGptPlan::Pro));
        assert_eq!(normalize_plan("team free trial"), Some(ChatGptPlan::Team));
        assert_eq!(normalize_plan("free"), Some(ChatGptPlan::Free));
        assert_eq!(normalize_plan(""), None);
    }

    #[test]
    fn inactive_seat_preserves_business_tier_from_subscription_plan() {
        let team_seat = json!({
            "accounts": {
                "default": {
                    "account": {"plan_type": "free"},
                    "entitlement": {
                        "has_active_subscription": false,
                        "subscription_plan": "chatgptteamplan"
                    }
                }
            }
        });
        assert_eq!(
            extract_plan(&team_seat),
            ChatGptPlanLookup::Known(ChatGptPlan::Team)
        );

        // A lapsed personal subscription (no business tier) still demotes to Free.
        let lapsed = json!({
            "accounts": {
                "default": {
                    "account": {"plan_type": "plus"},
                    "entitlement": {
                        "has_active_subscription": false,
                        "subscription_plan": "chatgptplusplan"
                    }
                }
            }
        });
        assert_eq!(
            extract_plan(&lapsed),
            ChatGptPlanLookup::Known(ChatGptPlan::Free)
        );
    }

    #[test]
    fn unavailable_and_unknown_plan_responses_never_become_free() {
        assert_eq!(
            classify_plan_response(429, br#"{"error":"rate limited"}"#),
            ChatGptPlanLookup::Unavailable
        );
        assert_eq!(
            classify_plan_response(500, br#"{"error":"temporary"}"#),
            ChatGptPlanLookup::Unavailable
        );
        assert_eq!(
            classify_plan_response(200, b"not-json"),
            ChatGptPlanLookup::Unavailable
        );

        let future_tier = json!({
            "accounts": {
                "default": {
                    "account": {"plan_type": "ultra-2027"},
                    "entitlement": {
                        "has_active_subscription": true,
                        "subscription_plan": "chatgpt-ultra-2027"
                    }
                }
            }
        });
        assert_eq!(extract_plan(&future_tier), ChatGptPlanLookup::Unknown);

        let explicit_free = json!({
            "accounts": {
                "default": {
                    "account": {"plan_type": "free"},
                    "entitlement": {"has_active_subscription": false}
                }
            }
        });
        assert_eq!(
            extract_plan(&explicit_free),
            ChatGptPlanLookup::Known(ChatGptPlan::Free)
        );
    }

    #[test]
    fn proxy_failover_is_bounded_and_wraps_round_robin_order() {
        assert_eq!(proxy_attempt_indices(0, 1), vec![0]);
        assert_eq!(proxy_attempt_indices(1, 3), vec![1, 2, 0]);
        assert_eq!(proxy_attempt_indices(4, 7), vec![4, 5, 6, 0]);
        assert!(FailureKind::Transient.can_failover());
        assert!(FailureKind::RateLimited.can_failover());
        assert!(!FailureKind::Dead.can_failover());
        assert!(!FailureKind::Permanent.can_failover());
    }

    struct TestControl {
        cancelled: AtomicBool,
    }

    impl ProbeControl for TestControl {
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(AtomicOrdering::Acquire)
        }

        fn wait_cancelled(&self, duration: Duration) -> bool {
            let started = Instant::now();
            while !self.is_cancelled() && started.elapsed() < duration {
                thread::yield_now();
            }
            self.is_cancelled()
        }
    }

    struct PendingRequest {
        dropped: Arc<AtomicBool>,
    }

    impl Future for PendingRequest {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    impl Drop for PendingRequest {
        fn drop(&mut self) {
            self.dropped.store(true, AtomicOrdering::Release);
        }
    }

    #[test]
    fn cancellation_aborts_in_flight_work_without_a_late_result() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        let control = Arc::new(TestControl {
            cancelled: AtomicBool::new(false),
        });
        let dropped = Arc::new(AtomicBool::new(false));
        let cancelling = Arc::clone(&control);
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            cancelling.cancelled.store(true, AtomicOrdering::Release);
        });

        let started = Instant::now();
        let result = runtime.block_on(cancellable_until(
            control.as_ref(),
            Instant::now() + Duration::from_secs(5),
            PendingRequest {
                dropped: Arc::clone(&dropped),
            },
        ));
        worker.join().expect("cancellation thread");
        assert_eq!(result, Err(FailureKind::Cancelled));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(dropped.load(AtomicOrdering::Acquire));
    }

    #[test]
    fn per_artifact_deadline_aborts_in_flight_work() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        let control = TestControl {
            cancelled: AtomicBool::new(false),
        };
        let dropped = Arc::new(AtomicBool::new(false));
        let result = runtime.block_on(cancellable_until(
            &control,
            Instant::now() + Duration::from_millis(25),
            PendingRequest {
                dropped: Arc::clone(&dropped),
            },
        ));
        assert_eq!(result, Err(FailureKind::Deadline));
        assert!(dropped.load(AtomicOrdering::Acquire));
    }
}
