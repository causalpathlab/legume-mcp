//! `legume`: a project connected to the agents that run the legume tools
//! (`legume init`), and the server they share (`legume mcp`, which the
//! agents start). Everything else is the server's: the person asks the
//! agent.

const USAGE: &str = "\
usage: legume init [...]   connect this project to Claude Code and Cursor (legume init --help)

Then start `claude` or `agent` in the project and ask: where are we? The
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
