//! A small Model Context Protocol server over stdio for the legume tools.
//!
//! A tool binary implements [`Handler`] (its tool list and how each is
//! called) and hands it to [`serve`]. The server speaks JSON-RPC 2.0, one
//! message per line, and answers `initialize`, `ping`, `tools/list` and
//! `tools/call`; it offers tools only (no resources, prompts or sampling).
//!
//! - stdout carries the protocol, so a tool must never print there: logs go
//!   to stderr, and work that prints runs as a child through [`Call::run`].
//! - A tool's failure is a result with `isError` and the error's chain as
//!   text, for the agent to read; JSON-RPC errors are kept for protocol
//!   faults (unknown method or tool, unreadable message).
//! - Calls run one at a time, in order. `ping` is answered and
//!   `notifications/cancelled` is taken while a call runs: the call sees it
//!   through [`Call::is_cancelled`], [`Call::run`] kills its child, and a
//!   cancelled call gets no response, as the protocol asks.
//! - [`Call::progress`] sends `notifications/progress` when the client
//!   asked for them (a `progressToken`), and does nothing otherwise.

use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Protocol revisions this server speaks, newest first. A client asking for
/// one of them gets it; any other gets the newest.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Lines of a failed child's stderr quoted in its error.
const STDERR_TAIL: usize = 20;
/// How often a waiting child run looks for a cancellation.
const POLL: Duration = Duration::from_millis(100);

/// What `initialize` reports: the server's name and version, and the
/// instructions an agent reads before its first call (what the tools are
/// for, and in which order to call them).
#[derive(Clone, Debug, Default)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub instructions: Option<String>,
}

/// One tool as `tools/list` shows it; `input_schema` is a JSON Schema
/// object for its arguments.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolSpec {
    pub fn new(name: &str, description: &str, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// A piece of a tool's result.
#[derive(Clone, Debug)]
pub enum Content {
    Text(String),
    /// Sent as pretty-printed text and, for the first object among the
    /// result's pieces, as `structuredContent` too.
    Json(Value),
    /// A PNG image, sent base64-encoded.
    Png(Vec<u8>),
}

/// A tool binary's side of the server: its tools, and a call to one of them
/// by name with the arguments the client sent (`{}` when it sent none).
/// State kept between calls lives in the handler.
pub trait Handler {
    fn tools(&self) -> Vec<ToolSpec>;
    fn call(&mut self, name: &str, args: Value, cx: &Call) -> Result<Vec<Content>>;
}

type Out = Arc<Mutex<Box<dyn Write + Send>>>;
/// Ids (as JSON text) of requests the client has cancelled.
type Cancelled = Arc<Mutex<HashSet<String>>>;

fn send(out: &Out, msg: &Value) {
    let mut w = out.lock().unwrap_or_else(|e| e.into_inner());
    // A closed stdout ends the session; the reader sees it too.
    let _ = writeln!(w, "{msg}").and_then(|()| w.flush());
}

fn respond(out: &Out, id: &Value, result: Value) {
    send(
        out,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    );
}

fn fail(out: &Out, id: &Value, code: i64, message: &str) {
    send(
        out,
        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

/// The call in progress, as a tool sees it.
pub struct Call {
    id: String,
    token: Option<Value>,
    step: AtomicU64,
    out: Out,
    cancelled: Cancelled,
}

impl Call {
    /// Whether the client has cancelled this call.
    pub fn is_cancelled(&self) -> bool {
        let set = self.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        set.contains(&self.id)
    }

    /// Tell the client what the call is doing, when it asked to hear.
    pub fn progress(&self, message: &str) {
        let Some(token) = &self.token else { return };
        let n = self.step.fetch_add(1, Ordering::Relaxed) + 1;
        send(
            &self.out,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": { "progressToken": token, "progress": n, "message": message },
            }),
        );
    }

    /// Run `cmd` to its end and return its stdout. Each stderr line is
    /// passed on as progress; a cancellation kills the child; a failure
    /// quotes the last lines of its stderr.
    pub fn run(&self, mut cmd: Command) -> Result<String> {
        let shown = format!("{cmd:?}");
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {shown}"))?;
        let mut stdout = child.stdout.take().context("child stdout")?;
        let stderr = child.stderr.take().context("child stderr")?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            stdout.read_to_string(&mut s).map(|_| s)
        });
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut tail = VecDeque::with_capacity(STDERR_TAIL);
        let status = loop {
            if self.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("cancelled");
            }
            match rx.recv_timeout(POLL) {
                Ok(line) => {
                    self.progress(&line);
                    if tail.len() == STDERR_TAIL {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                Err(RecvTimeoutError::Timeout) => {}
                // stderr is closed: the child is ending, or ended.
                Err(RecvTimeoutError::Disconnected) => match child.try_wait()? {
                    Some(status) => break status,
                    None => std::thread::sleep(POLL),
                },
            }
        };
        let out = reader
            .join()
            .map_err(|_| anyhow::anyhow!("reading the child's stdout"))??;
        if !status.success() {
            let tail: Vec<String> = tail.into();
            anyhow::bail!("{shown} failed ({status}):\n{}", tail.join("\n"));
        }
        Ok(out)
    }
}

/// Serve `handler` on stdin and stdout until stdin closes.
pub fn serve(info: ServerInfo, handler: impl Handler) -> Result<()> {
    serve_on(
        BufReader::new(std::io::stdin()),
        std::io::stdout(),
        info,
        handler,
    )
}

/// [`serve`] on any input and output (tests, sockets).
pub fn serve_on<R, W>(
    input: R,
    output: W,
    info: ServerInfo,
    mut handler: impl Handler,
) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    let out: Out = Arc::new(Mutex::new(Box::new(output)));
    let cancelled: Cancelled = Arc::default();
    let (tx, rx) = channel::<Value>();
    // The reader answers what must not wait for a running call: pings,
    // cancellations and unreadable lines. The rest queues for the loop.
    let reader = {
        let (out, cancelled) = (out.clone(), cancelled.clone());
        std::thread::spawn(move || {
            for line in input.lines() {
                let line = match line {
                    Ok(l) if l.trim().is_empty() => continue,
                    Ok(l) => l,
                    Err(e) => {
                        log::warn!("mcp: reading stdin: {e}");
                        break;
                    }
                };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    fail(&out, &Value::Null, -32700, "parse error");
                    continue;
                };
                match msg.get("method").and_then(Value::as_str) {
                    Some("ping") => respond(&out, &msg["id"], json!({})),
                    Some("notifications/cancelled") => {
                        let id = msg["params"]["requestId"].to_string();
                        cancelled
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id);
                    }
                    _ => {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        })
    };
    let specs = handler.tools();
    for msg in rx {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            continue; // a response to a request we never send
        };
        let Some(id) = msg.get("id") else {
            continue; // notifications/initialized and the like
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => respond(&out, id, initialize(&info, &params)),
            "tools/list" => {
                let tools: Vec<Value> = specs
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema,
                        })
                    })
                    .collect();
                respond(&out, id, json!({ "tools": tools }));
            }
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                if !specs.iter().any(|t| t.name == name) {
                    fail(&out, id, -32602, &format!("unknown tool: {name}"));
                    continue;
                }
                let args = match params.get("arguments") {
                    Some(a) if !a.is_null() => a.clone(),
                    _ => json!({}),
                };
                let cx = Call {
                    id: id.to_string(),
                    token: params["_meta"].get("progressToken").cloned(),
                    step: AtomicU64::new(0),
                    out: out.clone(),
                    cancelled: cancelled.clone(),
                };
                let result = catch_unwind(AssertUnwindSafe(|| handler.call(name, args, &cx)))
                    .unwrap_or_else(|p| {
                        let what = p
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| p.downcast_ref::<&str>().copied())
                            .unwrap_or("panic");
                        Err(anyhow::anyhow!("{name} panicked: {what}"))
                    });
                let was_cancelled = cx.is_cancelled();
                cancelled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&cx.id);
                if !was_cancelled {
                    respond(&out, id, call_result(result));
                }
            }
            _ => fail(&out, id, -32601, &format!("method not found: {method}")),
        }
    }
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("the stdin reader panicked"))
}

fn initialize(info: &ServerInfo, params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    let mut v = json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": info.name, "version": info.version },
    });
    if let Some(text) = &info.instructions {
        v["instructions"] = json!(text);
    }
    v
}

fn call_result(result: Result<Vec<Content>>) -> Value {
    let pieces = match result {
        Ok(p) => p,
        Err(e) => {
            return json!({
                "content": [{ "type": "text", "text": format!("{e:#}") }],
                "isError": true,
            })
        }
    };
    let mut structured: Option<Map<String, Value>> = None;
    let content: Vec<Value> = pieces
        .into_iter()
        .map(|c| match c {
            Content::Text(text) => json!({ "type": "text", "text": text }),
            Content::Json(v) => {
                let text = serde_json::to_string_pretty(&v).unwrap_or_default();
                if let (true, Value::Object(m)) = (structured.is_none(), v) {
                    structured = Some(m);
                }
                json!({ "type": "text", "text": text })
            }
            Content::Png(bytes) => json!({
                "type": "image",
                "mimeType": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            }),
        })
        .collect();
    let mut v = json!({ "content": content, "isError": false });
    if let Some(m) = structured {
        v["structuredContent"] = Value::Object(m);
    }
    v
}
