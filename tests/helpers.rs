//! The helpers a tool builds its calls from: its arguments, the paths and
//! command lines it shows, and a free name for a run's outputs.

use anyhow::Result;
use legume_mcp::{command_line, parse_args, serve_in, shown_path, Call, Content, Handler};
use legume_mcp::{ServerInfo, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct PickArgs {
    #[serde(default)]
    skip: Vec<String>,
    #[serde(default)]
    all_taken: bool,
}

/// `pick`: a free `fit` prefix whose run writes `{out}.a.bin`; `hold`: a
/// job writing `path` for a few seconds.
struct Tools;

impl Handler for Tools {
    fn tools(&self) -> Vec<ToolSpec> {
        ["pick", "hold"]
            .map(|n| ToolSpec::new(n, n, json!({ "type": "object" })))
            .into()
    }

    fn call(&mut self, name: &str, args: Value, cx: &Call) -> Result<Vec<Content>> {
        match name {
            "pick" => {
                let a: PickArgs = parse_args(args)?;
                let plan = |out: &str| Ok(vec![PathBuf::from(format!("{out}.a.bin"))]);
                let taken = |out: &str| {
                    a.all_taken || a.skip.iter().any(|s| out.ends_with(&format!("/{s}")))
                };
                let (out, outputs) = cx.free_prefix("fit", plan, taken)?;
                Ok(vec![Content::Json(
                    json!({ "out": out, "outputs": outputs }),
                )])
            }
            "hold" => {
                let path = PathBuf::from(args["path"].as_str().unwrap());
                let mut cmd = std::process::Command::new("sleep");
                cmd.arg("5");
                let job = cx.spawn_job(cmd, &[path], "held")?;
                Ok(vec![Content::Json(json!({ "job": job }))])
            }
            _ => unreachable!(),
        }
    }
}

fn call(id: u64, name: &str, args: Value) -> String {
    let msg = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                      "params": { "name": name, "arguments": args } });
    format!("{msg}\n")
}

/// The replies of a session of `lines`, by id, keeping records at `project`.
fn session(project: Option<PathBuf>, lines: &[String]) -> Vec<Value> {
    let sink = Sink::default();
    let info = ServerInfo {
        name: "t".into(),
        version: "0".into(),
        instructions: None,
    };
    serve_in(
        Cursor::new(lines.concat()),
        sink.clone(),
        info,
        project,
        Tools,
    )
    .unwrap();
    let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let mut replies: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    replies.sort_by_key(|r| r["id"].as_u64());
    replies
}

fn out_of(r: &Value) -> String {
    r["result"]["structuredContent"]["out"]
        .as_str()
        .unwrap_or_else(|| panic!("{r}"))
        .to_string()
}

#[test]
fn a_free_prefix_skips_names_written_held_or_taken() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().to_path_buf();
    let first = session(Some(project.clone()), &[call(1, "pick", json!({}))]);
    let fit = out_of(&first[0]);
    assert!(fit.ends_with("/fit"), "{fit}");
    assert!(PathBuf::from(&fit).is_absolute());

    // Written: fit.a.bin exists, so fit-2. Held: a job writes fit-2.a.bin,
    // so fit-3. Taken: the tool counts fit-3 out itself, so fit-4.
    std::fs::write(format!("{fit}.a.bin"), "").unwrap();
    let r = session(
        Some(project),
        &[
            call(1, "pick", json!({})),
            call(2, "hold", json!({ "path": format!("{fit}-2.a.bin") })),
            call(3, "pick", json!({})),
            call(4, "pick", json!({ "skip": ["fit-3"] })),
        ],
    );
    assert_eq!(out_of(&r[0]), format!("{fit}-2"));
    assert_eq!(out_of(&r[2]), format!("{fit}-3"));
    assert_eq!(out_of(&r[3]), format!("{fit}-4"));
    assert_eq!(
        r[3]["result"]["structuredContent"]["outputs"],
        json!([format!("{fit}-4.a.bin")])
    );
}

#[test]
fn a_free_prefix_gives_up_when_every_name_is_taken() {
    let tmp = tempfile::tempdir().unwrap();
    let r = session(
        Some(tmp.path().to_path_buf()),
        &[call(1, "pick", json!({ "all_taken": true }))],
    );
    assert_eq!(r[0]["result"]["isError"], true, "{}", r[0]);
    let text = r[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("all taken"), "{text}");
}

#[test]
fn without_records_a_free_prefix_asks_for_out() {
    let r = session(None, &[call(1, "pick", json!({}))]);
    assert_eq!(r[0]["result"]["isError"], true, "{}", r[0]);
    let text = r[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("keeps no records"), "{text}");
}

#[test]
fn arguments_name_what_is_wrong_with_them() {
    let err = parse_args::<PickArgs>(json!({ "skp": [] })).unwrap_err();
    let text = format!("{err}");
    assert!(
        text.starts_with("arguments: ") && text.contains("skp"),
        "{text}"
    );
    let ok: PickArgs = parse_args(json!({ "skip": ["a"] })).unwrap();
    assert_eq!(ok.skip, ["a"]);
}

#[test]
fn paths_are_shown_from_the_root() {
    let shown = shown_path(std::path::Path::new("a/b.tsv"));
    assert!(std::path::Path::new(&shown).is_absolute(), "{shown}");
    assert!(shown.ends_with("a/b.tsv"), "{shown}");
}

#[test]
fn a_command_line_pastes_into_a_shell() {
    let argv = ["tool", "fit", "--out", "my run/x", "--note=it's", "--k=-1"];
    assert_eq!(
        command_line(&argv),
        r"tool fit --out 'my run/x' '--note=it'\''s' --k=-1"
    );
}
