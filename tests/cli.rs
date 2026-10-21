//! A tool's command line as an agent sets it: flags listed from clap, and a
//! JSON map of flags turned into the words clap reads.

use clap::{Arg, ArgAction, Command};
use legume_mcp::cli::{check, describe, find, flag_name, is_switch, settable, to_argv};
use serde_json::{json, Map, Value};

fn tool() -> Command {
    let mut cmd = Command::new("tool")
        .arg(Arg::new("data").num_args(1..))
        .arg(Arg::new("output").long("output").short('o'))
        .arg(
            Arg::new("window")
                .long("cis-window")
                .default_value("500")
                .help("Window"),
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("device")
                .long("device")
                .value_parser(["cpu", "cuda"])
                .default_value("cpu"),
        )
        .arg(Arg::new("genes").long("genes").value_delimiter(','))
        .arg(Arg::new("k").short('k').action(ArgAction::Append))
        .arg(Arg::new("range").long("range").num_args(2))
        .arg(Arg::new("one").long("one"))
        .arg(Arg::new("loud").short('v').action(ArgAction::Count))
        .arg(
            Arg::new("width")
                .long("width")
                .visible_alias("wide")
                .alias("w-old"),
        )
        .arg(Arg::new("all").long("help-all").action(ArgAction::HelpLong))
        .arg(Arg::new("secret").long("secret").hide(true))
        .arg(
            Arg::new("mad")
                .long("mad")
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("true")
                .default_value("true"),
        );
    cmd.build();
    cmd
}

/// The tool's own arguments: the output prefix and the data files.
const OWN: &[(&str, &str)] = &[("output", "out"), ("data", "data")];

fn argv(flags: Value) -> anyhow::Result<Vec<String>> {
    let flags: Map<String, Value> = serde_json::from_value(flags).unwrap();
    to_argv(&tool(), &flags, OWN, "tool_flags")
}

#[test]
fn every_argument_but_help_is_settable() {
    let cmd = tool();
    let ids: Vec<&str> = settable(&cmd).map(|a| a.get_id().as_str()).collect();
    assert!(!ids.contains(&"help") && !ids.contains(&"all"), "{ids:?}");
    for id in ["data", "output", "window", "secret", "mad"] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
}

#[test]
fn flags_are_named_and_described_as_the_command_line_has_them() {
    let cmd = tool();
    let arg = |id: &str| settable(&cmd).find(|a| a.get_id() == id).unwrap();
    assert_eq!(flag_name(arg("window")), "--cis-window");
    assert_eq!(flag_name(arg("k")), "-k");
    assert_eq!(flag_name(arg("data")), "data");
    assert!(is_switch(arg("verbose")));
    assert!(
        !is_switch(arg("mad")),
        "a value it may omit is still a value"
    );

    assert_eq!(
        describe(arg("window")),
        json!({ "flag": "--cis-window", "help": "Window", "default": "500" })
    );
    assert_eq!(
        describe(arg("verbose")),
        json!({ "flag": "--verbose", "help": null, "switch": true })
    );
    assert_eq!(describe(arg("device"))["choices"], json!(["cpu", "cuda"]));
    assert_eq!(describe(arg("data"))["positional"], true);
}

#[test]
fn a_flag_is_found_by_any_of_its_spellings() {
    let cmd = tool();
    for (key, id) in [
        ("cis-window", "window"),
        ("--cis-window", "window"),
        ("cis_window", "window"),
        ("window", "window"),
        ("k", "k"),
        ("-k", "k"),
        ("wide", "width"),
        ("w-old", "width"),
    ] {
        assert_eq!(
            find(&cmd, key).map(|a| a.get_id().as_str()),
            Some(id),
            "{key}"
        );
    }
    assert!(find(&cmd, "nope").is_none());
    assert!(find(&cmd, "help").is_none());
}

#[test]
fn flags_become_words_clap_reads() {
    let words = argv(json!({
        "cis-window": 100,
        "verbose": true,
        "genes": ["a", "b"],
        "k": [-1, 2],
        "mad": false,
        "device": null,
        "secret": "s",
    }))
    .unwrap();
    for w in [
        "--cis-window=100",
        "--verbose",
        "--genes=a,b",
        "-k=-1",
        "-k=2",
        "--mad=false",
        "--secret=s",
    ] {
        assert!(words.contains(&w.to_string()), "{w} missing from {words:?}");
    }
    assert!(!words.iter().any(|w| w.contains("device")), "{words:?}");
    assert!(argv(json!({ "verbose": false })).unwrap().is_empty());

    // Several values for one flag, as clap takes them; a count as a number.
    let more = argv(json!({ "range": [1, 10], "loud": 3 })).unwrap();
    assert_eq!(more, ["-v", "-v", "-v", "--range", "1", "10"]);
    let m = check(&tool(), &more).unwrap();
    let range: Vec<&String> = m.get_many("range").unwrap().collect();
    assert_eq!(range, ["1", "10"]);
    assert_eq!(m.get_count("loud"), 3);

    let m = check(&tool(), &words).unwrap();
    assert_eq!(m.get_one::<String>("window").unwrap(), "100");
    let ks: Vec<&String> = m.get_many("k").unwrap().collect();
    assert_eq!(ks, ["-1", "2"]);
}

#[test]
fn a_flag_that_cannot_be_set_says_why() {
    let err = |flags: Value| format!("{}", argv(flags).unwrap_err());

    let e = err(json!({ "verbose": "yes" }));
    assert!(e.contains("--verbose") && e.contains("switch"), "{e}");

    for key in ["output", "--output", "o", "-o"] {
        let e = err(json!({ key: "x" }));
        assert!(e.contains("`out`"), "{key}: {e}");
    }
    let e = err(json!({ "data": "f.zarr" }));
    assert!(e.contains("`data`"), "{e}");

    // A positional the tool did not name is still not a flag.
    let flags: Map<String, Value> = serde_json::from_value(json!({ "data": "f" })).unwrap();
    let e = format!(
        "{}",
        to_argv(&tool(), &flags, &OWN[..1], "tool_flags").unwrap_err()
    );
    assert!(e.contains("positional"), "{e}");

    let e = err(json!({ "range": [-1, 10] }));
    assert!(e.contains("--range") && e.contains("-1"), "{e}");

    let e = err(json!({ "loud": "lots" }));
    assert!(e.contains("-v") && e.contains("number"), "{e}");

    let e = err(json!({ "one": ["a", "b"] }));
    assert!(e.contains("--one") && e.contains("one value"), "{e}");

    let e = err(json!({ "cis-window": 1, "window": 2 }));
    assert!(
        e.contains("cis-window") && e.contains("window") && e.contains("same"),
        "{e}"
    );

    let e = err(json!({ "nope": 1 }));
    assert!(e.contains("--nope") && e.contains("tool_flags"), "{e}");
}

#[test]
fn a_command_line_clap_refuses_comes_back_as_its_error() {
    let words = vec!["--device=gpu".to_string()];
    let e = format!("{}", check(&tool(), &words).unwrap_err());
    assert!(e.contains("gpu") && e.contains("--device"), "{e}");
}
