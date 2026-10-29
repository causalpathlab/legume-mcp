//! Session jobs: a tool starts a long step in a detached session of its own
//! TUI; a person attaches to it as to any session; the agent follows it
//! with job_status / job_wait / job_cancel as any job; it outlives its
//! server, the next server takes it up, and it is recorded only when it
//! succeeds.

#![cfg(unix)]

use anyhow::Result;
use legume_mcp::{serve_with, Call, Content, Handler, Options, ServerInfo, SessionJob, ToolSpec};
use legume_tui::testing::Private;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Lines, PipeReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const TOOL: legume_tui::Tool = legume_tui::Tool::new("sj");

/// The stand-in TUI, built alongside the tests.
fn stand_in() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().unwrap().parent().unwrap();
    let it = debug.join("examples").join("session_job");
    assert!(it.exists(), "build the example first: {}", it.display());
    it
}

struct Tools;

impl Handler for Tools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::new("fit", "fit", json!({ "type": "object" }))]
    }

    fn call(&mut self, _: &str, args: Value, cx: &Call) -> Result<Vec<Content>> {
        let out = PathBuf::from(args["out"].as_str().unwrap());
        let mode = args["mode"].as_str().unwrap_or("write");
        let mut watch = Command::new(stand_in());
        watch.arg("run").arg(mode).arg(&out);
        let mut record = Command::new("fitter");
        record.arg(&out);
        let started = cx.start_session_job(SessionJob {
            tool: TOOL,
            screen: "fit".into(),
            label: args["label"].as_str().map(String::from),
            watch,
            record,
            outputs: vec![out],
            summary: "a model fit".into(),
        })?;
        Ok(vec![Content::Json(
            json!({ "job": started.job, "session": started.session }),
        )])
    }
}

/// A server on `project`, its feed under `base`, and the agent's end of it.
struct Server {
    send: Option<Box<dyn FnMut(Value) + Send>>,
    replies: Lines<BufReader<PipeReader>>,
    id: u64,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(project: &Path, base: &Path) -> Self {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let (out_r, out_w) = std::io::pipe().unwrap();
        let options = Options {
            project: Some(project.to_path_buf()),
            watch: Some(base.to_path_buf()),
        };
        let info = ServerInfo {
            name: "t".into(),
            version: "0".into(),
            instructions: None,
        };
        let thread = std::thread::spawn(move || {
            serve_with(BufReader::new(reader), out_w, info, options, Tools).unwrap();
        });
        let mut s = Server {
            send: Some(Box::new(move |m| writeln!(writer, "{m}").unwrap())),
            replies: BufReader::new(out_r).lines(),
            id: 0,
            thread: Some(thread),
        };
        s.send(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} }));
        let init: Value = serde_json::from_str(&s.replies.next().unwrap().unwrap()).unwrap();
        let note = init["result"]["instructions"].as_str().unwrap();
        assert!(note.contains("attach"), "{note}");
        s
    }

    fn send(&mut self, m: Value) {
        (self.send.as_mut().unwrap())(m);
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                          "params": { "name": name, "arguments": args } }));
        let reply: Value = serde_json::from_str(&self.replies.next().unwrap().unwrap()).unwrap();
        reply["result"]["structuredContent"].clone()
    }

    /// The server ends as it does when its client goes.
    fn end(mut self) {
        self.send = None;
        self.thread.take().unwrap().join().unwrap();
    }
}

fn wait(s: &mut Server, job: &str) -> Value {
    let start = Instant::now();
    loop {
        let w = s.call("job_wait", json!({ "id": job, "seconds": 5 }));
        if w["state"] != "running" {
            return w;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{job} never ended"
        );
    }
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(15), "never {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn go(out: &Path) {
    std::fs::write(format!("{}.go", out.display()), "").unwrap();
}

fn index(project: &Path) -> String {
    std::fs::read_to_string(project.join("results/INDEX.tsv")).unwrap_or_default()
}

fn script(project: &Path) -> String {
    let Ok(days) = std::fs::read_dir(project.join("results")) else {
        return String::new();
    };
    days.flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| std::fs::read_to_string(e.path().join("runall.sh")).unwrap_or_default())
        .collect()
}

#[test]
fn a_session_job_is_watched_followed_cancelled_and_outlives_its_server() {
    let sessions = Private::new(stand_in());
    // The server and the jobs it starts keep their sessions there.
    std::env::set_var(legume_tui::DIR_VAR, sessions.path());
    let tmp = tempfile::tempdir().unwrap();
    let (base, project) = (tmp.path().join("w"), tmp.path().join("p"));
    std::fs::create_dir_all(&project).unwrap();
    let mut s = Server::start(&project, &base);

    // It returns at once, running in a session of its own.
    let made = project.join("model.bin");
    let started = Instant::now();
    let r = s.call("fit", json!({ "out": made, "label": "model" }));
    assert!(started.elapsed() < Duration::from_secs(3), "{r}");
    let (job, session) = (r["job"].as_str().unwrap(), r["session"].as_str().unwrap());
    assert_eq!((job, session), ("job-1", "sj-fit-model"), "{r}");
    let st = s.call("job_status", json!({ "id": job }));
    assert_eq!(st["state"], "running", "{st}");
    assert_eq!(st["session"], session, "{st}");
    assert_eq!(st["attach"], "sj attach sj-fit-model", "{st}");

    // A person attaches and sees its screen; detached again, it runs on.
    let mut t = sessions.term(&["attach", session], &[]);
    t.wait_screen("job working");
    t.type_(b"\x1b\x04");
    assert_eq!(t.exit_code(), 0);

    // Its log reaches job_status; its output is held while it runs.
    until("logged", || {
        let st = s.call("job_status", json!({ "id": job }));
        st["last_lines"].to_string().contains("step")
    });
    let refused = s.call("fit", json!({ "out": made }));
    assert!(refused.is_null(), "refused, as an error: {refused}");

    // It ends done, with its output, recorded as a step.
    go(&made);
    let w = wait(&mut s, job);
    assert_eq!(w["state"], "done", "{w}");
    assert_eq!(w["output"], "fitted", "{w}");
    assert!(made.exists());
    until("gone", || TOOL.find(session).unwrap().is_none());
    assert!(script(&project).contains("fitter"), "{}", script(&project));
    assert!(
        index(&project).contains("model.bin\ta model fit")
            && index(&project).contains(r#"[{"name":"steps","value":"#),
        "{}",
        index(&project)
    );

    // A label no session can have is made one it can.
    let spaced = project.join("spaced.bin");
    let r = s.call("fit", json!({ "out": spaced, "label": "my model" }));
    assert_eq!(r["session"], "sj-fit-my-model", "{r}");
    go(&spaced);
    assert_eq!(wait(&mut s, r["job"].as_str().unwrap())["state"], "done");

    // Cancelled: its session ends and it is not recorded.
    let cut = project.join("cut.bin");
    let r = s.call("fit", json!({ "out": cut }));
    let (job, session) = (
        r["job"].as_str().unwrap().to_string(),
        r["session"].as_str().unwrap().to_string(),
    );
    let c = s.call("job_cancel", json!({ "id": job }));
    assert_eq!(c["stopping"], true, "{c}");
    let w = wait(&mut s, &job);
    assert_eq!(w["state"], "cancelled", "{w}");
    until("gone", || TOOL.find(&session).unwrap().is_none());
    assert!(!index(&project).contains("cut.bin"), "{}", index(&project));

    // Failed, or done without its output: not recorded.
    for (mode, why) in [
        ("fail", "the fit diverged"),
        ("nowrite", "finished without writing"),
    ] {
        let out = project.join(format!("{mode}.bin"));
        let r = s.call("fit", json!({ "out": out, "mode": mode }));
        go(&out);
        let w = wait(&mut s, r["job"].as_str().unwrap());
        assert_eq!(w["state"], "failed", "{w}");
        assert!(w["error"].as_str().unwrap().contains(why), "{w}");
        assert!(
            !index(&project).contains(&format!("{mode}.bin")),
            "{}",
            index(&project)
        );
    }

    // Killed without a word: failed, and recorded as a step that did not
    // succeed.
    let killed = project.join("killed.bin");
    let r = s.call("fit", json!({ "out": killed }));
    let session = r["session"].as_str().unwrap().to_string();
    until("logged", || {
        let st = s.call("job_status", json!({ "id": r["job"] }));
        st["last_lines"].to_string().contains("step")
    });
    assert!(TOOL.stop(&session, true, Duration::from_secs(10)).unwrap());
    let w = wait(&mut s, r["job"].as_str().unwrap());
    assert_eq!(w["state"], "failed", "{w}");
    assert!(
        w["error"]
            .as_str()
            .unwrap()
            .contains("ended without saying how"),
        "{w}"
    );
    let said = script(&project);
    let commented = "ended without saying how):\n#   if [ ! -e killed.bin ]";
    assert!(said.contains(commented), "{said}");

    // The server ends; its session jobs run on.
    let late = project.join("late.bin");
    let early = project.join("early.bin");
    let r = s.call("fit", json!({ "out": late }));
    let late_session = r["session"].as_str().unwrap().to_string();
    let r = s.call("fit", json!({ "out": early }));
    let early_session = r["session"].as_str().unwrap().to_string();
    s.end();
    assert!(TOOL.find(&late_session).unwrap().is_some());
    assert!(TOOL.find(&early_session).unwrap().is_some());

    // One ends with no server: it records itself.
    go(&early);
    until("gone", || TOOL.find(&early_session).unwrap().is_none());
    until("recorded", || index(&project).contains("early.bin"));

    // The next server takes both up: the one done, the one still running,
    // which it follows to its end.
    let took = Instant::now();
    let mut s = Server::start(&project, &base);
    let all = s.call("job_status", json!({}));
    assert!(
        took.elapsed() < Duration::from_secs(1),
        "a job ended already is taken up at once: {:?}",
        took.elapsed()
    );
    let jobs = all["jobs"].as_array().unwrap();
    let of = |session: &str| {
        jobs.iter()
            .find(|j| j["session"] == session)
            .unwrap_or_else(|| panic!("{session} not taken up: {all}"))
            .clone()
    };
    assert_eq!(of(&early_session)["state"], "done", "{all}");
    let late_job = of(&late_session);
    assert_eq!(late_job["state"], "running", "{all}");
    let refused = s.call("fit", json!({ "out": late }));
    assert!(
        refused.is_null(),
        "still held by the job taken up: {refused}"
    );
    go(&late);
    let w = wait(&mut s, late_job["id"].as_str().unwrap());
    assert_eq!(w["state"], "done", "{w}");
    assert_eq!(
        index(&project).matches("late.bin").count(),
        1,
        "{}",
        index(&project)
    );
    s.end();
    assert!(sessions.sockets().is_empty(), "{:?}", sessions.sockets());
}
