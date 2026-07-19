//! A tiny blocking HTTP/1.1 client over `std::net::TcpStream`, just enough to drive the reference
//! `imbhd` server (`imbh-server::serve`) from an integration test. The server replies with
//! `Connection: close`, so a full read-to-EOF yields the whole response — no chunked/keep-alive
//! handling needed.

use std::io::{Read, Write};
use std::net::TcpStream;

/// A parsed HTTP response: status code, the `Content-Type` header (lowercased name match), and body.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The body decoded as UTF-8 (panics if the body is not valid UTF-8 — fine for test JSON/text).
    pub fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("response body is valid UTF-8")
    }
}

/// `POST path` to `addr` (e.g. `127.0.0.1:53812`) with `content_type` and `body`.
pub fn post(
    addr: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<HttpResponse> {
    request(addr, "POST", path, Some((content_type, body)))
}

/// `GET path` from `addr`.
pub fn get(addr: &str, path: &str) -> std::io::Result<HttpResponse> {
    request(addr, "GET", path, None)
}

fn request(
    addr: &str,
    method: &str,
    path: &str,
    payload: Option<(&str, &[u8])>,
) -> std::io::Result<HttpResponse> {
    let mut stream = TcpStream::connect(addr)?;
    let (content_type, body) = payload.unwrap_or(("application/octet-stream", &[]));
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    Ok(parse(&raw))
}

fn parse(raw: &[u8]) -> HttpResponse {
    // Split headers from body at the first CRLFCRLF.
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response has a header/body separator");
    let head = String::from_utf8_lossy(&raw[..sep]);
    let body = raw[sep + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    // "HTTP/1.1 200 OK" → 200
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let content_type = lines
        .find_map(|l| {
            l.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    .then(|| value.trim().to_owned())
            })
        })
        .unwrap_or_default();

    HttpResponse {
        status,
        content_type,
        body,
    }
}
