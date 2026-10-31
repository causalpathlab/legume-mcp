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
        !tools.iter().any(|t| t["name"] == "notebook"),
        "the notebook is the legume server's"
    );

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

/// The `legume` server's answers to `calls`, made in `proj`.
fn brain(proj: &std::path::Path, calls: &[Value]) -> Vec<Value> {
    let input: String = calls.iter().map(|l| format!("{l}\n")).collect();
    let sink = Sink::default();
    legume_mcp::brain::serve_in(Cursor::new(input), sink.clone(), Some(proj.to_path_buf()))
        .unwrap();
    let reply = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    reply
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The text of call `id`'s result.
fn said(msgs: &[Value], id: u64) -> String {
    by_id(msgs, id)["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn the_notebook_keeps_the_persons_words_and_numbers_their_hypotheses() {
    let base = tempfile::tempdir().unwrap();
    let proj = base.path().join("proj");
    std::fs::create_dir_all(proj.join("data")).unwrap();
    std::fs::write(proj.join("data/a.h5"), "x").unwrap();
    let note = |id, args| call(id, "notebook", args);
    let msgs = brain(
        &proj,
        &[
            note(
                1,
                json!({ "kind": "plan", "text": "The agent's question." }),
            ),
            note(
                2,
                json!({ "kind": "plan", "words": "Why topic 7, rather than topic 2?\nNot if donors split it." }),
            ),
            note(
                3,
                json!({ "kind": "observation", "text": "QC kept 9,812 of 11,204 cells." }),
            ),
            note(
                4,
                json!({ "kind": "hypothesis", "words": "Topic 7 is an interferon response" }),
            ),
            note(
                5,
                json!({ "kind": "hypothesis", "hypothesis": "H1", "rivals": ["batch", "a donor"], "nullify": "no ISGs in donors 5-6" }),
            ),
            note(
                6,
                json!({ "kind": "hypothesis", "hypothesis": "H5", "rivals": ["x"] }),
            ),
            note(7, json!({ "kind": "interpretation", "text": "It is IFN." })),
            note(8, json!({ "kind": "note", "text": "Mine.", "by": "user" })),
            note(
                9,
                json!({ "kind": "data", "path": "data/a.h5", "words": "the core's 10x run", "verdict": "fine" }),
            ),
            note(
                10,
                json!({ "kind": "data", "path": "data/a.h5", "words": "the core's 10x run", "verdict": "caveats" }),
            ),
            note(
                11,
                json!({ "kind": "summary", "words": "Topic 7 looks like IFN; one donor only." }),
            ),
            call(12, "status", json!({})),
        ],
    );
    for (id, refused) in [
        (1, true),
        (2, false),
        (3, false),
        (4, false),
        (5, false),
        (6, true),
        (7, true),
        (8, false),
        (9, true),
        (10, false),
        (11, false),
    ] {
        assert_eq!(
            by_id(&msgs, id)["result"]["isError"],
            refused,
            "call {id}: {}",
            said(&msgs, id)
        );
    }
    assert!(
        said(&msgs, 1).contains("`words` is required"),
        "{}",
        said(&msgs, 1)
    );
    assert!(
        said(&msgs, 4).contains("what else could explain it"),
        "{}",
        said(&msgs, 4)
    );
    assert_eq!(said(&msgs, 5), "Recorded as H1.");
    assert!(
        said(&msgs, 6).contains("there is no hypothesis H5"),
        "{}",
        said(&msgs, 6)
    );
    assert!(
        said(&msgs, 7).contains("`kind` is one of"),
        "{}",
        said(&msgs, 7)
    );
    let status = said(&msgs, 12);
    for want in [
        "data/a.h5 (1 kB): \"the core's 10x run\"; the person's verdict: caveats",
        "H1. Topic 7 is an interferon response",
    ] {
        assert!(status.contains(want), "{want} in {status}");
    }
    assert!(!status.contains("Open for the person"), "{status}");

    let results = proj.join("results");
    let hyps = std::fs::read_to_string(results.join("HYPOTHESES.tsv")).unwrap();
    assert!(
        hyps.starts_with("date\ttime\tserver\tid\tfrom\tclaim\trivals\tnullify\n"),
        "{hyps}"
    );
    assert!(
        hyps.contains("\tH1\tthe person, notebook ")
            && hyps.ends_with(
                "\tTopic 7 is an interferon response\tbatch; a donor\tno ISGs in donors 5-6\n"
            ),
        "{hyps}"
    );
    let day = std::fs::read_dir(&results)
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    let notebook = std::fs::read_to_string(day.join("notebook.md")).unwrap();
    for part in [
        " · legume · plan\n\n> Why topic 7, rather than topic 2?\n> Not if donors split it.\n",
        " · legume · hypothesis H1\n\n> Topic 7 is an interferon response\n",
        " · legume · hypothesis H1 (revised)\n\n> Topic 7 is an interferon response\n\nRivals they named: batch; a donor\n\nWould nullify it: no ISGs in donors 5-6\n",
        " · legume · note (user)\n\n> Mine.\n",
        " · legume · data\n\n`data/a.h5`, verdict: caveats\n\n> the core's 10x run\n",
        " · legume · summary\n\n> Topic 7 looks like IFN; one donor only.\n",
    ] {
        assert!(notebook.contains(part), "{part:?} in {notebook}");
    }
    assert!(!notebook.contains("The agent's question"), "{notebook}");
}

#[test]
fn evidence_builds_each_hypothesis_triangulation_table() {
    let base = tempfile::tempdir().unwrap();
    let proj = base.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let note = |id, args| call(id, "notebook", args);
    let ev = |h: &str, source: &str, class: &str, verdict: &str, text: &str| json!({ "kind": "evidence", "hypothesis": h, "source": source, "class": class, "verdict": verdict, "text": text, "words": format!("{verdict}, as I read it") });
    let msgs = brain(
        &proj,
        &[
            note(1, ev("H1", "s", "our_cells", "agrees", "t")),
            note(
                2,
                json!({ "kind": "hypothesis", "words": "IFN drives topic 7", "nullify": "no ISGs in held-out donors" }),
            ),
            note(
                3,
                json!({ "kind": "hypothesis", "words": "Two donors carry topic 7" }),
            ),
            note(4, ev("H9", "s", "our_cells", "agrees", "t")),
            note(5, ev("H1", "s", "cells", "agrees", "t")),
            note(6, ev("H1", "s", "our_cells", "agrees", " ")),
            note(
                7,
                ev(
                    "H1",
                    "donors 1-4",
                    "our_cells",
                    "agrees",
                    "ISG15, MX1 top markers; cocoa 212 genes at FDR < 0.1",
                ),
            ),
            note(
                8,
                ev(
                    "h1",
                    "donors 5-6",
                    "other_donors",
                    "agrees",
                    "5 of 6 donors higher",
                ),
            ),
            note(9, ev("1", "donors 5-6", "other_study", "agrees", "t")),
            note(
                10,
                ev(
                    "H1",
                    "eQTL catalogue",
                    "genetics",
                    "agrees",
                    "IRF7 eQTL in monocytes",
                ),
            ),
            note(
                11,
                ev(
                    "H2",
                    "donors 1-4",
                    "our_cells",
                    "disagrees",
                    "both treated donors above both controls",
                ),
            ),
            note(
                12,
                json!({ "kind": "evidence", "hypothesis": "H1", "source": "x", "class": "our_cells", "verdict": "agrees", "text": "t" }),
            ),
        ],
    );
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
    ] {
        assert_eq!(
            by_id(&msgs, id)["result"]["isError"],
            refused,
            "call {id}: {}",
            said(&msgs, id)
        );
    }
    let text = |id| said(&msgs, id);
    assert!(
        text(1).contains("there is no hypothesis H1 (the hypotheses are none yet"),
        "{}",
        text(1)
    );
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
    assert!(
        text(11).contains("has not said what would nullify H2"),
        "{}",
        text(11)
    );
    assert!(text(12).contains("`words` is required"), "{}", text(12));

    let results = proj.join("results");
    let evidence = std::fs::read_to_string(results.join("EVIDENCE.tsv")).unwrap();
    assert_eq!(
        evidence.lines().count(),
        5,
        "only the evidence taken: {evidence}"
    );
    assert!(
        evidence.contains("\tlegume\tH1\tdonors 5-6\tother_donors\tagrees\t5 of 6 donors higher\tagrees, as I read it\n"),
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
        " · legume · evidence H1\n\nH1 from donors 5-6 (other donors, same study): agrees.\n\n5 of 6 donors higher\n\n> agrees, as I read it\n\n**H1. IFN drives topic 7**\n",
        "| donors 5-6 | other donors, same study | lab, platform | agrees | 5 of 6 donors higher | agrees, as I read it |\n",
    ] {
        assert!(notebook.contains(part), "{part:?} in {notebook}");
    }
    assert!(
        !notebook.contains("Sources not yet used"),
        "the hints are for the agent: {notebook}"
    );
}

#[test]
fn the_notebook_is_the_legume_servers_alone() {
    let base = tempfile::tempdir().unwrap();
    let proj = base.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let init = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
    let list = json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/list" });

    // A tool's server: no notebook of its own.
    let tool: String = [
        init.clone(),
        call(1, "notebook", json!({ "kind": "plan", "words": "w" })),
        list.clone(),
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
    let instructions = by_id(&msgs, 0)["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("The notebook is the `legume` server's")
            && instructions.contains("not the job's id"),
        "{instructions}"
    );
    let tools = by_id(&msgs, 99)["result"]["tools"].as_array().unwrap();
    assert!(!tools.iter().any(|t| t["name"] == "notebook"));
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

    // The legume server: the notebook, the guide, the project's standing.
    let msgs = brain(
        &proj,
        &[
            init,
            call(1, "guide", json!({})),
            call(2, "guide", json!({ "section": "8.3" })),
            call(
                3,
                "notebook",
                json!({ "kind": "hypothesis", "words": "IFN drives topic 7" }),
            ),
            call(
                4,
                "notebook",
                json!({ "kind": "evidence", "hypothesis": "H1", "source": "donors 1-4", "class": "our_cells", "verdict": "agrees", "text": "ISGs top", "words": "clear in all four" }),
            ),
            call(5, "status", json!({})),
            call(6, "status", json!({ "hypothesis": "H1" })),
            call(7, "status", json!({ "hypothesis": "H2" })),
            json!({ "jsonrpc": "2.0", "id": 11, "method": "prompts/list" }),
            json!({ "jsonrpc": "2.0", "id": 12, "method": "prompts/get",
                    "params": { "name": "explore", "arguments": { "question": "why topic 7?" } } }),
            json!({ "jsonrpc": "2.0", "id": 13, "method": "prompts/get",
                    "params": { "name": "triangulate" } }),
            json!({ "jsonrpc": "2.0", "id": 14, "method": "prompts/get", "params": { "name": "nope" } }),
            list,
        ],
    );
    assert_eq!(by_id(&msgs, 0)["result"]["serverInfo"]["name"], "legume");
    let instructions = by_id(&msgs, 0)["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("Quick card for agents:")
            && instructions.contains("The person thinks; you run the tools"),
        "{instructions}"
    );
    let listed = by_id(&msgs, 99)["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = listed.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["guide", "status", "report", "notebook"],
        "no job tools: the legume server runs no steps"
    );
    assert_eq!(listed[0]["annotations"], json!({ "readOnlyHint": true }));
    assert_eq!(
        listed[2]["annotations"],
        json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true })
    );
    assert_eq!(
        listed[3]["annotations"],
        json!({ "readOnlyHint": false, "destructiveHint": false })
    );
    assert_eq!(
        listed[3]["inputSchema"]["properties"]["kind"]["enum"],
        json!([
            "plan",
            "data",
            "observation",
            "hypothesis",
            "evidence",
            "figure",
            "summary",
            "note"
        ])
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
            && explore.contains("Call `status`")
            && explore.contains("Never say what a result means"),
        "{explore}"
    );
    let tri = by_id(&msgs, 13)["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(tri.contains("Hypothesis: not given\n"), "{tri}");
    assert_eq!(by_id(&msgs, 14)["error"]["code"], -32602);
    assert!(
        said(&msgs, 1).contains("8.3        Sources of evidence"),
        "{}",
        said(&msgs, 1)
    );
    assert!(
        said(&msgs, 2).starts_with("8.3 Sources of evidence\n"),
        "{}",
        said(&msgs, 2)
    );
    let status = said(&msgs, 5);
    assert!(status.contains("H1. IFN drives topic 7 (one group of cells, not replicated; sources: 1 agree, 0 disagree, 0 mixed)"), "{status}");
    assert!(
        status.contains("Open for the person (ask, in plain words):\n  H1: what else could explain it?\n  H1: what result would make you drop it?"),
        "{status}"
    );
    assert!(
        said(&msgs, 6).contains(
            "| donors 1-4 | our cells | everything | agrees | ISGs top | clear in all four |"
        ),
        "{}",
        said(&msgs, 6)
    );
    assert!(
        said(&msgs, 6).contains("Sources not yet used for H1"),
        "{}",
        said(&msgs, 6)
    );
    assert_eq!(by_id(&msgs, 7)["result"]["isError"], true);
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
