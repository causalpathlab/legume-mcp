//! `legume`: a project connected to the agents that run the legume tools
//! (`legume init`), and the server they share (`legume mcp`, which the
//! agents start). Everything else is the server's: the person asks the
//! agent. Beside them, a benchmark's scoring: `legume score` and `legume
//! universe` (built with `benchmark`).

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

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") if args.len() == 1 => legume_mcp::brain::serve(),
        Some("init") => legume_mcp::init::main(&args[1..], &mut std::io::stdout()),
        Some(command @ ("score" | "universe")) => benchmark(command, &args[1..]),
        Some("-V" | "--version" | "version") => {
            println!("legume-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// `legume score` or `legume universe`, ending with the refusal's own exit
/// code (2 for a command line, 3 for a truth or score outside the universe,
/// 1 for anything else).
#[cfg(feature = "benchmark")]
fn benchmark(command: &str, args: &[String]) -> anyhow::Result<()> {
    let run = match command {
        "score" => legume_mcp::score::main,
        _ => legume_mcp::universe::main,
    };
    if let Err(e) = run(args, &mut std::io::stdout()) {
        let code = e
            .downcast_ref::<legume_mcp::score::Exit>()
            .map_or(1, |x| x.code);
        eprintln!("legume {command}: {e:#}");
        std::process::exit(code);
    }
    Ok(())
}

#[cfg(not(feature = "benchmark"))]
fn benchmark(_: &str, _: &[String]) -> anyhow::Result<()> {
    std::process::exit(legume_mcp::without_benchmark(&mut std::io::stderr()));
}
