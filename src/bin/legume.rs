//! `legume`: a project connected to the agents that run the legume tools
//! (`legume init`), and the server they share (`legume mcp`, which the
//! agents start). Everything else is the server's: the person asks the
//! agent.

const USAGE: &str = "\
usage: legume init [...]   connect this project to the agents: Claude Code, Codex, Cursor,
                           VS Code, Gemini CLI, Antigravity, opencode (legume init --help)

Then start the agent in the project (`claude`, `codex`, `agent`, ...) and
ask: where are we? AGENTS.md tells it how to work with you. The
agent reaches the rest through the `legume` server, which it starts itself
(`legume mcp`).";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") if args.len() == 1 => legume_mcp::brain::serve(),
        Some("init") => legume_mcp::init::main(&args[1..], &mut std::io::stdout()),
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
