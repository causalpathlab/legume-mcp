//! Just enough HTTP/1.1 to talk to a local OpenAI-compatible model server
//! (llama-server, LM Studio, Ollama, MLX, vLLM): a JSON request, a JSON
//! answer, plain `http://` only.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Where a model server is: `http://host:port/v1`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Url {
    pub host: String,
    pub port: u16,
    /// The path before `/chat/completions`, without a trailing `/`.
    pub base: String,
}

impl Url {
    pub(crate) fn parse(url: &str) -> Result<Self> {
        let Some(rest) = url.strip_prefix("http://") else {
            bail!("{url}: only a local http:// server is supported (https needs a TLS library the chat does without)");
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().with_context(|| format!("{url}: port"))?),
            None => (authority, 80),
        };
        anyhow::ensure!(!host.is_empty(), "{url}: no host");
        let host = host.trim_start_matches('[').trim_end_matches(']');
        Ok(Self {
            host: host.into(),
            port,
            base: format!("/{}", path.trim_end_matches('/'))
                .trim_end_matches('/')
                .to_string(),
        })
    }

    pub(crate) fn show(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{host}:{}{}", self.port, self.base)
    }

    /// Fail unless every address the host names is this machine's: the
    /// conversation (the person's data, the records) goes to the model in
    /// plain text, and its answers run the tools.
    pub(crate) fn ensure_local(&self) -> Result<()> {
        let addrs: Vec<_> = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .with_context(|| format!("{}: no address", self.show()))?
            .collect();
        anyhow::ensure!(
            !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback()),
            "{}: the model server must be on this machine (localhost, 127.0.0.1 or ::1); \
             the chat sends it the conversation in plain text, and its answers run the tools",
            self.show()
        );
        Ok(())
    }
}

/// The most of an answer read: more is not a model's answer.
const MOST: usize = 16 << 20;

/// How long to wait for a model's answer at most.
const ANSWER: Duration = Duration::from_secs(900);

fn connect(url: &Url, timeout: Duration) -> Result<TcpStream> {
    // `localhost` is ::1 first on macOS; a server may listen on 127.0.0.1
    // only: each address in turn.
    let mut last = None;
    let mut found = None;
    for addr in (url.host.as_str(), url.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                found = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let s = match (found, last) {
        (Some(s), _) => s,
        (None, Some(e)) => return Err(e).with_context(|| format!("connecting to {}", url.show())),
        (None, None) => anyhow::bail!("{}: no address", url.host),
    };
    s.set_read_timeout(Some(Duration::from_millis(200)))?;
    Ok(s)
}

/// `GET {base}{path}`, or None when nothing answers within `timeout`.
pub(crate) fn get(url: &Url, path: &str, timeout: Duration) -> Option<Value> {
    let mut s = connect(url, timeout).ok()?;
    let req = format!(
        "GET {}{path} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        url.base, url.host, url.port
    );
    s.write_all(req.as_bytes()).ok()?;
    let stop = AtomicBool::new(false);
    let (status, body) = read(&mut s, &stop, Instant::now() + timeout * 4).ok()?;
    (status == 200).then(|| serde_json::from_slice(&body).ok())?
}

/// `POST {base}{path}` with `body`; the answer's JSON. `stop` set gives up.
pub(crate) fn post(
    url: &Url,
    path: &str,
    body: &Value,
    key: Option<&str>,
    stop: &AtomicBool,
) -> Result<Value> {
    let mut s = connect(url, Duration::from_secs(5))?;
    let body = serde_json::to_vec(body)?;
    let auth = key.map_or(String::new(), |k| format!("Authorization: Bearer {k}\r\n"));
    let head = format!(
        "POST {}{path} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n{auth}Connection: close\r\n\r\n",
        url.base,
        url.host,
        url.port,
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(&body)?;
    let (status, body) = read(&mut s, stop, Instant::now() + ANSWER)?;
    let text = String::from_utf8_lossy(&body);
    if status != 200 {
        bail!("{} answered {status}: {}", url.show(), text.trim());
    }
    serde_json::from_str(&text).with_context(|| format!("{} answered: {}", url.show(), text.trim()))
}

/// A whole response: its status and body, chunked or not.
fn read(s: &mut TcpStream, stop: &AtomicBool, until: Instant) -> Result<(u16, Vec<u8>)> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 16384];
    // The head, once it is all there: where the body starts, and how its
    // end is known.
    let mut head: Option<(usize, Option<usize>, bool)> = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            bail!("stopped");
        }
        if Instant::now() > until {
            bail!("no answer in {} s", ANSWER.as_secs());
        }
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                anyhow::ensure!(raw.len() <= MOST, "an answer over {} MB", MOST >> 20);
                if head.is_none() {
                    head = head_of(&raw);
                }
                if let Some((start, length, chunked)) = head {
                    let body = &raw[start..];
                    let done = match length {
                        Some(n) => body.len() >= n,
                        None => chunked && body.ends_with(b"0\r\n\r\n"),
                    };
                    if done {
                        break;
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    parse(&raw)
}

/// Once `raw` holds the whole head: where the body starts, its length when
/// given, and whether it comes in chunks. A body with neither ends as the
/// server closes the connection.
fn head_of(raw: &[u8]) -> Option<(usize, Option<usize>, bool)> {
    let end = find(raw, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
    let length = header(&head, "content-length").and_then(|v| v.parse::<usize>().ok());
    let chunked = header(&head, "transfer-encoding").is_some_and(|v| v.contains("chunked"));
    Some((end + 4, length, chunked))
}

/// Whether `raw` holds the whole response.
#[cfg(test)]
fn complete(raw: &[u8]) -> bool {
    head_of(raw).is_some_and(|(start, length, chunked)| {
        let body = &raw[start..];
        match length {
            Some(n) => body.len() >= n,
            None => chunked && body.ends_with(b"0\r\n\r\n"),
        }
    })
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .find_map(|l| l.strip_prefix(name)?.trim_start().strip_prefix(':'))
        .map(str::trim)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse(raw: &[u8]) -> Result<(u16, Vec<u8>)> {
    let end = find(raw, b"\r\n\r\n").context("no HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .context("no HTTP status")?;
    let body = &raw[end + 4..];
    if header(&head, "transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        return Ok((status, unchunk(body)?));
    }
    let n = header(&head, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(body.len())
        .min(body.len());
    Ok((status, body[..n].to_vec()))
}

fn unchunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = find(body, b"\r\n").context("a chunk without its size")?;
        let size = String::from_utf8_lossy(&body[..line]);
        let size = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16)
            .context("a chunk size")?;
        body = &body[line + 2..];
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(body.len() >= size, "a chunk cut short");
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_read_as_local_servers_give_them() {
        let u = Url::parse("http://127.0.0.1:9999/v1/").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.base.as_str()),
            ("127.0.0.1", 9999, "/v1")
        );
        assert_eq!(Url::parse("http://localhost").unwrap().base, "");
        assert!(Url::parse("https://api.example.com/v1").is_err());
        let six = Url::parse("http://[::1]:8080/v1").unwrap();
        assert_eq!(
            (six.host.as_str(), six.show().as_str()),
            ("::1", "http://[::1]:8080/v1")
        );
        assert!(six.ensure_local().is_ok() && u.ensure_local().is_ok());
        assert!(Url::parse("http://10.1.2.3:8080/v1")
            .unwrap()
            .ensure_local()
            .is_err());
    }

    #[test]
    fn responses_are_read_whole_chunked_or_not() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert!(complete(plain));
        assert_eq!(parse(plain).unwrap(), (200, b"{}".to_vec()));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n5\r\n\":1}\n\r\n0\r\n\r\n";
        assert!(complete(chunked));
        assert_eq!(parse(chunked).unwrap().1, b"{\"a\":1}\n".to_vec());
        assert!(!complete(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}"));
    }
}
