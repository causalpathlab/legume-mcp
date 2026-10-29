use anyhow::Result;
use legume_mcp::{serve_on, Call, Content, Fact, Handler, ServerInfo, Step, ToolSpec};
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
                if let Some(b) = args["backtrace"].as_str() {
                    cmd.env("RUST_BACKTRACE", b);
                }
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
                // Its inputs read from the command; a fact found once done.
                let step = Step::new(std::slice::from_ref(&out), &format!("made from {name}"))
                    .after(|_, outs| {
                        let n = std::fs::metadata(&outs[0]).unwrap().len();
                        vec![Fact::count("bytes", n).rule("cp").flag("more than 10")]
                    });
                cx.run_step(cmd, step)?;
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
    assert_eq!(rows[0], legume_mcp::project::INDEX_HEAD);
    assert!(
        rows[1].ends_with(
            "\tt\tmake\tdata/made.txt\tmade from in.tsv\tin.tsv\t\
             [{\"name\":\"bytes\",\"value\":19,\"rule\":\"cp\",\"flag\":\"more than 10\"}]"
        ),
        "the inputs the command names, and the facts: {index}"
    );
    // The legume server shows them, the flagged ones apart.
    let sink = Sink::default();
    let ask = format!(
        "{}\n{}\n",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        call(2, "status", json!({}))
    );
    legume_mcp::brain::serve_in(Cursor::new(ask), sink.clone(), Some(proj.clone())).unwrap();
    let reply = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let status: Vec<Value> = reply
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let status = by_id(&status, 2)["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        status.contains("t make: data/made.txt (made from in.tsv)\n      bytes 19 (cp; flagged: more than 10)\n"),
        "{status}"
    );
    assert!(
        status.contains("Flagged by the tools")
            && status.contains("  data/made.txt (t make): bytes 19 (cp; flagged: more than 10)\n"),
        "{status}"
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
fn the_notebook_numbers_readings_and_keeps_who_decided() {
    let base = std::env::temp_dir().join(format!("legume-mcp-notebook-{}", std::process::id()));
    let proj = base.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let note = |id, args| call(id, "notebook", args);
    let ifn = json!({
        "claim": "Topic 7 is an interferon response",
        "evidence": [{ "kind": "marker", "term": "ISG15" }, { "kind": "marker", "term": "MX1" }],
        "alternatives": [{ "label": "batch", "why_not": "it spans\nboth runs" }],
        "rationale": "3x higher in treated donors",
        "check": "pseudobulk per donor",
    });
    let batch = json!({ "claim": "Topic 7 is run B", "rationale": "run B has more", "check": "within-run test" });
    let input: String = [
        note(10, json!({ "kind": "answer", "chose": [2], "words": "2" })),
        note(
            1,
            json!({ "kind": "interpretation", "readings": [ifn, batch] }),
        ),
        note(2, json!({ "kind": "interpretation", "text": "One idea." })),
        note(
            3,
            json!({ "kind": "interpretation", "readings": [ifn, { "claim": "x" }] }),
        ),
        note(4, json!({ "kind": "conclusion", "text": "Old name." })),
        note(11, json!({ "kind": "answer", "chose": [3], "words": "3" })),
        note(12, json!({ "kind": "answer", "chose": [1] })),
        note(
            5,
            json!({ "kind": "answer", "chose": [1], "words": "R1, with care." }),
        ),
        // Answered once only; a new proposal is answered anew.
        note(23, json!({ "kind": "answer", "chose": [2], "words": "continue" })),
        note(
            21,
            json!({ "kind": "interpretation", "readings": [ifn, batch] }),
        ),
        note(13, json!({ "kind": "choice", "chose": [1], "words": "1" })),
        note(
            6,
            json!({ "kind": "answer", "words": "Neither: doublets.\nSeen it before." }),
        ),
        note(7, json!({ "kind": "note", "text": "Mine.", "by": "user" })),
        note(
            8,
            json!({ "kind": "note", "text": "Who?", "by": "someone" }),
        ),
        note(9, json!({ "kind": "observation" })),
        note(14, json!({ "kind": "offer", "options": ["Stop here"] })),
        note(
            15,
            json!({ "kind": "note", "text": "Lines.", "options": ["a", "b"] }),
        ),
        note(
            16,
            json!({ "kind": "offer", "text": "Next lines for R1.", "options": ["Replication on donors 5-6", "Stop here"] }),
        ),
        note(17, json!({ "kind": "answer", "chose": [1], "words": "1" })),
        note(18, json!({ "kind": "choice", "chose": [3], "words": "3" })),
        note(
            19,
            json!({ "kind": "choice", "chose": [1], "words": "1, then stop" }),
        ),
        note(
            22,
            json!({ "kind": "offer", "options": ["Look again", "Stop here"] }),
        ),
        note(20, json!({ "kind": "choice", "words": "not now" })),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect();
    let sink = Sink::default();
    legume_mcp::brain::serve_in(Cursor::new(input), sink.clone(), Some(proj.clone())).unwrap();
    let reply = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let msgs: Vec<Value> = reply
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (id, refused) in [
        (1, false),
        (2, true),
        (3, true),
        (4, false),
        (5, false),
        (6, false),
        (7, false),
        (8, true),
        (9, true),
        (10, true),
        (11, true),
        (12, true),
        (13, true),
        (14, true),
        (15, true),
        (16, false),
        (17, true),
        (18, true),
        (19, false),
        (20, false),
        (21, false),
        (22, false),
        (23, true),
    ] {
        assert_eq!(by_id(&msgs, id)["result"]["isError"], refused, "call {id}");
    }
    let refusal = |id| {
        by_id(&msgs, id)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(refusal(10).contains("no proposal today"), "{}", refusal(10));
    assert!(
        refusal(23).contains("has its reply already"),
        "{}",
        refusal(23)
    );
    assert!(refusal(11).contains("(1 to 2), not 3"), "{}", refusal(11));
    assert!(
        refusal(12).contains("`words` is required"),
        "{}",
        refusal(12)
    );
    assert!(
        refusal(13).contains("the latest question today is the proposal of"),
        "{}",
        refusal(13)
    );
    assert!(
        refusal(14).contains("two or more `options`"),
        "{}",
        refusal(14)
    );
    assert!(
        refusal(15).contains("`options` go with kind `offer`"),
        "{}",
        refusal(15)
    );
    assert!(
        refusal(17).contains("the latest question today is the offer of"),
        "{}",
        refusal(17)
    );
    assert!(
        refusal(18).contains("option numbers of the offer of")
            && refusal(18).contains("(1 to 2), not 3"),
        "{}",
        refusal(18)
    );
    assert!(by_id(&msgs, 3)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("`rationale` is missing"));

    let day = std::fs::read_dir(proj.join("results"))
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    let notebook = std::fs::read_to_string(day.join("notebook.md")).unwrap();
    assert!(
        notebook.contains(
            " · legume · interpretation (proposed)\n\n\
             **R1. Topic 7 is an interferon response**\n\
             - Evidence: marker ISG15; marker MX1\n\
             - Not batch: it spans both runs\n\
             - Rationale: 3x higher in treated donors\n\
             - Check: pseudobulk per donor\n\n\
             **R2. Topic 7 is run B**\n\
             - Rationale: run B has more\n\
             - Check: within-run test\n"
        ),
        "{notebook}"
    );
    for heading in [
        " · legume · interpretation (proposed)\n\nOld name.\n",
        " (legume): chose R1. Hypotheses: R1 = H1.\n\n> R1, with care.\n",
        " (legume): set aside.\n\n> Neither: doublets.\n> Seen it before.\n",
        " · legume · interpretation (accepted by the user)\n\nAnswers the proposal of ",
        " · legume · interpretation (user override)\n\nAnswers the proposal of ",
        " · legume · note (user)\n\nMine.\n",
        " · legume · offer\n\nNext lines for R1.\n\n- **1.** Replication on donors 5-6\n- **2.** Stop here\n",
        " · legume · choice (user)\n\nAnswers the offer of ",
        " (legume): chose 1.\n\n> 1, then stop\n",
        " (legume): chose none.\n\n> not now\n",
    ] {
        assert!(notebook.contains(heading), "{heading:?} in {notebook}");
    }
    for refused in ["One idea.", "Who?", "Lines."] {
        assert!(!notebook.contains(refused), "{notebook}");
    }
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn evidence_builds_each_hypothesis_triangulation_table() {
    let base = std::env::temp_dir().join(format!("legume-mcp-evidence-{}", std::process::id()));
    let proj = base.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let note = |id, args| call(id, "notebook", args);
    let reading = |claim: &str| json!({ "claim": claim, "rationale": "r", "check": "c" });
    let ev = |h: &str, source: &str, class: &str, verdict: &str, text: &str| json!({ "kind": "evidence", "hypothesis": h, "source": source, "class": class, "verdict": verdict, "text": text });
    let input: String = [
        note(1, json!({ "kind": "evidence", "hypothesis": "H1", "source": "s", "class": "our_cells", "verdict": "agrees", "text": "t" })),
        note(2, json!({ "kind": "interpretation", "readings": [reading("IFN drives topic 7"), reading("Two donors carry topic 7")] })),
        note(3, json!({ "kind": "answer", "chose": [1, 2], "words": "both" })),
        note(4, ev("H9", "s", "our_cells", "agrees", "t")),
        note(5, ev("H1", "s", "cells", "agrees", "t")),
        note(6, ev("H1", "s", "our_cells", "agrees", " ")),
        note(7, ev("H1", "donors 1-4", "our_cells", "agrees", "ISG15, MX1 top markers; cocoa 212 genes at FDR < 0.1")),
        note(8, ev("h1", "donors 5-6", "other_donors", "agrees", "5 of 6 donors higher")),
        note(9, ev("1", "donors 5-6", "other_study", "agrees", "t")),
        note(10, ev("H1", "eQTL catalogue", "genetics", "agrees", "IRF7 eQTL in monocytes")),
        note(11, ev("H2", "donors 1-4", "our_cells", "disagrees", "both treated donors above both controls")),
        note(12, json!({ "kind": "note", "text": "x", "hypothesis": "H1" })),
        note(13, json!({ "kind": "interpretation", "readings": [reading("Something else"), reading("IFN drives topic 7")] })),
        note(14, json!({ "kind": "answer", "chose": [2, 1], "words": "2 and 1" })),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect();
    let sink = Sink::default();
    legume_mcp::brain::serve_in(Cursor::new(input), sink.clone(), Some(proj.clone())).unwrap();
    let reply = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let msgs: Vec<Value> = reply
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let text = |id| {
        by_id(&msgs, id)["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    for (id, refused) in [
        (1, true),
        (2, false),
        (3, false),
        (4, true),
        (5, true),
        (6, true),
        (7, false),
        (8, false),
        (9, true),
        (10, false),
        (11, false),
        (12, true),
        (13, false),
        (14, false),
    ] {
        assert_eq!(
            by_id(&msgs, id)["result"]["isError"],
            refused,
            "call {id}: {}",
            text(id)
        );
    }
    assert!(
        text(1).contains("there is no hypothesis H1 (the hypotheses are none yet"),
        "{}",
        text(1)
    );
    assert!(text(3).starts_with("R1 = H1, R2 = H2."), "{}", text(3));
    assert!(
        text(4).contains("the hypotheses are H1 to H2"),
        "{}",
        text(4)
    );
    assert!(
        text(5).contains("`class` is one of our_cells, same_cells"),
        "{}",
        text(5)
    );
    assert!(text(6).contains("`text` is required"), "{}", text(6));
    assert!(
        text(7).contains("**H1. IFN drives topic 7**"),
        "{}",
        text(7)
    );
    assert!(text(7).contains("Verb: \"marks\""), "{}", text(7));
    assert!(
        text(7).contains("Sources not yet used for H1, those that would raise the verb first: other donors, same study (`other_donors`); another study (`other_study`)"),
        "{}",
        text(7)
    );
    assert!(
        text(8).contains("Verb: \"is associated with\". Standing: replicated"),
        "{}",
        text(8)
    );
    assert!(text(8).contains("used for H1, those that would raise the verb first: genetics (`genetics`); perturbation"), "{}", text(8));
    assert!(
        text(9).contains("was recorded as class `other_donors`"),
        "{}",
        text(9)
    );
    assert!(
        text(10).contains("Verb: \"affects\", \"drives\". Standing: replicated, with genetics"),
        "{}",
        text(10)
    );
    assert!(text(11).contains("Standing: no source agrees. Name with the verb what does not agree: donors 1-4 (disagrees)."), "{}", text(11));
    assert!(text(12).contains("go with kind `evidence`"), "{}", text(12));
    assert!(text(14).starts_with("R2 = H1, R1 = H3."), "{}", text(14));

    let results = proj.join("results");
    let hyps = std::fs::read_to_string(results.join("HYPOTHESES.tsv")).unwrap();
    assert!(
        hyps.starts_with("date\ttime\tserver\tid\tfrom\tclaim\n"),
        "{hyps}"
    );
    assert_eq!(hyps.lines().count(), 4, "{hyps}");
    assert!(
        hyps.contains("\tlegume\tH3\tR1 of the proposal of "),
        "{hyps}"
    );
    let evidence = std::fs::read_to_string(results.join("EVIDENCE.tsv")).unwrap();
    assert_eq!(
        evidence.lines().count(),
        5,
        "only the evidence taken: {evidence}"
    );
    assert!(
        evidence.contains("\tlegume\tH1\tdonors 5-6\tother_donors\tagrees\t5 of 6 donors higher\n"),
        "{evidence}"
    );

    let day = std::fs::read_dir(&results)
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    let notebook = std::fs::read_to_string(day.join("notebook.md")).unwrap();
    for part in [
        " (legume): chose R1, R2. Hypotheses: R1 = H1, R2 = H2.\n\n> both\n",
        " · legume · evidence\n\nH1 from donors 5-6 (other donors): agrees.\n\n5 of 6 donors higher\n\n**H1. IFN drives topic 7**\n",
        "| donors 5-6 | other donors, same study | lab, platform | agrees | 5 of 6 donors higher |\n",
    ] {
        assert!(notebook.contains(part), "{part:?} in {notebook}");
    }
    assert!(
        !notebook.contains("Sources not yet used"),
        "the hints are for the agent: {notebook}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_tools_notebook_leaves_the_reasoning_to_the_legume_server() {
    let base = std::env::temp_dir().join(format!("legume-mcp-roles-{}", std::process::id()));
    let proj = base.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let init = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
    let list = json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/list" });
    let lines = |calls: &[Value]| -> String {
        std::iter::once(&init)
            .chain(calls)
            .chain(std::iter::once(&list))
            .map(|l| format!("{l}\n"))
            .collect()
    };
    let reading = |claim: &str| json!({ "claim": claim, "rationale": "r", "check": "c" });

    // A tool's server: plan, observation and note only.
    let tool = lines(&[
        call(
            1,
            "notebook",
            json!({ "kind": "plan", "text": "Question: why topic 7?" }),
        ),
        call(
            2,
            "notebook",
            json!({ "kind": "interpretation", "readings": [reading("a"), reading("b")] }),
        ),
        call(
            3,
            "notebook",
            json!({ "kind": "conclusion", "text": "Old name, still taken." }),
        ),
    ]);
    let sink = Sink::default();
    let info = ServerInfo {
        name: "t".into(),
        version: "0".into(),
        instructions: None,
    };
    legume_mcp::serve_in(
        Cursor::new(tool),
        sink.clone(),
        info,
        Some(proj.clone()),
        Tools { calls: 0 },
    )
    .unwrap();
    let msgs: Vec<Value> = String::from_utf8(sink.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(by_id(&msgs, 1)["result"]["isError"], false);
    assert_eq!(by_id(&msgs, 2)["result"]["isError"], true);
    let refusal = by_id(&msgs, 2)["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        refusal.contains("goes to the `legume` server's notebook"),
        "{refusal}"
    );
    assert_eq!(by_id(&msgs, 3)["result"]["isError"], false);
    let instructions = by_id(&msgs, 0)["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("in the notebook of the `legume` server"),
        "{instructions}"
    );
    assert!(!instructions.contains("`evidence`"), "{instructions}");
    let tools = by_id(&msgs, 99)["result"]["tools"].as_array().unwrap();
    let notebook = tools.iter().find(|t| t["name"] == "notebook").unwrap();
    assert_eq!(
        notebook["inputSchema"]["properties"]["kind"]["enum"],
        json!(["plan", "observation", "note"])
    );
    assert!(tools.iter().any(|t| t["name"] == "job_wait"));
    let cancel = tools.iter().find(|t| t["name"] == "job_cancel").unwrap();
    assert_eq!(
        cancel["annotations"],
        json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true })
    );
    assert!(
        tools
            .iter()
            .all(|t| t["name"] != "echo" || t.get("annotations").is_none()),
        "a tool that says nothing of how it behaves sends no hints"
    );

    // The legume server: the reasoning, the guide, the project's standing.
    let brain = lines(&[
        call(1, "guide", json!({})),
        call(2, "guide", json!({ "section": "8.3" })),
        call(
            3,
            "notebook",
            json!({ "kind": "interpretation", "readings": [reading("IFN drives topic 7"), reading("Donor 2 carries topic 7")] }),
        ),
        call(4, "status", json!({})),
        call(
            5,
            "notebook",
            json!({ "kind": "answer", "chose": [1], "words": "1" }),
        ),
        call(
            6,
            "notebook",
            json!({ "kind": "evidence", "hypothesis": "H1", "source": "donors 1-4", "class": "our_cells", "verdict": "agrees", "text": "ISGs top" }),
        ),
        call(7, "status", json!({})),
        call(8, "hypotheses", json!({})),
        call(9, "hypotheses", json!({ "id": "H1" })),
        call(10, "hypotheses", json!({ "id": "H2" })),
        json!({ "jsonrpc": "2.0", "id": 11, "method": "prompts/list" }),
        json!({ "jsonrpc": "2.0", "id": 12, "method": "prompts/get",
                "params": { "name": "explore", "arguments": { "question": "why topic 7?" } } }),
        json!({ "jsonrpc": "2.0", "id": 13, "method": "prompts/get",
                "params": { "name": "triangulate" } }),
        json!({ "jsonrpc": "2.0", "id": 14, "method": "prompts/get", "params": { "name": "nope" } }),
    ]);
    let sink = Sink::default();
    legume_mcp::brain::serve_in(Cursor::new(brain), sink.clone(), Some(proj.clone())).unwrap();
    let msgs: Vec<Value> = String::from_utf8(sink.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let text = |id| {
        by_id(&msgs, id)["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(by_id(&msgs, 0)["result"]["serverInfo"]["name"], "legume");
    let instructions = by_id(&msgs, 0)["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("Quick card for agents:"),
        "{instructions}"
    );
    assert!(
        instructions.contains("record what each source says of it as\n`evidence`")
            || instructions.contains("`evidence`"),
        "{instructions}"
    );
    let names: Vec<&str> = by_id(&msgs, 99)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["guide", "status", "hypotheses", "report", "notebook"],
        "no job tools: the legume server runs no steps"
    );
    // How each behaves, for the client; and the recipes, as prompts.
    let listed = by_id(&msgs, 99)["result"]["tools"].as_array().unwrap();
    assert_eq!(listed[0]["annotations"], json!({ "readOnlyHint": true }));
    assert_eq!(
        listed[3]["annotations"],
        json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true })
    );
    assert_eq!(
        listed[4]["annotations"],
        json!({ "readOnlyHint": false, "destructiveHint": false })
    );
    assert_eq!(
        by_id(&msgs, 0)["result"]["capabilities"]["prompts"],
        json!({ "listChanged": false })
    );
    let prompts: Vec<&str> = by_id(&msgs, 11)["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(prompts, ["explore", "triangulate"]);
    let explore = by_id(&msgs, 12)["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(
        explore.contains("Question: why topic 7?\nData: not given\n")
            && explore.contains("Call `status`"),
        "{explore}"
    );
    let tri = by_id(&msgs, 13)["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(tri.contains("Hypothesis: not given\n"), "{tri}");
    assert_eq!(by_id(&msgs, 14)["error"]["code"], -32602);
    assert!(
        text(1).contains("8.3        Sources of evidence"),
        "{}",
        text(1)
    );
    assert!(
        text(2).starts_with("8.3 Sources of evidence\n"),
        "{}",
        text(2)
    );
    assert!(
        text(4).contains("Waiting for the user: the proposal of "),
        "{}",
        text(4)
    );
    assert!(
        text(4).contains("R1. IFN drives topic 7 / R2. Donor 2 carries topic 7"),
        "{}",
        text(4)
    );
    assert!(
        text(4).contains("Question: why topic 7?") || text(4).contains("· t · plan"),
        "{}",
        text(4)
    );
    assert!(!text(7).contains("Waiting for the user"), "{}", text(7));
    assert!(text(7).contains("H1. IFN drives topic 7 (one group of cells, not replicated; sources: 1 agree, 0 disagree, 0 mixed)"), "{}", text(7));
    assert!(
        text(8).starts_with("H1. IFN drives topic 7 ("),
        "{}",
        text(8)
    );
    assert!(
        text(9).contains("| donors 1-4 | our cells | everything | agrees | ISGs top |"),
        "{}",
        text(9)
    );
    assert!(
        text(9).contains("Sources not yet used for H1"),
        "{}",
        text(9)
    );
    assert_eq!(by_id(&msgs, 10)["result"]["isError"], true);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_failed_childs_error_is_quoted_past_its_backtrace() {
    // As a Rust program fails with a backtrace on: its error, then many
    // frames. The quote starts at the error; the child was asked for none.
    let script = "echo \"backtrace asked: $RUST_BACKTRACE $RUST_LIB_BACKTRACE\" >&2; \
                  echo 'Error: no section matches qc' >&2; echo >&2; echo 'Stack backtrace:' >&2; \
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

#[test]
fn a_tool_that_asks_for_a_backtrace_keeps_it() {
    let script = "printf \"$RUST_BACKTRACE/$RUST_LIB_BACKTRACE\"";
    let msgs = session(&[call(
        1,
        "child",
        json!({ "script": script, "backtrace": "1" }),
    )]);
    assert_eq!(by_id(&msgs, 1)["result"]["content"][0]["text"], "1/0");
}

#[test]
fn an_answer_numbers_each_chosen_reading_once() {
    let base = tempfile::tempdir().unwrap();
    let proj = base.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let reading = |claim: &str| json!({ "claim": claim, "rationale": "r", "check": "c" });
    let propose = |id, a: &str, b: &str| {
        call(
            id,
            "notebook",
            json!({ "kind": "interpretation", "readings": [reading(a), reading(b)] }),
        )
    };
    let input: String = [
        json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} }),
        propose(1, "A", "B"),
        call(
            2,
            "notebook",
            json!({ "kind": "answer", "chose": [1, 2, 1], "words": "1, 2" }),
        ),
        propose(3, "C", "A"),
        call(
            4,
            "notebook",
            json!({ "kind": "answer", "chose": "both", "words": "both" }),
        ),
        call(
            5,
            "notebook",
            json!({ "kind": "answer", "chose": 2, "words": "2" }),
        ),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect();
    let sink = Sink::default();
    legume_mcp::brain::serve_in(Cursor::new(input), sink.clone(), Some(proj.clone())).unwrap();
    let msgs: Vec<Value> = String::from_utf8(sink.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let text = |id| {
        by_id(&msgs, id)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(text(2).starts_with("R1 = H1, R2 = H2."), "{}", text(2));
    assert_eq!(
        by_id(&msgs, 4)["result"]["isError"],
        true,
        "a word is not a number"
    );
    assert!(text(4).contains("`chose` is a list"), "{}", text(4));
    assert!(
        text(5).starts_with("R2 = H1."),
        "one number is one choice: {}",
        text(5)
    );
    let ledger = std::fs::read_to_string(proj.join("results/HYPOTHESES.tsv")).unwrap();
    assert_eq!(
        ledger.lines().count(),
        3,
        "a header and two hypotheses: {ledger}"
    );
}
