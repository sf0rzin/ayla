use crate::proxy_store::{CheckLease, CheckUpdate, ProxyItem, ProxyManager, StoredProxy};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use ureq::{Proxy, ProxyProtocol};

const DEFAULT_THREADS: usize = 40;
const MAX_THREADS: usize = 200;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const ATTEMPTS: usize = 2;
const MAX_RESPONSE_BYTES: usize = 32 * 1024;
const GEO_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_GEO_CACHE_ENTRIES: usize = 10_000;
const SECURE_JUDGES: [&str; 2] = [
    "https://api.ipify.org/",
    "https://api.proxyscrape.com/ip.php",
];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CheckProxiesRequest {
    pub ids: Vec<String>,
    pub threads: usize,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckProxiesResponse {
    pub results: Vec<ProxyItem>,
    pub done: usize,
    pub live: usize,
    pub http_only: usize,
    pub failed: usize,
    /// Retained for wire compatibility. Health checks no longer delete proxies.
    pub removed: usize,
    pub total: usize,
    pub stopped: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyProgressEvent {
    done: usize,
    total: usize,
    percent: f64,
    live: usize,
    http_only: usize,
    failed: usize,
    removed: usize,
    item: Option<ProxyItem>,
    id: String,
    proxy: String,
    status: String,
    running: bool,
}

#[derive(Debug)]
struct CheckOutcome {
    live: bool,
    http_only: bool,
    latency_ms: u64,
    message: String,
    ip: String,
    country: String,
    country_code: String,
    city: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct GeoResponse {
    status: String,
    country: String,
    country_code: String,
    city: String,
}

#[derive(Clone)]
struct GeoLocation {
    country: String,
    country_code: String,
    city: String,
}

#[derive(Clone, Copy)]
struct Judge {
    host: &'static str,
    path: &'static str,
}

const JUDGES: [Judge; 4] = [
    Judge {
        host: "api.ipify.org",
        path: "/",
    },
    Judge {
        host: "checkip.amazonaws.com",
        path: "/",
    },
    Judge {
        host: "ipv4.icanhazip.com",
        path: "/",
    },
    Judge {
        host: "ident.me",
        path: "/",
    },
];

static JUDGE_OFFSET: AtomicUsize = AtomicUsize::new(0);

pub fn run_check(
    app: &AppHandle,
    manager: &ProxyManager,
    request: CheckProxiesRequest,
) -> Result<CheckProxiesResponse, String> {
    let lease = manager.begin_check()?;
    let result = run_check_inner(app, manager, &lease, request);
    manager.finish_check(&lease.run_id);
    result
}

fn run_check_inner(
    app: &AppHandle,
    manager: &ProxyManager,
    lease: &CheckLease,
    request: CheckProxiesRequest,
) -> Result<CheckProxiesResponse, String> {
    let targets = manager.targets(&request.ids)?;
    let total = targets.len();
    let threads = if request.threads == 0 {
        DEFAULT_THREADS
    } else {
        request.threads.clamp(1, MAX_THREADS)
    };
    let timeout_ms = if request.timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        request.timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
    };
    let timeout = Duration::from_millis(timeout_ms);

    emit_progress(
        app,
        ProxyProgressEvent {
            done: 0,
            total,
            percent: 0.0,
            live: 0,
            http_only: 0,
            failed: 0,
            removed: 0,
            item: None,
            id: String::new(),
            proxy: String::new(),
            status: "running".to_string(),
            running: total > 0,
        },
    );

    let jobs = Arc::new(targets);
    let next_job = Arc::new(AtomicUsize::new(0));
    let worker_count = threads.min(total);
    let channel_capacity = (worker_count * 2).max(1);
    let (sender, receiver) = mpsc::sync_channel(channel_capacity);
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let jobs = Arc::clone(&jobs);
        let next_job = Arc::clone(&next_job);
        let sender = sender.clone();
        let cancellation = Arc::clone(&lease.cancellation);
        workers.push(thread::spawn(move || {
            loop {
                if cancellation.load(Ordering::Acquire) {
                    break;
                }
                let position = next_job.fetch_add(1, Ordering::Relaxed);
                let Some(item) = jobs.get(position) else {
                    break;
                };
                let id = item.id.clone();
                let display = item.item().display;
                let mut outcome = check_one(item, timeout, &cancellation);
                if (outcome.live || outcome.http_only) && !cancellation.load(Ordering::Acquire) {
                    if item.ip == outcome.ip
                        && (!item.country.is_empty()
                            || !item.country_code.is_empty()
                            || !item.city.is_empty())
                    {
                        outcome.country = item.country.clone();
                        outcome.country_code = item.country_code.clone();
                        outcome.city = item.city.clone();
                    } else if let Some(location) = lookup_geo(&outcome.ip) {
                        outcome.country = location.country;
                        outcome.country_code = location.country_code;
                        outcome.city = location.city;
                    }
                }
                if cancellation.load(Ordering::Acquire) {
                    break;
                }
                if sender.send((id, display, outcome)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(sender);

    let mut done = 0;
    let mut live = 0;
    let mut http_only = 0;
    let mut failed = 0;
    let mut run_error = None;

    for (id, display, outcome) in receiver {
        if lease.cancellation.load(Ordering::Acquire) || run_error.is_some() {
            continue;
        }

        match manager.apply_check(
            &id,
            CheckUpdate {
                live: outcome.live,
                http_only: outcome.http_only,
                latency_ms: outcome.latency_ms,
                message: outcome.message.clone(),
                ip: outcome.ip,
                country: outcome.country,
                country_code: outcome.country_code,
                city: outcome.city,
            },
        ) {
            Ok(item) => {
                done += 1;
                if outcome.live {
                    live += 1;
                } else if outcome.http_only {
                    http_only += 1;
                } else {
                    failed += 1;
                }
                emit_progress(
                    app,
                    ProxyProgressEvent {
                        done,
                        total,
                        percent: percentage(done, total),
                        live,
                        http_only,
                        failed,
                        removed: 0,
                        item,
                        id,
                        proxy: display,
                        status: if outcome.live {
                            "live"
                        } else if outcome.http_only {
                            "httpOnly"
                        } else {
                            "failed"
                        }
                        .to_string(),
                        running: true,
                    },
                );
            }
            Err(error) => {
                lease.cancellation.store(true, Ordering::Release);
                run_error = Some(error);
            }
        }
    }

    for worker in workers {
        let _ = worker.join();
    }
    manager.commit_check()?;
    if let Some(error) = run_error {
        return Err(error);
    }

    let stopped = lease.cancellation.load(Ordering::Acquire);
    let results = manager.snapshot()?;
    let response = CheckProxiesResponse {
        done,
        total,
        results,
        live,
        http_only,
        failed,
        removed: 0,
        stopped,
    };

    emit_progress(
        app,
        ProxyProgressEvent {
            done,
            total,
            percent: percentage(done, total),
            live,
            http_only,
            failed,
            removed: 0,
            item: None,
            id: String::new(),
            proxy: String::new(),
            status: if stopped { "stopped" } else { "done" }.to_string(),
            running: false,
        },
    );
    let _ = app.emit("proxy:done", response.clone());
    Ok(response)
}

fn emit_progress(app: &AppHandle, event: ProxyProgressEvent) {
    let _ = app.emit("proxy:progress", event);
}

fn cancelled_outcome() -> CheckOutcome {
    CheckOutcome {
        live: false,
        http_only: false,
        latency_ms: 0,
        message: "cancelled".to_string(),
        ip: String::new(),
        country: String::new(),
        country_code: String::new(),
        city: String::new(),
    }
}

fn check_one(
    proxy: &StoredProxy,
    timeout: Duration,
    cancellation: &std::sync::atomic::AtomicBool,
) -> CheckOutcome {
    let start = JUDGE_OFFSET.fetch_add(1, Ordering::Relaxed) % JUDGES.len();
    let deadline = Instant::now() + timeout;
    let mut last = CheckOutcome {
        live: false,
        http_only: false,
        latency_ms: 0,
        message: "no response".to_string(),
        ip: String::new(),
        country: String::new(),
        country_code: String::new(),
        city: String::new(),
    };

    let mut https_failure = "HTTPS capability was not verified".to_string();
    if let Some(remaining) = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
    {
        if cancellation.load(Ordering::Acquire) {
            return cancelled_outcome();
        }
        // Always reserve budget for the plain-HTTP/SOCKS probes so the HTTPS attempt cannot
        // consume the entire deadline and starve the only path some proxies can pass.
        let secure_timeout = (remaining / 2).min(Duration::from_secs(6));
        let secure_judge = SECURE_JUDGES[start % SECURE_JUDGES.len()];
        last = probe_https(proxy, secure_judge, secure_timeout);
        if last.live {
            return last;
        }
        https_failure = last.message.clone();
    }

    for attempt in 0..ATTEMPTS {
        for offset in 0..JUDGES.len() {
            if cancellation.load(Ordering::Acquire) {
                return cancelled_outcome();
            }
            let judge = JUDGES[(start + attempt + offset) % JUDGES.len()];
            let Some(remaining) = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
            else {
                last.message = "timed out".to_string();
                return last;
            };
            let mut http_outcome = probe(proxy, judge, remaining);
            if http_outcome.live {
                http_outcome.live = false;
                http_outcome.http_only = true;
                http_outcome.message = format!("HTTP only; HTTPS unavailable ({https_failure})");
                return http_outcome;
            }
            last = http_outcome;
        }
    }
    last
}

fn probe_https(proxy: &StoredProxy, judge: &str, timeout: Duration) -> CheckOutcome {
    let started = Instant::now();
    let result = (|| -> io::Result<String> {
        let proxy = ureq_proxy(proxy)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(Some(proxy))
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(2)
            .timeout_global(Some(timeout))
            .build()
            .into();
        let mut response = agent
            .get(judge)
            .header("Accept", "text/plain")
            .header("User-Agent", "Ayla/0.1")
            .call()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(io::Error::other(format!("HTTPS {status}")));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_vec()
            .map_err(|error| io::Error::other(error.to_string()))?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTPS response is too large",
            ));
        }
        extract_ip(&String::from_utf8_lossy(&body)).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTPS response has no valid IP address",
            )
        })
    })();

    match result {
        Ok(ip) => CheckOutcome {
            live: true,
            http_only: false,
            latency_ms: elapsed_ms(started),
            message: "ok (https)".to_string(),
            ip,
            country: String::new(),
            country_code: String::new(),
            city: String::new(),
        },
        Err(error) => CheckOutcome {
            live: false,
            http_only: false,
            latency_ms: elapsed_ms(started),
            message: io_error(&error),
            ip: String::new(),
            country: String::new(),
            country_code: String::new(),
            city: String::new(),
        },
    }
}

fn ureq_proxy(stored: &StoredProxy) -> io::Result<Proxy> {
    let protocol = match stored.protocol.as_str() {
        "http" => ProxyProtocol::Http,
        "socks4" => ProxyProtocol::Socks4A,
        "socks5" => ProxyProtocol::Socks5h,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported proxy protocol",
            ));
        }
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
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid proxy"))
}

fn probe(proxy: &StoredProxy, judge: Judge, timeout: Duration) -> CheckOutcome {
    let started = Instant::now();
    let result = match proxy.protocol.as_str() {
        "socks5" => request_through_socks5(proxy, judge, timeout),
        "socks4" => request_through_socks4(proxy, judge, timeout),
        _ => request_through_http_proxy(proxy, judge, timeout),
    };

    match result {
        Ok(ip) => CheckOutcome {
            live: true,
            http_only: false,
            latency_ms: elapsed_ms(started),
            message: "ok".to_string(),
            ip,
            country: String::new(),
            country_code: String::new(),
            city: String::new(),
        },
        Err(error) => CheckOutcome {
            live: false,
            http_only: false,
            latency_ms: elapsed_ms(started),
            message: io_error(&error),
            ip: String::new(),
            country: String::new(),
            country_code: String::new(),
            city: String::new(),
        },
    }
}

fn lookup_geo(ip: &str) -> Option<GeoLocation> {
    let IpAddr::V4(address) = ip.trim().parse::<IpAddr>().ok()? else {
        return None;
    };
    static CACHE: OnceLock<Mutex<HashMap<Ipv4Addr, (Instant, Option<GeoLocation>)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some((cached_at, location)) = cache.get(&address)
        && cached_at.elapsed() < GEO_CACHE_TTL
    {
        return location.clone();
    }

    let location = lookup_geo_uncached(address);
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= MAX_GEO_CACHE_ENTRIES {
            cache.retain(|_, (cached_at, _)| cached_at.elapsed() < GEO_CACHE_TTL);
            if cache.len() >= MAX_GEO_CACHE_ENTRIES
                && let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, (cached_at, _))| *cached_at)
                    .map(|(address, _)| *address)
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(address, (Instant::now(), location.clone()));
    }
    location
}

fn lookup_geo_uncached(address: Ipv4Addr) -> Option<GeoLocation> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(2)
            .timeout_global(Some(Duration::from_secs(4)))
            .build()
            .into()
    });
    let url = format!("http://ip-api.com/json/{address}?fields=status,country,countryCode,city");
    let mut response = agent.get(&url).call().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(4_097)
        .read_to_vec()
        .ok()?;
    if body.len() > 4_096 {
        return None;
    }
    let payload: GeoResponse = serde_json::from_slice(&body).ok()?;
    if payload.status != "success" {
        return None;
    }
    Some(GeoLocation {
        country: payload.country,
        country_code: payload.country_code,
        city: payload.city,
    })
}

fn request_through_http_proxy(
    proxy: &StoredProxy,
    judge: Judge,
    timeout: Duration,
) -> io::Result<String> {
    let mut stream = connect_tcp(&proxy.host, proxy.port, timeout)?;
    let auth = proxy
        .username
        .as_ref()
        .map(|username| {
            let credentials = format!(
                "{}:{}",
                username,
                proxy.password.as_deref().unwrap_or_default()
            );
            format!(
                "Proxy-Authorization: Basic {}\r\n",
                base64(credentials.as_bytes())
            )
        })
        .unwrap_or_default();
    let request = format!(
        "GET http://{}{} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Ayla/0.1\r\nAccept: text/plain\r\n{}Connection: close\r\n\r\n",
        judge.host, judge.path, judge.host, auth
    );
    stream.write_all(request.as_bytes())?;
    read_ip_response(&mut stream)
}

fn request_through_socks5(
    proxy: &StoredProxy,
    judge: Judge,
    timeout: Duration,
) -> io::Result<String> {
    let mut stream = connect_tcp(&proxy.host, proxy.port, timeout)?;
    socks5_handshake(&mut stream, proxy, judge.host, 80)?;
    send_origin_request(&mut stream, judge)
}

fn request_through_socks4(
    proxy: &StoredProxy,
    judge: Judge,
    timeout: Duration,
) -> io::Result<String> {
    let mut stream = connect_tcp(&proxy.host, proxy.port, timeout)?;
    socks4a_handshake(&mut stream, proxy, judge.host, 80)?;
    send_origin_request(&mut stream, judge)
}

fn send_origin_request(stream: &mut TcpStream, judge: Judge) -> io::Result<String> {
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Ayla/0.1\r\nAccept: text/plain\r\nConnection: close\r\n\r\n",
        judge.path, judge.host
    );
    stream.write_all(request.as_bytes())?;
    read_ip_response(stream)
}

fn connect_tcp(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    let addresses = (host, port).to_socket_addrs()?;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                stream.set_read_timeout(Some(timeout))?;
                stream.set_write_timeout(Some(timeout))?;
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "host has no address")))
}

fn socks5_handshake(
    stream: &mut TcpStream,
    proxy: &StoredProxy,
    target_host: &str,
    target_port: u16,
) -> io::Result<()> {
    let has_auth = proxy.username.is_some();
    let methods: &[u8] = if has_auth { &[0x00, 0x02] } else { &[0x00] };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(methods);
    stream.write_all(&greeting)?;

    let mut selection = [0u8; 2];
    stream.read_exact(&mut selection)?;
    if selection[0] != 0x05 || selection[1] == 0xff {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SOCKS5 has no accepted method",
        ));
    }

    if selection[1] == 0x02 {
        if !has_auth {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SOCKS5 requested authentication that was not offered",
            ));
        }
        let username = proxy.username.as_deref().unwrap_or_default().as_bytes();
        let password = proxy.password.as_deref().unwrap_or_default().as_bytes();
        if username.len() > 255 || password.len() > 255 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SOCKS5 credential is too long",
            ));
        }
        let mut auth = vec![0x01, username.len() as u8];
        auth.extend_from_slice(username);
        auth.push(password.len() as u8);
        auth.extend_from_slice(password);
        stream.write_all(&auth)?;
        let mut auth_response = [0u8; 2];
        stream.read_exact(&mut auth_response)?;
        if auth_response[0] != 0x01 || auth_response[1] != 0x00 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SOCKS5 authentication was rejected",
            ));
        }
    } else if selection[1] != 0x00 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsupported SOCKS5 method",
        ));
    }

    let host = target_host.as_bytes();
    if host.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target host is too long",
        ));
    }
    let mut request = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
    request.extend_from_slice(host);
    request.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&request)?;

    let mut response = [0u8; 4];
    stream.read_exact(&mut response)?;
    if response[0] != 0x05 || response[1] != 0x00 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("SOCKS5 rejected the connection ({})", response[1]),
        ));
    }
    let address_length = match response[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length)?;
            usize::from(length[0])
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid SOCKS5 response",
            ));
        }
    };
    let mut ignored = vec![0u8; address_length + 2];
    stream.read_exact(&mut ignored)?;
    Ok(())
}

fn socks4a_handshake(
    stream: &mut TcpStream,
    proxy: &StoredProxy,
    target_host: &str,
    target_port: u16,
) -> io::Result<()> {
    let user = proxy.username.as_deref().unwrap_or("ayla");
    let user: Vec<u8> = user
        .bytes()
        .filter(|byte| *byte != 0 && byte.is_ascii())
        .collect();
    let mut request = vec![0x04, 0x01];
    request.extend_from_slice(&target_port.to_be_bytes());
    request.extend_from_slice(&[0, 0, 0, 1]);
    request.extend_from_slice(&user);
    request.push(0);
    request.extend_from_slice(target_host.as_bytes());
    request.push(0);
    stream.write_all(&request)?;

    let mut response = [0u8; 8];
    stream.read_exact(&mut response)?;
    if response[1] != 90 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("SOCKS4 rejected the connection ({})", response[1]),
        ));
    }
    Ok(())
}

fn read_ip_response(stream: &mut TcpStream) -> io::Result<String> {
    let mut response = Vec::new();
    let mut buffer = [0u8; 2048];
    while response.len() < MAX_RESPONSE_BYTES {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = MAX_RESPONSE_BYTES - response.len();
                response.extend_from_slice(&buffer[..read.min(remaining)]);
            }
            Err(error)
                if !response.is_empty()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    let response = String::from_utf8_lossy(&response);
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "incomplete HTTP response"))?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP status"))?;
    // Require an exact 200: a 3xx redirect to a captive portal is not a working exit, and
    // its body must not be mined for an incidental IP-looking token.
    if status != 200 {
        return Err(io::Error::other(format!("HTTP {status}")));
    }
    extract_ip(body).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "response has no valid IP address",
        )
    })
}

fn extract_ip(body: &str) -> Option<String> {
    body.split(|character: char| {
        character.is_ascii_whitespace()
            || matches!(
                character,
                '"' | '\'' | '{' | '}' | '[' | ']' | '(' | ')' | ','
            )
    })
    .filter_map(|token| {
        let token = token.trim_matches(|character: char| {
            !character.is_ascii_hexdigit() && character != '.' && character != ':'
        });
        token.parse::<IpAddr>().ok().filter(is_public_ip)
    })
    .next()
    .map(|address| address.to_string())
}

// Only a routable public address is a plausible proxy exit IP. Private, loopback,
// link-local, carrier-grade-NAT and other reserved ranges typically come from a captive
// portal or gateway error page returned by a proxy that does not actually forward traffic.
fn is_public_ip(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            !v4.is_private()
                && !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_broadcast()
                && !v4.is_unspecified()
                && !v4.is_multicast()
                && octets[0] != 0
                && !(octets[0] == 100 && (64..128).contains(&octets[1]))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !v6.is_loopback()
                && !v6.is_unspecified()
                && !v6.is_multicast()
                && (first & 0xfe00) != 0xfc00
                && (first & 0xffc0) != 0xfe80
        }
    }
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn io_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "timed out".to_string(),
        io::ErrorKind::PermissionDenied => "authentication rejected".to_string(),
        io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
        io::ErrorKind::InvalidData => error.to_string(),
        _ => "connection failed".to_string(),
    }
}

fn percentage(done: usize, total: usize) -> f64 {
    if total == 0 {
        100.0
    } else {
        (done as f64 / total as f64 * 100.0).min(100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn extracts_addresses_from_plain_and_chunked_bodies() {
        assert_eq!(extract_ip("1.2.3.4\n").as_deref(), Some("1.2.3.4"));
        assert_eq!(
            extract_ip("b\r\n2001:db8::1\r\n0\r\n").as_deref(),
            Some("2001:db8::1")
        );
        assert!(extract_ip("not an ip").is_none());
    }

    #[test]
    fn extract_ip_rejects_private_and_reserved_addresses() {
        for reserved in [
            "10.0.0.138",
            "192.168.1.1",
            "172.16.5.4",
            "127.0.0.1",
            "169.254.10.10",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fc00::1",
        ] {
            assert!(
                extract_ip(reserved).is_none(),
                "{reserved} must not be accepted as a public exit IP"
            );
        }
        assert_eq!(extract_ip("8.8.8.8").as_deref(), Some("8.8.8.8"));
        assert_eq!(
            extract_ip("Access blocked by gateway 203.0.113.9").as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn non_200_status_is_not_treated_as_live() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock proxy");
        let address = listener.local_addr().expect("mock address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let _ = read_headers(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: https://portal.example/\r\nContent-Length: 10\r\n\r\n203.0.113.5",
                )
                .expect("send redirect");
        });
        let proxy = mock_proxy("http", address.port());

        let result = request_through_http_proxy(
            &proxy,
            Judge {
                host: "example.test",
                path: "/ip",
            },
            Duration::from_secs(2),
        );
        assert!(
            result.is_err(),
            "a 3xx redirect must not be reported as a live proxy"
        );
        server.join().expect("mock proxy");
    }

    #[test]
    fn basic_auth_encoding_matches_standard_vector() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn percentage_handles_empty_and_caps_values() {
        assert_eq!(percentage(0, 0), 100.0);
        assert_eq!(percentage(1, 4), 25.0);
        assert_eq!(percentage(8, 4), 100.0);
    }

    #[test]
    fn http_proxy_sends_auth_and_reads_public_ip() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock proxy");
        let address = listener.local_addr().expect("mock address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept HTTP proxy request");
            let request = read_headers(&mut stream);
            assert!(request.starts_with("GET http://example.test/ip HTTP/1.1"));
            assert!(request.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n203.0.113.10\n")
                .expect("send HTTP response");
        });
        let proxy = mock_proxy("http", address.port());

        let ip = request_through_http_proxy(
            &proxy,
            Judge {
                host: "example.test",
                path: "/ip",
            },
            Duration::from_secs(2),
        )
        .expect("HTTP proxy check");

        assert_eq!(ip, "203.0.113.10");
        server.join().expect("mock HTTP proxy");
    }

    #[test]
    fn socks5_performs_authenticated_handshake() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock proxy");
        let address = listener.local_addr().expect("mock address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept SOCKS5 request");
            let mut greeting = [0u8; 4];
            stream.read_exact(&mut greeting).expect("read greeting");
            assert_eq!(greeting, [0x05, 0x02, 0x00, 0x02]);
            stream.write_all(&[0x05, 0x02]).expect("select auth");

            let mut auth = [0u8; 11];
            stream.read_exact(&mut auth).expect("read credentials");
            assert_eq!(&auth, b"\x01\x04user\x04pass");
            stream.write_all(&[0x01, 0x00]).expect("accept auth");

            let mut connect = [0u8; 19];
            stream
                .read_exact(&mut connect)
                .expect("read connect request");
            assert_eq!(&connect[..5], &[0x05, 0x01, 0x00, 0x03, 12]);
            assert_eq!(&connect[5..17], b"example.test");
            assert_eq!(&connect[17..], &[0, 80]);
            stream
                .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 80])
                .expect("accept connect");

            let request = read_headers(&mut stream);
            assert!(request.starts_with("GET /ip HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n198.51.100.7")
                .expect("send judge response");
        });
        let proxy = mock_proxy("socks5", address.port());

        let ip = request_through_socks5(
            &proxy,
            Judge {
                host: "example.test",
                path: "/ip",
            },
            Duration::from_secs(2),
        )
        .expect("SOCKS5 proxy check");

        assert_eq!(ip, "198.51.100.7");
        server.join().expect("mock SOCKS5 proxy");
    }

    #[test]
    fn socks4a_sends_domain_and_reads_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock proxy");
        let address = listener.local_addr().expect("mock address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept SOCKS4 request");
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"example.test\0") {
                stream.read_exact(&mut byte).expect("read SOCKS4 request");
                request.push(byte[0]);
            }
            assert_eq!(&request[..8], &[0x04, 0x01, 0x00, 0x50, 0, 0, 0, 1]);
            assert_eq!(&request[8..13], b"user\0");
            stream
                .write_all(&[0x00, 90, 0, 80, 127, 0, 0, 1])
                .expect("accept connect");

            let request = read_headers(&mut stream);
            assert!(request.starts_with("GET /ip HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n192.0.2.123")
                .expect("send judge response");
        });
        let proxy = mock_proxy("socks4", address.port());

        let ip = request_through_socks4(
            &proxy,
            Judge {
                host: "example.test",
                path: "/ip",
            },
            Duration::from_secs(2),
        )
        .expect("SOCKS4 proxy check");

        assert_eq!(ip, "192.0.2.123");
        server.join().expect("mock SOCKS4 proxy");
    }

    fn mock_proxy(protocol: &str, port: u16) -> StoredProxy {
        StoredProxy {
            protocol: protocol.to_string(),
            host: "127.0.0.1".to_string(),
            port,
            username: Some("user".to_string()),
            password: Some("pass".to_string()),
            ..StoredProxy::default()
        }
    }

    fn read_headers(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("read request byte");
            request.push(byte[0]);
        }
        String::from_utf8(request).expect("UTF-8 request")
    }
}
