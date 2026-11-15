//! `legume`, run as the binary: each command's refusals and the exit codes
//! they end with, and what a build without `benchmark` says for `legume
//! benchmark`, `legume score` and `legume universe`.

use std::process::{Command, Output};

/// `legume` with `args`, run in a folder of its own, its output captured.
fn legume(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_legume"))
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// A command line that cannot run exits 2 with the refusal, then the
// command's own usage, once.
#[test]
fn a_command_line_init_cannot_run_exits_2_with_its_usage() {
    let o = legume(&["init", "--bogus"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(
        stderr(&o),
        format!(
            "legume init: unknown argument --bogus\n\n{}\n",
            legume_mcp::init::USAGE
        )
    );
}

#[cfg(feature = "benchmark")]
#[test]
fn a_command_line_benchmark_score_or_universe_cannot_run_exits_2_with_its_usage() {
    for (args, usage) in [
        (
            &["benchmark", "plan", "--bogus"][..],
            legume_mcp::benchmark::USAGE,
        ),
        (&["score", "--bogus", "1"], legume_mcp::score::USAGE),
        (
            &["universe", "cis", "--bogus", "1"],
            legume_mcp::universe::USAGE,
        ),
    ] {
        let o = legume(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert_eq!(
            stderr(&o),
            format!("legume {}: unknown argument --bogus\n\n{usage}\n", args[0]),
            "{args:?}"
        );
    }
}

#[cfg(not(feature = "benchmark"))]
#[test]
fn a_build_without_benchmark_prints_the_install_line_and_exits_2() {
    for args in [
        &["score"][..],
        &["score", "--help"],
        &["universe", "cis"],
        &["benchmark", "plan", "S1.json"],
    ] {
        let o = legume(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&o),
            "this legume was built without benchmark: \
             cargo install legume-mcp --features pdf,benchmark\n",
            "{args:?}"
        );
        assert!(o.stdout.is_empty(), "{args:?}");
    }
}
