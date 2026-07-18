use serde::Serialize;
use std::{collections::HashSet, net::IpAddr};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ParsedProxy {
    pub(crate) protocol: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyPreview {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub has_auth: bool,
    pub display: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedProxy {
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyParseReport {
    pub accepted: Vec<ProxyPreview>,
    pub rejected: Vec<RejectedProxy>,
    pub duplicates: usize,
}

pub(crate) struct ParsedProxyBatch {
    pub(crate) entries: Vec<ParsedProxy>,
    pub(crate) rejected: Vec<RejectedProxy>,
    pub(crate) duplicates: usize,
}

pub fn parse_batch(raw: &str, default_protocol: &str) -> ProxyParseReport {
    let parsed = parse_entries(raw, default_protocol);
    ProxyParseReport {
        accepted: parsed.entries.iter().map(ParsedProxy::preview).collect(),
        rejected: parsed.rejected,
        duplicates: parsed.duplicates,
    }
}

pub(crate) fn parse_entries(raw: &str, default_protocol: &str) -> ParsedProxyBatch {
    let default_protocol = normalized_protocol(default_protocol);
    let mut entries = Vec::new();
    let mut rejected = Vec::new();
    let mut duplicates = 0;
    let mut seen = HashSet::new();

    for (index, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim().trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }

        let candidates = proxy_tokens(line, default_protocol);
        for candidate in candidates {
            match parse_line(candidate, default_protocol) {
                Ok(proxy) => {
                    if seen.insert(proxy.key()) {
                        entries.push(proxy);
                    } else {
                        duplicates += 1;
                    }
                }
                Err(reason) => rejected.push(RejectedProxy {
                    line: index + 1,
                    reason: reason.to_string(),
                }),
            }
        }
    }

    ParsedProxyBatch {
        entries,
        rejected,
        duplicates,
    }
}

fn proxy_tokens<'a>(line: &'a str, default_protocol: &str) -> Vec<&'a str> {
    if parse_line(line, default_protocol).is_ok() {
        return vec![line];
    }

    let tokens: Vec<_> = line
        .split(|character: char| matches!(character, ',' | ';' | '\t') || character.is_whitespace())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        vec![line]
    } else {
        tokens
    }
}

fn parse_line(input: &str, default_protocol: &str) -> Result<ParsedProxy, &'static str> {
    let (protocol, value) = if let Some((scheme, rest)) = input.split_once("://") {
        let protocol = normalize_protocol(scheme).ok_or("unsupported protocol")?;
        (protocol, rest)
    } else {
        (default_protocol, input)
    };

    if value.chars().any(char::is_whitespace) {
        return Err("spaces are not allowed");
    }

    if let Some((auth, address)) = value.rsplit_once('@') {
        let (username, password) = parse_auth(auth)?;
        let (host, port, remainder) = parse_host_port(address)?;
        if !remainder.is_empty() {
            return Err("invalid format after the port");
        }
        return build_proxy(protocol, host, port, Some(username), Some(password));
    }

    if value.starts_with('[') {
        let (host, port, remainder) = parse_host_port(value)?;
        let (username, password) = parse_optional_auth(&remainder)?;
        return build_proxy(protocol, host, port, username, password);
    }

    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() < 2 {
        return Err("use host:port");
    }

    let address_first = parse_port(parts[1]).ok();
    let auth_first = parts
        .last()
        .and_then(|value| parse_port(value).ok())
        .filter(|_| parts.len() >= 4);
    let prefer_auth_first = address_first.is_some()
        && auth_first.is_some()
        && looks_like_strong_host(parts[parts.len() - 2])
        && !looks_like_strong_host(parts[0]);

    if let Some(port) = address_first
        && !prefer_auth_first
    {
        let host = parts[0].to_string();
        let remainder = if parts.len() > 2 {
            format!(":{}", parts[2..].join(":"))
        } else {
            String::new()
        };
        let (username, password) = parse_optional_auth(&remainder)?;
        return build_proxy(protocol, host, port, username, password);
    }

    if let Some(port) = auth_first {
        let host = parts[parts.len() - 2].to_string();
        let username = parts[0].to_string();
        let password = parts[1..parts.len() - 2].join(":");
        if username.is_empty() || password.is_empty() {
            return Err("incomplete authentication");
        }
        return build_proxy(protocol, host, port, Some(username), Some(password));
    }

    Err("invalid proxy format")
}

fn looks_like_strong_host(value: &str) -> bool {
    value.parse::<IpAddr>().is_ok()
        || value.contains('.')
        || value.eq_ignore_ascii_case("localhost")
}

fn parse_host_port(value: &str) -> Result<(String, u16, String), &'static str> {
    if let Some(without_open) = value.strip_prefix('[') {
        let closing = without_open
            .find(']')
            .ok_or("IPv6 address is missing a closing bracket")?;
        let host = without_open[..closing].to_string();
        let tail = &without_open[closing + 1..];
        let tail = tail.strip_prefix(':').ok_or("missing port")?;
        let (port, remainder) = split_port_and_remainder(tail)?;
        return Ok((host, port, remainder));
    }

    let (host, tail) = value.split_once(':').ok_or("missing port")?;
    let (port, remainder) = split_port_and_remainder(tail)?;
    Ok((host.to_string(), port, remainder))
}

fn split_port_and_remainder(tail: &str) -> Result<(u16, String), &'static str> {
    let (port, remainder) = match tail.split_once(':') {
        Some((port, remainder)) => (port, format!(":{remainder}")),
        None => (tail, String::new()),
    };
    Ok((parse_port(port)?, remainder))
}

fn parse_optional_auth(remainder: &str) -> Result<(Option<String>, Option<String>), &'static str> {
    if remainder.is_empty() {
        return Ok((None, None));
    }
    let auth = remainder
        .strip_prefix(':')
        .ok_or("invalid authentication")?;
    let (username, password) = parse_auth(auth)?;
    Ok((Some(username), Some(password)))
}

fn parse_auth(auth: &str) -> Result<(String, String), &'static str> {
    let (username, password) = auth.split_once(':').ok_or("use username:password")?;
    if username.is_empty() || password.is_empty() {
        return Err("incomplete authentication");
    }
    Ok((username.to_string(), password.to_string()))
}

fn parse_port(value: &str) -> Result<u16, &'static str> {
    let port = value.parse::<u16>().map_err(|_| "invalid port")?;
    if port == 0 {
        return Err("invalid port");
    }
    Ok(port)
}

fn normalize_protocol(value: &str) -> Option<&'static str> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "" | "http" | "https" => Some("http"),
        "socks4" | "socks4a" => Some("socks4"),
        "socks5" | "socks5h" => Some("socks5"),
        _ => None,
    }
}

pub(crate) fn normalized_protocol(value: &str) -> &'static str {
    normalize_protocol(value).unwrap_or("http")
}

fn build_proxy(
    protocol: &str,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
) -> Result<ParsedProxy, &'static str> {
    let host = host.trim().to_string();
    if host.is_empty()
        || host.chars().any(|character| {
            character.is_whitespace() || matches!(character, '/' | '@' | '[' | ']')
        })
    {
        return Err("invalid host");
    }

    Ok(ParsedProxy {
        protocol: protocol.to_string(),
        host,
        port,
        username,
        password,
    })
}

impl ParsedProxy {
    pub(crate) fn key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.protocol,
            self.host.to_ascii_lowercase(),
            self.port,
            self.username.as_deref().unwrap_or_default(),
            self.password.as_deref().unwrap_or_default()
        )
    }

    pub(crate) fn preview(&self) -> ProxyPreview {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        ProxyPreview {
            protocol: self.protocol.clone(),
            host: self.host.clone(),
            port: self.port,
            has_auth: self.username.is_some(),
            display: format!("{}://{}:{}", self.protocol, host, self.port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn accepts_reference_formats() {
        let cases = [
            ("1.2.3.4:8080", "1.2.3.4", 8080, None),
            ("user:pass@1.2.3.4:8080", "1.2.3.4", 8080, Some("user")),
            ("1.2.3.4:8080:user:p:a:ss", "1.2.3.4", 8080, Some("user")),
            (
                "alice:s3cret:10.0.0.1:3128",
                "10.0.0.1",
                3128,
                Some("alice"),
            ),
            ("socks5://1.2.3.4:1080", "1.2.3.4", 1080, None),
            ("[2001:db8::1]:8080:u:p", "2001:db8::1", 8080, Some("u")),
            ("proxy.example.com:3128", "proxy.example.com", 3128, None),
        ];

        for (input, host, port, username) in cases {
            let parsed = parse_line(input, "http").unwrap_or_else(|error| {
                panic!("{input} should parse: {error}");
            });
            assert_eq!(parsed.host, host, "{input}");
            assert_eq!(parsed.port, port, "{input}");
            assert_eq!(parsed.username.as_deref(), username, "{input}");
        }
    }

    #[test]
    fn rejects_invalid_values() {
        for input in ["not-a-proxy", "1.2.3.4:0", "1.2.3.4:99999", "ftp://x:21"] {
            assert!(parse_line(input, "http").is_err(), "{input}");
        }
    }

    #[test]
    fn batch_deduplicates_equivalent_formats_without_returning_passwords() {
        let report = parse_batch(
            "1.2.3.4:8080:user:pass\nuser:pass@1.2.3.4:8080\nsocks5://2.2.2.2:1080\nbad",
            "http",
        );

        assert_eq!(report.accepted.len(), 2);
        assert_eq!(report.duplicates, 1);
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].line, 4);
        assert!(report.accepted[0].has_auth);
        assert!(!report.accepted[0].display.contains("pass"));
    }

    #[test]
    fn keeps_same_address_when_protocols_are_different() {
        let report = parse_batch("http://1.2.3.4:8080\nsocks5://1.2.3.4:8080", "http");

        assert_eq!(report.accepted.len(), 2);
        assert_eq!(report.duplicates, 0);
    }

    #[test]
    fn accepts_previous_batch_separators_without_breaking_single_credentials() {
        let report = parse_batch(
            "1.1.1.1:80, 2.2.2.2:81;\t3.3.3.3:82   socks5://4.4.4.4:1080",
            "http",
        );
        assert_eq!(report.accepted.len(), 4);
        assert!(report.rejected.is_empty());

        let password_with_separator = parse_batch("user:pa;ss@5.5.5.5:8080", "http");
        assert_eq!(password_with_separator.accepted.len(), 1);
        assert!(password_with_separator.accepted[0].has_auth);
    }

    #[test]
    fn numeric_password_before_a_clear_host_uses_auth_first_format() {
        let parsed =
            parse_line("alice:123:proxy.example.com:3128", "http").expect("numeric password proxy");
        assert_eq!(parsed.host, "proxy.example.com");
        assert_eq!(parsed.port, 3128);
        assert_eq!(parsed.username.as_deref(), Some("alice"));
        assert_eq!(parsed.password.as_deref(), Some("123"));
    }

    #[test]
    #[ignore = "requires an authorized local file in AYLA_PROXY_FILE"]
    fn validates_authorized_proxy_file_without_logging_credentials() {
        let path = env::var_os("AYLA_PROXY_FILE").expect("set AYLA_PROXY_FILE");
        let raw = fs::read_to_string(path).expect("read proxy file as UTF-8 text");
        let expected = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("//"))
            .count();
        let report = parse_batch(&raw, "http");

        assert!(
            report.rejected.is_empty(),
            "{} proxy entries were rejected",
            report.rejected.len()
        );
        assert_eq!(report.duplicates, 0, "the proxy file contains duplicates");
        assert_eq!(report.accepted.len(), expected);
    }
}
