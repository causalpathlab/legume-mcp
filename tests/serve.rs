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
            "echo", "fail", "count", "child", "picture", "panic", "write", "make", "peek",
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
            "peek" => {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "echo seen"]);
                Ok(vec![Content::Text(cx.run_reading(cmd)?)])
            }
            "make" => {
                let (out, from) = (
                    args["out"].as_str().unwrap(),
                    args["from"].as_str().unwrap(),
                );
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "cp \"$0\" \"$1\"", from, out]);
                let out = std::path::PathBuf::from(out);
                let name = std::path::Path::new(from)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                cx.run_making(
                    cmd,
                    std::slice::from_ref(&out),
                    &format!("made from {name}"),
                )?;
                cx.provenance(&out, &format!("copied from {name}"));
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
    let told = r["instructions"].as_str().unwrap();
    assert!(
        told.starts_with("read me\n\n"),
        "the server's own first: {told}"
    );
    assert!(told.contains("job_status"), "then the jobs: {told}");
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
    let mut input =
        b"not json\n\n\xff\xfe\n[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n".to_vec();
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":\"p\",\"method\":\"ping\"}\n");
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
        msgs[1]["error"]["code"], -32700,
        "not UTF-8: answered, and the session goes on"
    );
    assert_eq!(msgs[2]["error"]["code"], -32600, "no batches");
    assert_eq!(
        msgs[3],
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
fn the_records_redo_the_work_in_a_copy_of_the_project() {
    let base = std::env::temp_dir().join(format!("legume-mcp-records-{}", std::process::id()));
    let proj = base.join("proj");
    std::fs::create_dir_all(proj.join("data")).unwrap();
    let at = |p: &str| proj.join(p).to_string_lossy().into_owned();
    let made = at("data/made.txt");
    let input: String = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        call(
            3,
            "notebook",
            json!({ "kind": "plan", "text": "Check *x*." }),
        ),
        call(4, "echo", json!({ "a": 1 })),
        call(
            5,
            "write",
            json!({ "path": at("in.tsv"), "text": "a\tb\nLEGUME_MCP_EOF\n" }),
        ),
        call(6, "write", json!({ "path": at("odd.txt"), "text": ODD })),
        call(7, "make", json!({ "out": made, "from": at("in.tsv") })),
        call(8, "child", json!({ "script": "exit 2" })),
        call(10, "peek", json!({})),
        call(9, "notebook", json!({ "kind": "plan" })),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect();
    let sink = Sink::default();
    let info = ServerInfo {
        name: "t".into(),
        version: "0".into(),
        instructions: None,
    };
    legume_mcp::serve_in(
        Cursor::new(input),
        sink.clone(),
        info,
        Some(proj.clone()),
        Tools { calls: 0 },
    )
    .unwrap();
    let reply = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let msgs: Vec<Value> = reply
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(by_id(&msgs, 1)["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("results/<today>/runall.sh"));
    let tools = by_id(&msgs, 2)["result"]["tools"].as_array().unwrap();
    assert!(
        tools.iter().any(|t| t["name"] == "notebook"),
        "the notebook is built in"
    );
    assert_eq!(by_id(&msgs, 9)["result"]["isError"], true);

    let day: Vec<_> = std::fs::read_dir(proj.join("results"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(day.len(), 1, "one folder for the day");
    let day = day[0].path();
    let script = std::fs::read_to_string(day.join("runall.sh")).unwrap();
    assert!(script.starts_with("#!/usr/bin/env bash\n"), "{script}");
    assert!(
        script.contains("cd \"$(dirname \"$0\")/../..\""),
        "{script}"
    );
    assert!(
        script.contains(" t 0 echo {\"a\":1}\n"),
        "every call is a comment: {script}"
    );
    assert!(
        !script.contains(&proj.to_string_lossy().into_owned()),
        "paths are relative: {script}"
    );
    assert!(
        script.contains("if [ ! -e data/made.txt ]; then\n  mkdir -p data\n  sh -c "),
        "restartable: {script}"
    );
    assert!(script.contains("# did not succeed ("), "{script}");
    assert!(
        script.contains(" t 0 peek {}\n") && !script.contains("echo seen"),
        "a read is only its call: {script}"
    );
    assert!(
        script.contains("<<'LEGUME_MCP_EOF_'\na\tb\nLEGUME_MCP_EOF\nLEGUME_MCP_EOF_\n"),
        "{script}"
    );
    assert!(
        script.contains("base64 -d > odd.txt"),
        "control bytes go base64: {script}"
    );
    let notebook = std::fs::read_to_string(day.join("notebook.md")).unwrap();
    assert!(
        notebook.contains(" · t · plan\n\nCheck *x*.\n"),
        "{notebook}"
    );
    let index = std::fs::read_to_string(proj.join("results/INDEX.tsv")).unwrap();
    let rows: Vec<&str> = index.lines().collect();
    assert_eq!(rows[0], "date\ttime\tserver\ttool\toutput\tsummary");
    assert!(
        rows[1].ends_with("\tt\tmake\tdata/made.txt\tmade from in.tsv"),
        "{index}"
    );
    let readme = std::fs::read_to_string(proj.join("data/README.md")).unwrap();
    assert!(
        readme.contains("`data/made.txt`: copied from in.tsv (t 0 make)"),
        "{readme}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(day.join("runall.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "the script is the user's");
    }

    // A copy of the project, elsewhere and without the outputs, redoes them.
    let copy = base.join("copy");
    let rel_day = day.strip_prefix(&proj).unwrap();
    std::fs::create_dir_all(copy.join(rel_day)).unwrap();
    std::fs::copy(day.join("runall.sh"), copy.join(rel_day).join("runall.sh")).unwrap();
    let ran = Command::new("bash")
        .arg(copy.join(rel_day).join("runall.sh"))
        .status()
        .unwrap();
    assert!(ran.success());
    assert_eq!(
        std::fs::read_to_string(copy.join("data/made.txt")).unwrap(),
        "a\tb\nLEGUME_MCP_EOF\n"
    );
    assert_eq!(std::fs::read_to_string(copy.join("odd.txt")).unwrap(), ODD);
    assert!(!copy.join("pwned").exists(), "nothing past a false end ran");
    // Run again: the step whose output exists is skipped.
    std::fs::write(copy.join("data/made.txt"), "kept").unwrap();
    assert!(Command::new("bash")
        .arg(copy.join(rel_day).join("runall.sh"))
        .status()
        .unwrap()
        .success());
    assert_eq!(
        std::fs::read_to_string(copy.join("data/made.txt")).unwrap(),
        "kept"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_failed_childs_error_is_quoted_past_its_backtrace() {
    // As a Rust program fails with a backtrace on: its error, then many
    // frames. The quote starts at the error; the child was asked for none.
    let script = "echo \"backtrace asked: $RUST_BACKTRACE $RUST_LIB_BACKTRACE\" >&2; \
                  echo 'Error: no section matches qc' >&2; \
                  for i in $(seq 40); do echo \"  $i: frame\" >&2; done; exit 1";
    let msgs = session(&[call(1, "child", json!({ "script": script }))]);
    let text = by_id(&msgs, 1)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("Error: no section matches qc"), "{text}");
    assert!(
        !text.contains("40: frame"),
        "the quote stops before the frames run out: {text}"
    );
}

#[test]
fn a_child_is_asked_for_no_backtrace() {
    let script = "printf \"$RUST_BACKTRACE/$RUST_LIB_BACKTRACE\"";
    let msgs = session(&[call(1, "child", json!({ "script": script }))]);
    assert_eq!(by_id(&msgs, 1)["result"]["content"][0]["text"], "0/0");
}
