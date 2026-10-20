use anyhow::Result;
use legume_mcp::{serve_on, Call, Content, Handler, ServerInfo, ToolSpec};
use serde_json::{json, Value};
use std::io::{Cursor, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Tools {
    calls: usize,
}

impl Handler for Tools {
    fn tools(&self) -> Vec<ToolSpec> {
        [
            "echo", "fail", "count", "child", "picture", "panic", "write",
        ]
        .map(|n| ToolSpec::new(n, n, json!({ "type": "object" })))
        .into()
    }

    fn call(&mut self, name: &str, args: Value, cx: &Call) -> Result<Vec<Content>> {
        self.calls += 1;
        match name {
            "echo" => Ok(vec![Content::Json(args)]),
            "fail" => Err(anyhow::anyhow!("inner").context("outer")),
            "count" => Ok(vec![Content::Text(self.calls.to_string())]),
            "child" => {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", args["script"].as_str().unwrap()]);
                Ok(vec![Content::Text(cx.run(cmd)?)])
            }
            "picture" => Ok(vec![Content::Png(vec![0x89, b'P', b'N', b'G'])]),
            "write" => {
                let path = std::path::PathBuf::from(args["path"].as_str().unwrap());
                cx.write_file(&path, args["text"].as_str().unwrap())?;
                Ok(vec![])
            }
            _ => panic!("boom"),
        }
    }
}

/// Serve `lines` to the end and return every message written, in order.
fn session(lines: &[Value]) -> Vec<Value> {
    let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let sink = Sink::default();
    let info = ServerInfo {
        name: "test".into(),
        version: "0.0.0".into(),
        instructions: Some("read me".into()),
    };
    serve_on(Cursor::new(input), sink.clone(), info, Tools { calls: 0 }).unwrap();
    let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args } })
}

fn by_id(msgs: &[Value], id: u64) -> &Value {
    msgs.iter().find(|m| m["id"] == json!(id)).unwrap()
}

#[test]
fn initialize_negotiates_the_version() {
    let msgs = session(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {} } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize",
                "params": { "protocolVersion": "1999-01-01" } }),
    ]);
    assert_eq!(msgs.len(), 2, "a notification gets no answer");
    let r = &by_id(&msgs, 1)["result"];
    assert_eq!(r["protocolVersion"], "2025-06-18");
    assert_eq!(r["serverInfo"]["name"], "test");
    assert_eq!(r["instructions"], "read me");
    assert!(r["capabilities"]["tools"].is_object());
    assert_eq!(
        by_id(&msgs, 2)["result"]["protocolVersion"],
        legume_mcp::PROTOCOL_VERSIONS[0]
    );
}

#[test]
fn lists_and_calls_tools() {
    let msgs = session(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        call(2, "echo", json!({ "a": 1 })),
        call(3, "fail", json!({})),
        call(4, "nope", json!({})),
        call(5, "count", Value::Null),
        call(6, "picture", json!({})),
        json!({ "jsonrpc": "2.0", "id": 7, "method": "resources/list" }),
        call(8, "panic", json!({})),
        call(9, "count", json!({})),
    ]);
    let tools = by_id(&msgs, 1)["result"]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "echo");
    assert!(tools[0]["inputSchema"].is_object());

    let echo = &by_id(&msgs, 2)["result"];
    assert_eq!(echo["isError"], false);
    assert_eq!(echo["structuredContent"], json!({ "a": 1 }));
    assert_eq!(echo["content"][0]["type"], "text");

    let fail = &by_id(&msgs, 3)["result"];
    assert_eq!(fail["isError"], true);
    assert_eq!(fail["content"][0]["text"], "outer: inner");

    assert_eq!(by_id(&msgs, 4)["error"]["code"], -32602);
    assert_eq!(by_id(&msgs, 5)["result"]["content"][0]["text"], "3");
    let png = &by_id(&msgs, 6)["result"]["content"][0];
    assert_eq!(png["type"], "image");
    assert_eq!(png["mimeType"], "image/png");
    assert_eq!(png["data"], "iVBORw==");
    assert_eq!(by_id(&msgs, 7)["error"]["code"], -32601);

    let panic = &by_id(&msgs, 8)["result"];
    assert_eq!(panic["isError"], true);
    assert!(panic["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("boom"));
    assert_eq!(
        by_id(&msgs, 9)["result"]["content"][0]["text"],
        "6",
        "the handler survives a panic"
    );
}

#[test]
fn bad_lines_and_pings() {
    let input = "not json\n\n{\"jsonrpc\":\"2.0\",\"id\":\"p\",\"method\":\"ping\"}\n";
    let sink = Sink::default();
    serve_on(
        Cursor::new(input),
        sink.clone(),
        ServerInfo::default(),
        Tools { calls: 0 },
    )
    .unwrap();
    let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let msgs: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(msgs[0]["error"]["code"], -32700);
    assert_eq!(msgs[0]["id"], Value::Null);
    assert_eq!(
        msgs[1],
        json!({ "jsonrpc": "2.0", "id": "p", "result": {} })
    );
}

#[test]
fn child_runs_report_progress_and_failures() {
    let mut with_token = call(
        1,
        "child",
        json!({ "script": "echo one >&2; echo two >&2; printf out" }),
    );
    with_token["params"]["_meta"] = json!({ "progressToken": "t" });
    let msgs = session(&[
        with_token,
        call(2, "child", json!({ "script": "echo quiet >&2" })),
        call(3, "child", json!({ "script": "echo why >&2; exit 3" })),
    ]);
    let progress: Vec<&Value> = msgs
        .iter()
        .filter(|m| m["method"] == "notifications/progress")
        .collect();
    assert_eq!(
        progress.len(),
        2,
        "only the call with a token hears progress"
    );
    assert_eq!(progress[0]["params"]["progressToken"], "t");
    assert_eq!(progress[0]["params"]["message"], "one");
    assert_eq!(progress[1]["params"]["progress"], 2);
    assert_eq!(by_id(&msgs, 1)["result"]["content"][0]["text"], "out");
    let failed = &by_id(&msgs, 3)["result"];
    assert_eq!(failed["isError"], true);
    let text = failed["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("why") && text.contains("exit status: 3"),
        "{text}"
    );
}

#[test]
fn a_cancelled_call_is_killed_and_unanswered() {
    let start = std::time::Instant::now();
    let msgs = session(&[
        call(1, "child", json!({ "script": "sleep 30" })),
        json!({ "jsonrpc": "2.0", "method": "notifications/cancelled",
                "params": { "requestId": 1, "reason": "test" } }),
        call(2, "count", json!({})),
    ]);
    assert!(start.elapsed().as_secs() < 10, "the child was killed");
    assert!(msgs.iter().all(|m| m["id"] != json!(1)));
    assert_eq!(by_id(&msgs, 2)["result"]["content"][0]["text"], "2");
}

/// Contents bash and Rust could split differently: a NUL and a CR beside
/// the heredoc's end word, then a command.
const ODD: &str = "x\nLEGUME_MCP_EOF\u{0}\ntouch pwned\r\nLEGUME_MCP_EOF\r\n";

#[test]
fn the_session_script_repeats_what_the_tools_wrote() {
    let dir = std::env::temp_dir().join(format!("legume-mcp-journal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("s.sh");
    let made = dir.join("made it.txt");
    let input: String = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        call(2, "echo", json!({ "a": 1 })),
        call(
            3,
            "child",
            json!({ "script": format!("printf 'x\\n' > '{}'", made.display()) }),
        ),
        call(4, "child", json!({ "script": "exit 2" })),
        call(
            5,
            "write",
            json!({ "path": dir.join("in.tsv"), "text": "a\tb\nLEGUME_MCP_EOF\n" }),
        ),
        call(
            6,
            "write",
            json!({ "path": dir.join("odd.txt"), "text": ODD }),
        ),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect();
    let sink = Sink::default();
    legume_mcp::serve_journaled(
        Cursor::new(input),
        sink.clone(),
        ServerInfo::default(),
        Some(script.clone()),
        Tools { calls: 0 },
    )
    .unwrap();
    let text = std::fs::read_to_string(&script).unwrap();
    assert!(text.starts_with("#!/usr/bin/env bash\n"), "{text}");
    assert!(text.contains("set -euo pipefail\ncd "), "{text}");
    assert!(
        text.contains(" echo {\"a\":1}\n"),
        "every call is a comment: {text}"
    );
    assert!(
        text.contains("\nsh -c 'printf "),
        "a child is its command: {text}"
    );
    assert!(text.contains("# did not succeed ("), "{text}");
    assert!(
        text.contains("<<'LEGUME_MCP_EOF_'\na\tb\nLEGUME_MCP_EOF\nLEGUME_MCP_EOF_\n"),
        "a written input is a heredoc that its contents cannot end: {text}"
    );
    let reply = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(
        reply.contains(&script.display().to_string()),
        "the instructions name the script"
    );

    // The script redoes the work.
    std::fs::remove_file(&made).unwrap();
    let ran = Command::new("bash").arg(&script).status().unwrap();
    assert!(ran.success());
    assert_eq!(std::fs::read_to_string(&made).unwrap(), "x\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("in.tsv")).unwrap(),
        "a\tb\nLEGUME_MCP_EOF\n"
    );
    assert!(
        text.contains("base64 -d > "),
        "control bytes go base64: {text}"
    );
    assert_eq!(std::fs::read_to_string(dir.join("odd.txt")).unwrap(), ODD);
    assert!(!dir.join("pwned").exists(), "nothing past a false end ran");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the script is the user's to read");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
