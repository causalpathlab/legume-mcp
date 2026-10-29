//! `legume chat`'s consent: a step that writes runs when the person lets
//! it, and an answer is recorded as the person confirms it. In a test
//! binary of its own: it puts a tool's server on the PATH.

mod stand;

use legume_mcp::chat::{run_with, Options};
use serde_json::json;
use stand::{calls, says, stand};

fn notebook(project: &std::path::Path) -> String {
    let day = std::fs::read_dir(project.join("results"))
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    std::fs::read_to_string(day.join("notebook.md")).unwrap()
}

#[test]
fn a_step_runs_when_the_person_lets_it_and_consent_is_in_their_words() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    // A tool's server that is not the legume server: the legume binary
    // under another name, on the PATH.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_legume"), bin.join("leg-tool")).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin.clone()];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
    let plan = |text: &str| {
        calls(
            "leg-tool__notebook",
            json!({ "kind": "plan", "text": text }),
        )
    };
    let reading = |c: &str| json!({ "claim": c, "rationale": "r", "check": "c" });
    let s = stand(vec![
        calls("leg-tool__guide", json!({})),
        plan("first"),
        plan("second"),
        plan("third"),
        says("Planned."),
        calls(
            "legume__notebook",
            json!({ "kind": "interpretation", "readings": [reading("A"), reading("B")] }),
        ),
        says("Which hold?"),
        calls(
            "legume__notebook",
            json!({ "kind": "answer", "chose": [1, 2], "words": "yes, both", "by": "user" }),
        ),
        calls(
            "legume__notebook",
            json!({ "kind": "answer", "chose": [1], "words": "the first" }),
        ),
        says("Noted."),
    ]);
    let options = Options {
        url: Some(s.url.clone()),
        model: Some("stand-in".into()),
        project: Some(project.clone()),
        tools: Some(vec!["leg-tool".into(), "../x".into()]),
        small: Some(false),
        legume: Some(env!("CARGO_BIN_EXE_legume").into()),
        ..Default::default()
    };
    // The model reads both readings into the person's line, and they say
    // no; then the first alone, and they say yes.
    let lines = "plan it\nn\na\nwhat now\nonly the first, A\nn\ny\n";
    let mut out = Vec::new();
    run_with(options, std::io::Cursor::new(lines), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("(../x is not a tool's name; skipped)"),
        "{out}"
    );
    // A read-only tool runs; a writing one asks: no, then always.
    assert!(!out.contains("Run leg-tool__guide"), "{out}");
    assert_eq!(out.matches("Run leg-tool__notebook").count(), 2, "{out}");
    let nb = notebook(&project);
    assert!(
        !nb.contains("\n\nfirst\n") && nb.contains("\n\nsecond\n") && nb.contains("\n\nthird\n"),
        "{nb}"
    );
    // The answer is what the person confirmed, in their words, and claims
    // no `by`.
    assert!(
        out.contains("Record that you chose R1. A / R2. B? [y]es, [n]o"),
        "{out}"
    );
    assert!(
        out.contains("Record that you chose R1. A? [y]es, [n]o"),
        "{out}"
    );
    assert!(
        nb.contains("chose R1. Hypotheses: R1 = H1.\n\n> only the first, A\n"),
        "{nb}"
    );
    assert!(!nb.contains("R2 = H") && !nb.contains("yes, both"), "{nb}");
    let asked = s.asked.lock().unwrap();
    let declined = asked[2]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert!(
        declined["content"]
            .as_str()
            .unwrap()
            .starts_with("Not run: the person did not let it"),
        "{declined}"
    );
}
