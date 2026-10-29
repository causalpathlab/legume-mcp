//! `legume`: the server the legume tools share (`legume mcp`), its guide
//! on the command line (`legume guide [section]`), and, with feature
//! `chat`, the tools with a local model (`legume chat`, set up for a
//! project by `legume init`); with feature `screen`, the chat on a screen
//! of its own, in a session to leave and come back to (`legume attach`).

const USAGE: &str = "\
usage: legume mcp              serve the legume MCP server on stdin and stdout
       legume guide [SECTION]  the guide to working with the legume tools, or one section
       legume init [...]       set up this project for the chat (legume init --help)
       legume chat [...]       the tools with a model on this machine (legume chat --help)
       legume report [...]     the day's work as slides, from the records (legume report --help)
       legume graph [...]      the provenance graph, from results/INDEX.tsv (legume graph --help)
       legume attach [NAME]    return to a chat left running
       legume sessions         the chats left running";

fn main() -> anyhow::Result<()> {
    // A chat's session host, started again as this program, serves it.
    #[cfg(feature = "screen")]
    legume_tui::serve_if_host();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") if args.len() == 1 => legume_mcp::brain::serve(),
        Some("guide") if args.len() <= 2 => {
            print!(
                "{}",
                legume_mcp::brain::guide(args.get(1).map(String::as_str))?
            );
            Ok(())
        }
        Some(c @ ("report" | "graph")) => {
            legume_mcp::report::main(c, &args[1..], &mut std::io::stdout())
        }
        Some("chat") => chat(&args[1..]),
        Some("init") => init(&args[1..]),
        Some("attach") if args.len() <= 2 => sessions(args.get(1).cloned(), true),
        Some("sessions") if args.len() == 1 => sessions(None, false),
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

#[cfg(feature = "chat")]
fn chat(args: &[String]) -> anyhow::Result<()> {
    use legume_mcp::chat;
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", chat::USAGE);
        return Ok(());
    }
    let options = chat::options(args)?;
    #[cfg(feature = "screen")]
    {
        use std::io::IsTerminal;
        if !options.plain && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            let no_detach = options.no_detach;
            return chat::screen::run(options, no_detach);
        }
    }
    chat::run(options)
}

/// `legume attach [NAME]` (`attach`), or `legume sessions`.
#[cfg(feature = "screen")]
fn sessions(name: Option<String>, attach: bool) -> anyhow::Result<()> {
    use legume_mcp::chat::screen::TOOL;
    if attach {
        legume_tui::cli::attach(&TOOL, &legume_tui::cli::AttachArgs { name })?;
    } else {
        legume_tui::cli::sessions(&TOOL)?;
    }
    Ok(())
}

#[cfg(not(feature = "screen"))]
fn sessions(_: Option<String>, _: bool) -> anyhow::Result<()> {
    anyhow::bail!(
        "this legume was built without the chat's screen: cargo install legume-mcp --features screen"
    )
}

#[cfg(feature = "chat")]
fn init(args: &[String]) -> anyhow::Result<()> {
    use legume_mcp::chat::config;
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", config::USAGE);
        return Ok(());
    }
    config::init(args, &mut std::io::stdout())
}

#[cfg(not(feature = "chat"))]
fn init(args: &[String]) -> anyhow::Result<()> {
    chat(args)
}

#[cfg(not(feature = "chat"))]
fn chat(_: &[String]) -> anyhow::Result<()> {
    anyhow::bail!(
        "this legume was built without the chat: cargo install legume-mcp --features chat"
    )
}
