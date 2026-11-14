//! `legume`: a project connected to the agents that run the legume tools
//! (`legume init`), and the server they share (`legume mcp`, which the
//! agents start). Everything else is the server's: the person asks the
//! agent. Beside them, a benchmark's scoring: `legume score` and `legume
//! universe` (built with `benchmark`).

use legume_mcp::command;
use std::io::Write;

const USAGE: &str = "\
usage: legume init [...]   connect this project to the agents: Claude Code, Codex, Cursor,
                           VS Code, Gemini CLI, Antigravity, opencode (legume init --help)

Then start the agent in the project (`claude`, `codex`, `agent`, ...) and
ask: where are we? AGENTS.md tells it how to work with you. The
agent reaches the rest through the `legume` server, which it starts itself
(`legume mcp`).

legume score [...]         a method's scores against a truth over a universe:
                           AUROC and average precision (legume score --help)
legume universe cis [...]  every gene's cis candidate peaks, a peak-to-gene
                           universe (legume universe --help)
legume --version           which legume-mcp this is";

/// A command run on its arguments (those after its name), saying what it
/// did on the first writer and its warnings on the second.
type Main = fn(&[String], &mut dyn Write, &mut dyn Write) -> anyhow::Result<()>;

/// Each command: its name, what runs it, and the usage said after a
/// command line it cannot run.
const COMMANDS: &[(&str, Main, &str)] = &[
    (
        "init",
        |args, out, _| legume_mcp::init::main(args, out),
        legume_mcp::init::USAGE,
    ),
    benchmark::SCORE,
    benchmark::UNIVERSE,
];

/// `legume score` and `legume universe`.
#[cfg(feature = "benchmark")]
mod benchmark {
    use super::Main;

    pub const SCORE: (&str, Main, &str) = (
        "score",
        |args, out, _| legume_mcp::score::main(args, out),
        legume_mcp::score::USAGE,
    );
    pub const UNIVERSE: (&str, Main, &str) = (
        "universe",
        |args, out, _| legume_mcp::universe::main(args, out),
        legume_mcp::universe::USAGE,
    );
}

/// `legume score` and `legume universe` in a build without `benchmark`:
/// they say how to install the one that has them, and exit 2.
#[cfg(not(feature = "benchmark"))]
mod benchmark {
    use super::Main;
    use std::io::Write;

    const WITHOUT_BENCHMARK: &str =
        "this legume was built without benchmark: cargo install legume-mcp --features pdf,benchmark";

    fn without_benchmark(
        _: &[String],
        _: &mut dyn Write,
        err: &mut dyn Write,
    ) -> anyhow::Result<()> {
        // Nothing is left to do when even stderr is gone.
        let _ = writeln!(err, "{WITHOUT_BENCHMARK}");
        std::process::exit(2);
    }

    pub const SCORE: (&str, Main, &str) = ("score", without_benchmark, "");
    pub const UNIVERSE: (&str, Main, &str) = ("universe", without_benchmark, "");
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);
    match first {
        Some("mcp") if args.len() == 1 => return legume_mcp::brain::serve(),
        Some("-V" | "--version" | "version") => {
            println!("legume-mcp {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => {}
    }
    let Some(&(name, run, usage)) = COMMANDS.iter().find(|c| Some(c.0) == first) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    if let Err(e) = run(&args[1..], &mut std::io::stdout(), &mut std::io::stderr()) {
        // A refusal's own exit code: 2 for a command line, said with the
        // command's usage, 3 for a truth or score outside the universe;
        // 1 for anything else.
        let code = command::code(&e);
        eprintln!("legume {name}: {e:#}");
        if code == 2 {
            eprintln!("\n{usage}");
        }
        std::process::exit(code);
    }
    Ok(())
}
