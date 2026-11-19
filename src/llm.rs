//! A local language model, reached over an OpenAI-compatible HTTP API.
//!
//! omaCortex never ships activity to a hosted model. `endpoint = "auto"`
//! probes only loopback ports used by local runtimes, in this order:
//!
//! 1. `omarchy agent local` models (ports from its `ports.json`, then 11435+)
//! 2. Ollama (11434)
//! 3. llama.cpp `llama-server` (8080)
//! 4. LM Studio (1234)
//!
//! An endpoint that is not on this machine is refused unless
//! `model.allow_remote = true`. Plain HTTP only: there is no TLS stack in the
//! binary, which is the point — nothing here is meant to leave the machine.

use crate::config;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

const MAX_RESPONSE: u64 = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct ModelError(pub String);

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ModelError> {
    Err(ModelError(msg.into()))
}

struct Url {
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<Url, ModelError> {
    let Some(rest) = url.strip_prefix("http://") else {
        return err(format!("{url}: only plain http:// endpoints are supported"));
    };
    let (authority, path) = rest
        .find('/')
        .map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    let (host, port) = if let Some(h) = authority.strip_prefix('[') {
        let (h, after) = h
            .split_once(']')
            .ok_or_else(|| ModelError(format!("{url}: bad host")))?;
        (
            h.to_string(),
            after.strip_prefix(':').unwrap_or("80").to_string(),
        )
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.to_string()),
            None => (authority.to_string(), "80".into()),
        }
    };
    let port = port
        .parse()
        .map_err(|_| ModelError(format!("{url}: bad port")))?;
    if host.is_empty() {
        return err(format!("{url}: no host"));
    }
    Ok(Url {
        host,
        port,
        path: path.to_string(),
    })
}

fn resolve_addrs(host: &str, port: u16) -> Vec<SocketAddr> {
    (host, port)
        .to_socket_addrs()
        .map(Iterator::collect)
        .unwrap_or_default()
}

pub fn is_loopback(url: &str) -> bool {
    let Ok(u) = parse_url(url) else { return false };
    if u.host == "localhost" {
        return true;
    }
    if let Ok(ip) = u.host.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    let addrs = resolve_addrs(&u.host, u.port);
    !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback())
}

fn decode_chunked(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = body.windows(2).position(|w| w == b"\r\n") {
        let size_line = String::from_utf8_lossy(&body[..nl]);
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or(0);
        body = &body[nl + 2..];
        if size == 0 || size > body.len() {
            break;
        }
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).unwrap_or(&[]);
    }
    out
}

/// Minimal HTTP/1.1 request with `Connection: close`.
fn http(url: &str, payload: Option<&Value>, timeout: Duration) -> Result<Value, ModelError> {
    let u = parse_url(url)?;
    let addrs = resolve_addrs(&u.host, u.port);
    let mut last = format!("{url}: cannot resolve host");
    let mut stream = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout.min(Duration::from_secs(3))) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = format!("{url}: {e}"),
        }
    }
    let mut s = stream.ok_or(ModelError(last))?;
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    let body = payload.map(Value::to_string).unwrap_or_default();
    let method = if payload.is_some() { "POST" } else { "GET" };
    let host = if u.host.contains(':') {
        format!("[{}]", u.host)
    } else {
        u.host.clone()
    };
    let req = format!(
        "{method} {} HTTP/1.1\r\nHost: {host}:{}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        u.path,
        u.port,
        body.len()
    );
    s.write_all(req.as_bytes())
        .map_err(|e| ModelError(format!("{url}: {e}")))?;
    let mut raw = Vec::new();
    s.take(MAX_RESPONSE + 64 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| ModelError(format!("{url}: {e}")))?;
    let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return err(format!("{url}: bad HTTP response"));
    };
    let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut body = raw[split + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        body = decode_chunked(&body);
    }
    if body.len() as u64 > MAX_RESPONSE {
        return err(format!("{url}: response too large"));
    }
    if !(200..300).contains(&status) {
        return err(format!("{url}: HTTP {status}"));
    }
    let v: Value =
        serde_json::from_slice(&body).map_err(|_| ModelError(format!("{url}: not JSON")))?;
    if !v.is_object() {
        return err(format!("{url}: unexpected response"));
    }
    Ok(v)
}

/// Drop a leading `<think>…</think>` block that reasoning models emit.
pub fn strip_reasoning(text: &str) -> String {
    match (text.find("<think>"), text.find("</think>")) {
        (Some(a), Some(b)) if b > a => format!("{}{}", &text[..a], &text[b + "</think>".len()..]),
        _ => text.to_string(),
    }
}

#[derive(Debug)]
pub struct Model {
    pub endpoint: String,
    pub model: String,
    timeout: Duration,
    max_tokens: u32,
}

impl Model {
    pub fn chat(&self, system: &str, user: &str) -> Result<String, ModelError> {
        let reply = http(
            &format!("{}/chat/completions", self.endpoint.trim_end_matches('/')),
            Some(&json!({
                "model": self.model,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
                "temperature": 0.2,
                "max_tokens": self.max_tokens,
                "stream": false,
            })),
            self.timeout,
        )?;
        let text = reply["choices"][0]["message"]["content"]
            .as_str()
            .ok_or(ModelError("model returned no message".into()))?;
        Ok(strip_reasoning(text).trim().to_string())
    }
}

pub fn list_models(endpoint: &str) -> Result<Vec<String>, ModelError> {
    let v = http(
        &format!("{}/models", endpoint.trim_end_matches('/')),
        None,
        Duration::from_millis(1500),
    )?;
    let items = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .filter_map(|i| {
            i.get("id")
                .or_else(|| i.get("name"))
                .or_else(|| i.get("model"))
                .and_then(Value::as_str)
        })
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

fn omarchy_local_ports() -> Vec<u16> {
    let path = config::home().join(".local/state/omarchy/agent-local/ports.json");
    let mut ports: Vec<u16> = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.values()
                .filter_map(Value::as_u64)
                .filter(|p| (1024..=65535).contains(p))
                .map(|p| p as u16)
                .collect()
        })
        .unwrap_or_default();
    ports.sort_unstable();
    ports.dedup();
    if ports.is_empty() {
        vec![11435, 11436, 11437]
    } else {
        ports
    }
}

pub fn candidate_endpoints() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for port in omarchy_local_ports().into_iter().chain([11434, 8080, 1234]) {
        let url = format!("http://127.0.0.1:{port}/v1");
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// Find a usable local model, or say why there is none.
pub fn resolve(m: &config::Model) -> Result<Model, ModelError> {
    let mut endpoint = m.endpoint.trim().to_string();
    if let Ok(over) = std::env::var("OMACORTEX_MODEL_ENDPOINT")
        && !over.trim().is_empty()
    {
        endpoint = over.trim().to_string();
    }
    if endpoint.eq_ignore_ascii_case("off") {
        return err("the local model is turned off (model.endpoint = \"off\")");
    }
    let candidates = if endpoint.is_empty() || endpoint.eq_ignore_ascii_case("auto") {
        candidate_endpoints()
    } else {
        vec![endpoint]
    };
    let mut errors = Vec::new();
    for url in candidates {
        if let Err(e) = parse_url(&url) {
            errors.push(e.0);
            continue;
        }
        if !m.allow_remote && !is_loopback(&url) {
            return err(format!(
                "{url} is not on this machine; set model.allow_remote = true to allow it"
            ));
        }
        match list_models(&url) {
            Err(e) => errors.push(e.0),
            Ok(names) => {
                let model = if m.model.is_empty() {
                    names.first().cloned().unwrap_or_default()
                } else {
                    m.model.clone()
                };
                if model.is_empty() {
                    errors.push(format!("{url}: no models listed"));
                    continue;
                }
                return Ok(Model {
                    endpoint: url,
                    model,
                    timeout: Duration::from_secs(m.timeout_seconds.max(1)),
                    max_tokens: m.max_tokens.max(16),
                });
            }
        }
    }
    let hint = "run `omarchy agent local use best`, or start Ollama / llama-server";
    err(format!(
        "no local model answered ({hint}){}",
        errors.last().map(|e| format!(": {e}")).unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_guard() {
        assert!(is_loopback("http://127.0.0.1:1/v1"));
        assert!(is_loopback("http://[::1]:1/v1"));
        assert!(is_loopback("http://localhost:11434/v1"));
        assert!(!is_loopback("http://10.0.0.1:1/v1"));
        assert!(!is_loopback("https://127.0.0.1/v1"));
        let cfg = config::Model {
            endpoint: "http://203.0.113.5:11434/v1".into(),
            ..Default::default()
        };
        assert!(resolve(&cfg).unwrap_err().0.contains("not on this machine"));
        let off = config::Model {
            endpoint: "off".into(),
            ..Default::default()
        };
        assert!(resolve(&off).unwrap_err().0.contains("turned off"));
    }

    #[test]
    fn chunked_and_reasoning() {
        assert_eq!(
            decode_chunked(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"),
            b"Wikipedia"
        );
        assert_eq!(strip_reasoning("<think>hm</think>Answer"), "Answer");
    }
}
