//! A stand-in for a tool's TUI that a session job runs: `session_job run
//! MODE OUT` shows `job working` on a screen of its own, says a line of
//! log a tick, and once a file `OUT.go` appears ends as MODE says:
//! `write` writes OUT and succeeds, `nowrite` succeeds without writing
//! it, `fail` fails. A stop (`job_cancel`, `session_job kill`) ends it
//! cancelled. Each end is told to legume-mcp's [`Reporter`].
//! `session_job attach|sessions|kill [ID]` are the session handlers.

use legume_mcp::job::{End, Reporter};
use legume_tui::cli::{AttachArgs, KillArgs};
use legume_tui::{Options, Round, Tool};
use ratatui::widgets::Paragraph;
use std::path::PathBuf;
use std::time::Duration;

const TOOL: Tool = Tool::new("sj");

fn main() -> std::io::Result<()> {
    legume_tui::serve_if_host();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = args.get(1).cloned();
    match args.first().map(String::as_str) {
        Some("attach") => legume_tui::cli::attach(&TOOL, &AttachArgs { name }),
        Some("sessions") => legume_tui::cli::sessions(&TOOL),
        Some("kill") => legume_tui::cli::kill(&TOOL, &KillArgs { name, force: false }),
        Some("run") => run(&args[1], PathBuf::from(&args[2])),
        _ => Err(std::io::Error::other("session_job run MODE OUT")),
    }
}

fn run(mode: &str, out: PathBuf) -> std::io::Result<()> {
    let reporter = Reporter::from_env();
    let session = TOOL.session("job", &Options::default())?;
    let mut terminal = session.terminal()?;
    let go = PathBuf::from(format!("{}.go", out.display()));
    let mut step = 0;
    let end = loop {
        session.draw(&mut terminal, |f| {
            f.render_widget(Paragraph::new(format!("job working {step}")), f.area())
        })?;
        if session.poll(&mut terminal, Duration::from_millis(50))? == Round::Stopped {
            break End::Cancelled;
        }
        step += 1;
        if let Some(r) = &reporter {
            r.line(&format!("step {step}"));
        }
        if go.exists() {
            break match mode {
                "write" => {
                    std::fs::write(&out, "fitted")?;
                    End::Done("fitted".into())
                }
                "nowrite" => End::Done("fitted".into()),
                _ => End::Failed("the fit diverged".into()),
            };
        }
    };
    session.restore();
    if let Some(r) = reporter {
        r.finish(end);
    }
    Ok(())
}
