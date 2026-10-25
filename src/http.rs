//! A minimal HTTP/1.1 client for plain-HTTP services on this machine or the LAN
//! (readiness probes and Anvil JSON-RPC). Never uses a proxy.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

fn split_url(url: &str) -> Result<(String, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("only http:// URLs: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    Ok((authority, path.to_string()))
}

pub fn request(method: &str, url: &str, body: Option<&str>, timeout: Duration) -> Result<Response> {
    let (authority, path) = split_url(url)?;
    let addr = authority
        .to_socket_addrs()
        .with_context(|| format!("resolve {authority}"))?
        .next()
        .ok_or_else(|| anyhow!("no address for {authority}"))?;
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).with_context(|| format!("connect {url}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nAccept: */*\r\n"
    );
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(body) = body {
        stream.write_all(body.as_bytes())?;
    }
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .with_context(|| format!("read {url}"))?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<Response> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed HTTP response"))?;
    let head = std::str::from_utf8(&raw[..end])?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("malformed HTTP status line"))?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let payload = &raw[end + 4..];
    let body = if chunked {
        dechunk(payload)?
    } else {
        payload.to_vec()
    };
    Ok(Response {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow!("bad chunk"))?;
        let size_text = std::str::from_utf8(&data[..line_end])?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            bail!("truncated chunk");
        }
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or_default();
    }
}

pub fn get(url: &str, timeout: Duration) -> Result<Response> {
    request("GET", url, None, timeout)
}

/// `GET <base>/readyz` answers `{"ready": true}`.
pub fn ready(base: &str) -> bool {
    get(&format!("{base}/readyz"), Duration::from_secs(3))
        .ok()
        .filter(Response::success)
        .and_then(|r| serde_json::from_str::<Value>(&r.body).ok())
        .is_some_and(|v| v["ready"] == Value::Bool(true))
}

/// A JSON-RPC call; returns `result`.
pub fn rpc(url: &str, method: &str, params: Value) -> Result<Value> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
    let response = request("POST", url, Some(&body), Duration::from_secs(5))?;
    let value: Value =
        serde_json::from_str(&response.body).with_context(|| format!("{method}: not JSON"))?;
    if let Some(error) = value.get("error") {
        bail!("{method}: {error}");
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("{method}: no result"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_chunked_bodies() {
        let plain = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!((plain.status, plain.body.as_str()), (200, "{}"));
        let chunked =
            parse_response(b"HTTP/1.1 503 X\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n").unwrap();
        assert_eq!((chunked.status, chunked.body.as_str()), (503, "{\"a\":1}"));
    }

    #[test]
    fn splits_urls() {
        assert_eq!(
            split_url("http://127.0.0.1:8080/readyz").unwrap(),
            ("127.0.0.1:8080".into(), "/readyz".into())
        );
        assert_eq!(
            split_url("http://localhost").unwrap(),
            ("localhost:80".into(), "/".into())
        );
        assert!(split_url("https://x").is_err());
    }
}
