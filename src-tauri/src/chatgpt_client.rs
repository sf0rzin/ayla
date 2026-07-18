use crate::{auth_artifact::PreparedChatGptAuth, proxy_store::StoredProxy};
use serde::Deserialize;
use serde_json::Value;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Duration,
};
use ureq::{Agent, Proxy, ProxyProtocol};

const SESSION_URL: &str = "https://chatgpt.com/api/auth/session";
const ACCOUNTS_URL: &str = "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const ME_URL: &str = "https://chatgpt.com/backend-api/me";
const HOME_URL: &str = "https://chatgpt.com/";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROXY_ATTEMPTS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatGptPlan {
    Free,
    Go,
    Plus,
    Pro,
    Team,
    Enterprise,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatGptProbeStatus {
    Active(ChatGptPlan),
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
    fn check(&self, auth: &PreparedChatGptAuth) -> ChatGptProbeResult;
}

pub(crate) struct ChatGptProbePool {
    agents: Vec<Agent>,
    proxy_count: usize,
    retries: u8,
    next_agent: AtomicUsize,
}

impl ChatGptProbePool {
    pub(crate) fn new(
        timeout_ms: u64,
        retries: u8,
        proxies: &[StoredProxy],
    ) -> Result<Self, String> {
        let timeout = Duration::from_millis(timeout_ms.clamp(3_000, 120_000));
        let agents = if proxies.is_empty() {
            vec![build_agent(timeout, None)?]
        } else {
            let mut agents = Vec::with_capacity(proxies.len());
            let mut last_error = None;
            for stored in proxies {
                match proxy_from_stored(stored).and_then(|proxy| build_agent(timeout, Some(proxy)))
                {
                    Ok(agent) => agents.push(agent),
                    Err(error) => last_error = Some(error),
                }
            }
            if agents.is_empty() {
                return Err(
                    last_error.unwrap_or_else(|| "no usable proxy is available".to_string())
                );
            }
            agents
        };
        let proxy_count = if proxies.is_empty() { 0 } else { agents.len() };
        Ok(Self {
            agents,
            proxy_count,
            retries: retries.min(5),
            next_agent: AtomicUsize::new(0),
        })
    }

    pub(crate) fn proxy_count(&self) -> usize {
        self.proxy_count
    }
}

impl ChatGptProber for ChatGptProbePool {
    fn check(&self, auth: &PreparedChatGptAuth) -> ChatGptProbeResult {
        let start = self.next_agent.fetch_add(1, Ordering::Relaxed) % self.agents.len();
        let mut previous_retries = 0usize;

        for (attempt, index) in proxy_attempt_indices(start, self.agents.len())
            .into_iter()
            .enumerate()
        {
            match self.check_with_agent(&self.agents[index], auth) {
                Ok(mut result) => {
                    result.retries = result.retries.saturating_add(previous_retries);
                    return result;
                }
                Err(failure) => {
                    previous_retries = previous_retries.saturating_add(failure.retries);
                    if attempt + 1 < self.agents.len().min(MAX_PROXY_ATTEMPTS) {
                        previous_retries = previous_retries.saturating_add(1);
                        continue;
                    }
                    return ChatGptProbeResult {
                        status: match failure.kind {
                            FailureKind::RateLimited => ChatGptProbeStatus::RateLimited,
                            FailureKind::Transient => ChatGptProbeStatus::Error,
                            FailureKind::Dead => ChatGptProbeStatus::Dead,
                            FailureKind::Permanent => ChatGptProbeStatus::Error,
                        },
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
    fn check_with_agent(
        &self,
        agent: &Agent,
        auth: &PreparedChatGptAuth,
    ) -> Result<ChatGptProbeResult, HttpFailure> {
        let session = match get_with_retry(agent, SESSION_URL, auth, None, self.retries) {
            Ok(response) => response,
            Err(failure) if failure.kind.can_failover() => return Err(failure),
            Err(failure) => {
                return Ok(ChatGptProbeResult {
                    status: match failure.kind {
                        FailureKind::Dead => ChatGptProbeStatus::Dead,
                        FailureKind::RateLimited => ChatGptProbeStatus::RateLimited,
                        FailureKind::Transient | FailureKind::Permanent => {
                            ChatGptProbeStatus::Error
                        }
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

        let (plan, plan_retries) = fetch_plan(agent, auth, &session.access_token, self.retries);
        retries = retries.saturating_add(plan_retries);
        Ok(ChatGptProbeResult {
            status: ChatGptProbeStatus::Active(plan),
            retries,
        })
    }
}

fn proxy_attempt_indices(start: usize, pool_size: usize) -> Vec<usize> {
    (0..pool_size.min(MAX_PROXY_ATTEMPTS))
        .map(|offset| (start + offset) % pool_size)
        .collect()
}

fn build_agent(timeout: Duration, proxy: Option<Proxy>) -> Result<Agent, String> {
    let config = Agent::config_builder()
        .proxy(proxy)
        .https_only(true)
        .http_status_as_error(false)
        .max_redirects(8)
        .timeout_global(Some(timeout))
        .build();
    Ok(config.into())
}

fn proxy_from_stored(stored: &StoredProxy) -> Result<Proxy, String> {
    let protocol = match stored.protocol.as_str() {
        "http" => ProxyProtocol::Http,
        "socks4" => ProxyProtocol::Socks4A,
        "socks5" => ProxyProtocol::Socks5h,
        _ => return Err("unsupported proxy protocol".to_string()),
    };
    let mut builder = Proxy::builder(protocol)
        .host(&stored.host)
        .port(stored.port);
    if let Some(username) = stored.username.as_deref() {
        builder = builder.username(username);
    }
    if let Some(password) = stored.password.as_deref() {
        builder = builder.password(password);
    }
    builder
        .build()
        .map_err(|_| "the configured proxy is invalid".to_string())
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    Dead,
    RateLimited,
    Transient,
    Permanent,
}

impl FailureKind {
    fn can_failover(self) -> bool {
        matches!(self, Self::RateLimited | Self::Transient)
    }
}

struct HttpFailure {
    kind: FailureKind,
    retries: usize,
}

fn get_with_retry(
    agent: &Agent,
    url: &str,
    auth: &PreparedChatGptAuth,
    bearer: Option<&str>,
    retries: u8,
) -> Result<HttpResponse, HttpFailure> {
    let attempts = usize::from(retries) + 1;
    for attempt in 0..attempts {
        if attempt > 0 {
            thread::sleep(Duration::from_secs((1_u64 << (attempt - 1).min(3)).min(12)));
        }
        match get_once(agent, url, auth, bearer) {
            Ok(mut response) => {
                response.retries = attempt;
                return Ok(response);
            }
            Err(kind)
                if attempt + 1 < attempts
                    && matches!(kind, FailureKind::RateLimited | FailureKind::Transient) => {}
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

fn get_once(
    agent: &Agent,
    url: &str,
    auth: &PreparedChatGptAuth,
    bearer: Option<&str>,
) -> Result<HttpResponse, FailureKind> {
    let mut request = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json, text/plain, */*")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Referer", HOME_URL)
        .header("Origin", "https://chatgpt.com")
        .header("oai-language", "en-US")
        .header("Cookie", auth.cookie_header());
    if let Some(device_id) = auth.device_id() {
        request = request.header("oai-device-id", device_id);
    }
    if let Some(token) = bearer.filter(|token| !token.is_empty()) {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let mut response = request.call().map_err(|_| FailureKind::Transient)?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_vec()
        .map_err(|_| FailureKind::Transient)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(FailureKind::Permanent);
    }
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

fn fetch_plan(
    agent: &Agent,
    auth: &PreparedChatGptAuth,
    access_token: &str,
    retries: u8,
) -> (ChatGptPlan, usize) {
    let mut retried = 0usize;
    if !access_token.is_empty() {
        match get_with_retry(agent, ACCOUNTS_URL, auth, Some(access_token), retries) {
            Ok(response) => {
                retried = retried.saturating_add(response.retries);
                if let Ok(root) = serde_json::from_slice::<Value>(&response.body) {
                    return (extract_plan(&root), retried);
                }
            }
            Err(error) => retried = retried.saturating_add(error.retries),
        }
    }
    match get_with_retry(agent, ACCOUNTS_URL, auth, None, retries) {
        Ok(response) => {
            retried = retried.saturating_add(response.retries);
            if let Ok(root) = serde_json::from_slice::<Value>(&response.body) {
                return (extract_plan(&root), retried);
            }
        }
        Err(error) => retried = retried.saturating_add(error.retries),
    }

    for bearer in [Some(access_token), None] {
        if bearer.is_some_and(str::is_empty) {
            continue;
        }
        match get_with_retry(agent, ME_URL, auth, bearer, retries) {
            Ok(response) => {
                retried = retried.saturating_add(response.retries);
                return (ChatGptPlan::Free, retried);
            }
            Err(error) => retried = retried.saturating_add(error.retries),
        }
    }
    (ChatGptPlan::Free, retried)
}

fn extract_plan(root: &Value) -> ChatGptPlan {
    let Some(root_object) = root.as_object() else {
        return ChatGptPlan::Free;
    };
    let mut best = ChatGptPlan::Free;
    if let Some(accounts) = root_object.get("accounts").and_then(Value::as_object) {
        if let Some(default) = accounts.get("default") {
            best = plan_from_account(default);
        }
        for account in accounts.values() {
            let candidate = plan_from_account(account);
            if plan_rank(candidate) > plan_rank(best) {
                best = candidate;
            }
        }
        return best;
    }
    if root_object.contains_key("entitlement") || root_object.contains_key("account") {
        return plan_from_account(root);
    }
    ChatGptPlan::Free
}

fn plan_from_account(value: &Value) -> ChatGptPlan {
    let entitlement = value.get("entitlement").and_then(Value::as_object);
    let account = value.get("account").and_then(Value::as_object);
    let subscription = entitlement
        .and_then(|node| first_string(node, &["subscription_plan", "subscription_plan_name"]));
    let active = entitlement
        .and_then(|node| node.get("has_active_subscription"))
        .and_then(Value::as_bool);
    let plan_type = account.and_then(|node| first_string(node, &["plan_type", "planType"]));

    if active == Some(false) {
        let normalized = normalize_plan(plan_type.unwrap_or_default());
        return if matches!(normalized, ChatGptPlan::Team | ChatGptPlan::Enterprise) {
            normalized
        } else {
            ChatGptPlan::Free
        };
    }
    if let Some(subscription) = subscription {
        return normalize_plan(subscription);
    }
    plan_type.map(normalize_plan).unwrap_or(ChatGptPlan::Free)
}

fn first_string<'a>(object: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn normalize_plan(raw: &str) -> ChatGptPlan {
    let normalized: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !matches!(character, ' ' | '_' | '-'))
        .collect();
    if normalized.contains("free") || matches!(normalized.as_str(), "" | "null" | "none" | "noplan")
    {
        ChatGptPlan::Free
    } else if normalized.contains("team") {
        ChatGptPlan::Team
    } else if normalized.contains("enterprise") {
        ChatGptPlan::Enterprise
    } else if normalized == "go"
        || normalized.contains("chatgptgo")
        || normalized.ends_with("goplan")
    {
        ChatGptPlan::Go
    } else if normalized.contains("pro") && !normalized.contains("plus") {
        ChatGptPlan::Pro
    } else if normalized.contains("plus") || normalized == "paid" {
        ChatGptPlan::Plus
    } else {
        ChatGptPlan::Free
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

fn is_cloudflare_body(status: u16, body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    text.contains("just a moment")
        || text.contains("cf-browser-verification")
        || text.contains("cf-challenge")
        || text.contains("enable javascript and cookies")
        || text.contains("_cf_chl")
        || (text.contains("cloudflare") && text.contains("challenge"))
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
        assert!(extract_plan(&plus) == ChatGptPlan::Plus);

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
        assert!(extract_plan(&expired) == ChatGptPlan::Free);
        assert!(normalize_plan("chatgptprolite") == ChatGptPlan::Pro);
        assert!(normalize_plan("chatgptgoplan") == ChatGptPlan::Go);
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
}
