//! `legume init`: a project connected to the agents that run the legume
//! tools. It lists the `legume` server and each legume tool on the PATH
//! that serves MCP (`<tool> mcp`) in each agent's MCP settings in the
//! project (Claude Code, Codex, Cursor, VS Code, Gemini CLI, Antigravity,
//! opencode), and writes the instructions they all read: a section of
//! `AGENTS.md` (Claude Code's `CLAUDE.md` imports it). The agent started
//! in that folder then has them all. Servers already listed are kept, the
//! rest of each file is the user's; nothing is installed.

use crate::command::usage;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The legume tools, as they are named on the PATH.
pub const TOOLS: &[&str] = &[
    "data-beans",
    "senna",
    "lupin",
    "pinto",
    "cocoa",
    "chickpea",
    "faba",
    "mung",
];

/// How an agent lists MCP servers in its settings.
#[derive(Clone, Copy, PartialEq)]
enum Format {
    /// JSON `mcpServers`, each `{command, args}`.
    McpServers,
    /// The same, each with `"type": "stdio"`.
    Stdio,
    /// JSON `servers`, each `{type: stdio, command, args}` (VS Code).
    Servers,
    /// JSON `mcp`, each `{type: local, command: [..], enabled}` (opencode).
    Local,
    /// JSON `mcpServers`, with `AGENTS.md` among its context files
    /// (Gemini CLI reads only `GEMINI.md` unless told).
    Gemini,
    /// TOML `[mcp_servers.<name>]` tables (Codex).
    Toml,
}

/// An agent whose MCP settings a project can hold.
struct Agent {
    name: &'static str,
    /// Its `legume init` flag, to write only the agents named.
    flag: &'static str,
    /// Its programs, any of which on the PATH says it is here.
    programs: &'static [&'static str],
    /// Its settings, in the project.
    file: &'static str,
    format: Format,
    /// How it is started there, and what it asks first.
    start: &'static str,
}

const AGENTS: &[Agent] = &[
    Agent {
        name: "Claude Code",
        flag: "--claude",
        programs: &["claude"],
        file: ".mcp.json",
        format: Format::McpServers,
        start: "Claude Code: start `claude` in this folder and trust it when it asks; \
                /mcp__legume__explore starts the guided work.",
    },
    Agent {
        name: "Codex",
        flag: "--codex",
        programs: &["codex"],
        file: ".codex/config.toml",
        format: Format::Toml,
        start: "Codex: start `codex` in this folder and trust it when it asks: Codex reads a \
                project's servers only in a project you trust.",
    },
    Agent {
        name: "Cursor",
        flag: "--cursor",
        programs: &["agent", "cursor-agent", "cursor"],
        file: ".cursor/mcp.json",
        format: Format::Stdio,
        start: "Cursor: start `agent` in this folder (or open it in Cursor) and turn on the \
                project's servers when it asks.",
    },
    Agent {
        name: "VS Code",
        flag: "--vscode",
        programs: &["code"],
        file: ".vscode/mcp.json",
        format: Format::Servers,
        start: "VS Code: open this folder (`code .`), trust it, and use Copilot Chat in Agent \
                mode; start the servers when it asks.",
    },
    Agent {
        name: "Gemini CLI",
        flag: "--gemini",
        programs: &["gemini"],
        file: ".gemini/settings.json",
        format: Format::Gemini,
        start: "Gemini CLI: start `gemini` in this folder, and trust it if it asks.",
    },
    Agent {
        name: "Antigravity",
        flag: "--antigravity",
        programs: &["agy", "antigravity"],
        file: ".agents/mcp_config.json",
        format: Format::McpServers,
        start: "Antigravity: start `agy` in this folder (or open it in Antigravity).",
    },
    Agent {
        name: "opencode",
        flag: "--opencode",
        programs: &["opencode"],
        file: "opencode.json",
        format: Format::Local,
        start: "opencode: start `opencode` in this folder.",
    },
];

/// The instructions every agent reads, and Claude Code's, which imports
/// them.
const INSTRUCTIONS: &str = "AGENTS.md";
const CLAUDE_MD: &str = "CLAUDE.md";

/// The lines around what `legume init` writes in the instruction files:
/// the text between them is rewritten each time, the rest is the user's.
const BEGIN: &str = "<!-- legume init: begin. `legume init` rewrites the text between these \
                     lines; write your own notes outside them. -->";
const END: &str = "<!-- legume init: end -->";

/// `legume init`'s usage.
pub const USAGE: &str = "\
legume init [--project DIR] [--claude] [--codex] [--cursor] [--vscode]
            [--gemini] [--antigravity] [--opencode]

Connect this project to the agents that run the legume tools. The `legume`
server and each legume tool on the PATH that serves MCP are listed in each
agent's MCP settings in the project:

  Claude Code   .mcp.json             (approved in .claude/settings.local.json)
  Codex         .codex/config.toml    (read once you trust the project)
  Cursor        .cursor/mcp.json
  VS Code       .vscode/mcp.json      (Copilot Chat, Agent mode)
  Gemini CLI    .gemini/settings.json
  Antigravity   .agents/mcp_config.json
  opencode      opencode.json

All of them, or only the ones named. AGENTS.md gets the instructions they
all read: how to work with the person and the tools, from the guide's quick
card; CLAUDE.md imports it for Claude Code. Servers already listed are kept
as they are, and in AGENTS.md and CLAUDE.md only the legume section is
rewritten. The project is marked (.legume/), so the legume server keeps its
records in results/ whichever subfolder an agent starts in. Not your home
folder or /: every folder under them would be the project. Nothing is
installed.";

/// `legume init ...` on the command line. A command line it cannot run is
/// refused with exit code 2 ([`crate::command::usage`]).
pub fn main(args: &[String], out: &mut dyn Write) -> Result<()> {
    let mut project = None;
    let mut named: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--project" => {
                let Some(dir) = it.next() else {
                    return Err(usage("--project needs a folder"));
                };
                project = Some(PathBuf::from(dir));
            }
            "-h" | "--help" => {
                writeln!(out, "{USAGE}")?;
                return Ok(());
            }
            flag => match AGENTS.iter().find(|a| a.flag == flag) {
                Some(a) => named.push(a.name),
                None => return Err(usage(format!("unknown argument {flag}"))),
            },
        }
    }
    let project = match project {
        Some(p) => crate::absolute(&p),
        None => std::env::current_dir()?,
    };
    let same = |a: &Path, b: &Path| {
        a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if project.parent().is_none() || home.is_some_and(|h| same(&project, &h)) {
        anyhow::bail!(
            "{} is not a project's folder: every folder under it would keep its records \
             there. Make a folder for the project, change into it and run `legume init` \
             again.",
            project.display()
        );
    }
    let agents: Vec<&Agent> = AGENTS
        .iter()
        .filter(|a| named.is_empty() || named.contains(&a.name))
        .collect();
    let (servers, left) = servers();
    connect(&project, &agents, &servers, &left, out)
}

/// The project connected to `agents`, with `servers` (and the tools `left`
/// out, to say): their settings, the instructions, and how to start each.
fn connect(
    project: &Path,
    agents: &[&Agent],
    servers: &[(String, String)],
    left: &[&str],
    out: &mut dyn Write,
) -> Result<()> {
    writeln!(out, "Project: {}", project.display())?;
    let names: Vec<&str> = servers.iter().map(|s| s.0.as_str()).collect();
    writeln!(out, "Servers: {}", names.join(", "))?;
    if !left.is_empty() {
        writeln!(
            out,
            "  not included (not on the PATH, or no MCP server): {}",
            left.join(", ")
        )?;
    }
    mark(project)?;
    let mut failed = Vec::new();
    for a in agents {
        let saved = match save(project, a.file, a.format, servers) {
            Ok(saved) => saved,
            Err(e) => {
                writeln!(out, "Not written for {}: {e:#}", a.name)?;
                failed.push(a.name);
                continue;
            }
        };
        let at = rel(project, &saved.path);
        if !saved.changed {
            writeln!(out, "{at} for {} lists them all already", a.name)?;
        } else {
            writeln!(out, "Wrote {at} for {}", a.name)?;
            if !saved.kept.is_empty() {
                writeln!(
                    out,
                    "  already listed there, kept as they are: {}",
                    saved.kept.join(", ")
                )?;
            }
        }
        if a.flag == "--claude" {
            approved(project, servers, &saved.ours, out, &mut failed)?;
        }
    }
    let claude = agents.iter().any(|a| a.flag == "--claude");
    for (file, body) in [
        (INSTRUCTIONS, Some(instructions())),
        (CLAUDE_MD, claude.then(claude_md)),
    ] {
        let Some(body) = body else { continue };
        match write_section(&project.join(file), &body, file == CLAUDE_MD) {
            Ok(Some(done)) => writeln!(out, "{done} {file}")?,
            Ok(None) => writeln!(out, "{file} already imports {INSTRUCTIONS}: kept as it is")?,
            Err(e) => {
                writeln!(out, "Not written: {e:#}")?;
                failed.push(file);
            }
        }
    }
    writeln!(out)?;
    let here: Vec<&Agent> = agents
        .iter()
        .filter(|a| a.programs.iter().any(|p| crate::on_path(p).is_some()))
        .copied()
        .collect();
    for a in if here.is_empty() { agents } else { &here } {
        writeln!(out, "{}", a.start)?;
    }
    let elsewhere: Vec<&str> = agents
        .iter()
        .filter(|a| !here.is_empty() && !here.iter().any(|h| h.name == a.name))
        .map(|a| a.name)
        .collect();
    if !elsewhere.is_empty() {
        writeln!(
            out,
            "Also ready for {} (not on this PATH; `legume init --help` says where each \
             looks).",
            elsewhere.join(", ")
        )?;
    }
    writeln!(
        out,
        "Then ask it to look at your data: {INSTRUCTIONS} tells it how to work with you."
    )?;
    if !failed.is_empty() {
        anyhow::bail!("not written: {}", failed.join(", "));
    }
    Ok(())
}

/// The project marked as one ([`crate::project::MARK`]): its servers find
/// it from any subfolder an agent is started in.
fn mark(project: &Path) -> Result<()> {
    let dir = project.join(crate::project::MARK);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    std::fs::create_dir(&dir).with_context(|| format!("making {}", dir.display()))?;
    crate::project::write_fresh(
        &dir.join("README.md"),
        b"This folder marks the project for the legume tools (`legume init`): the\n\
          legume server keeps the project's records in results/ beside it,\n\
          whichever subfolder an agent is started in.\n",
    )
}

/// `path` from `root`.
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// `legume` and each legume tool on the PATH that serves MCP, by name and
/// command (a name the PATH finds, so the settings hold no one's own
/// paths); and the tools left out.
fn servers() -> (Vec<(String, String)>, Vec<&'static str>) {
    let legume = match crate::on_path("legume") {
        Some(_) => "legume".to_string(),
        None => std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "legume".into()),
    };
    let mut out = vec![("legume".to_string(), legume)];
    let mut left = Vec::new();
    for &tool in TOOLS {
        let serves = crate::on_path(tool)
            .is_some_and(|path| quiet(std::process::Command::new(path).args(["mcp", "--help"])));
        if serves {
            out.push((tool.to_string(), tool.to_string()));
        } else {
            left.push(tool);
        }
    }
    (out, left)
}

/// What [`save`] did with an agent's MCP settings.
struct Saved {
    path: PathBuf,
    /// The servers listed there already, kept as they are.
    kept: Vec<String>,
    /// The servers listed as `legume init` writes them: in JSON settings,
    /// new or kept ([`entry`]); in Codex's TOML, the new ones only.
    ours: Vec<String>,
    /// Whether the file changed (it is not written when it would not).
    changed: bool,
}

/// The project's MCP settings at `file` with `servers` added (each
/// `<command> mcp`), the servers already there kept as they are (their
/// command, arguments and environment are the user's).
fn save(project: &Path, file: &str, format: Format, servers: &[(String, String)]) -> Result<Saved> {
    let path = project.join(file);
    let old = read_own(&path)?;
    let mut changed = old.is_none();
    let mut kept = Vec::new();
    let mut ours = Vec::new();
    let text = if format == Format::Toml {
        let mut text = String::from_utf8(old.unwrap_or_default())
            .with_context(|| format!("{} is not UTF-8", path.display()))?;
        let fresh: Vec<_> = servers
            .iter()
            .filter(|(name, _)| {
                let listed = toml_lists(&text, name);
                if listed {
                    kept.push(name.clone());
                }
                !listed
            })
            .collect();
        if !fresh.is_empty() {
            changed = true;
            if !text.is_empty() && !text.ends_with("\n\n") {
                text.push_str(if text.ends_with('\n') { "\n" } else { "\n\n" });
            }
            text.push_str("# The legume tools (legume init).\n");
            for (name, command) in fresh {
                ours.push(name.clone());
                // A JSON string is a TOML basic string: the same escapes.
                text.push_str(&format!(
                    "[mcp_servers.{name}]\ncommand = {}\nargs = [\"mcp\"]\n\n",
                    json!(command)
                ));
            }
            text.pop();
        }
        text
    } else {
        let mut root = object(old.as_deref(), &path)?;
        let key = match format {
            Format::Servers => "servers",
            Format::Local => "mcp",
            _ => "mcpServers",
        };
        let map = root
            .entry(key)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .with_context(|| format!("{}'s {key} is not an object", path.display()))?;
        for (name, command) in servers {
            let mine = entry(format, command);
            match map.get(name) {
                Some(theirs) => {
                    kept.push(name.clone());
                    if *theirs == mine {
                        ours.push(name.clone());
                    }
                }
                None => {
                    map.insert(name.clone(), mine);
                    ours.push(name.clone());
                    changed = true;
                }
            }
        }
        if format == Format::Gemini {
            let context = root
                .entry("context")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .with_context(|| format!("{}'s context is not an object", path.display()))?;
            let before = context.remove("fileName");
            let names = match before.clone() {
                None => vec![json!(INSTRUCTIONS), json!("GEMINI.md")],
                Some(Value::Array(mut a)) => {
                    if !a.contains(&json!(INSTRUCTIONS)) {
                        a.push(json!(INSTRUCTIONS));
                    }
                    a
                }
                Some(one) if one == json!(INSTRUCTIONS) => vec![one],
                Some(one) => vec![one, json!(INSTRUCTIONS)],
            };
            let names = Value::Array(names);
            changed |= before.as_ref() != Some(&names);
            context.insert("fileName".into(), names);
        }
        serde_json::to_string_pretty(&root)? + "\n"
    };
    folder_for(&path)?;
    if changed {
        replace(&path, text.as_bytes())?;
    }
    Ok(Saved {
        path,
        kept,
        ours,
        changed,
    })
}

/// A server as `legume init` lists it in a JSON settings file of `format`:
/// `<command> mcp`.
fn entry(format: Format, command: &str) -> Value {
    match format {
        Format::Stdio | Format::Servers => {
            json!({ "type": "stdio", "command": command, "args": ["mcp"] })
        }
        Format::Local => json!({ "type": "local", "command": [command, "mcp"], "enabled": true }),
        _ => json!({ "command": command, "args": ["mcp"] }),
    }
}

/// The JSON object a settings file at `path` holds (`old`, its bytes), or
/// an empty one when there is no file.
fn object(old: Option<&[u8]>, path: &Path) -> Result<serde_json::Map<String, Value>> {
    match old {
        Some(b) => match serde_json::from_slice::<Value>(b)
            .with_context(|| format!("reading {}", path.display()))?
        {
            Value::Object(m) => Ok(m),
            _ => anyhow::bail!("{} is not a JSON object", path.display()),
        },
        None => Ok(serde_json::Map::new()),
    }
}

/// The folder the file at `path` goes in, made if it is not there.
fn folder_for(path: &Path) -> Result<&Path> {
    let dir = parent_folder(path)?;
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    Ok(dir)
}

/// The folder the file at `path` goes in (not made), unless something
/// other than a folder is there.
fn parent_folder(path: &Path) -> Result<&Path> {
    let dir = path.parent().context("a file in the project")?;
    if std::fs::symlink_metadata(dir).is_ok_and(|m| !m.is_dir()) {
        anyhow::bail!("{} is not a folder", dir.display());
    }
    Ok(dir)
}

/// Claude Code's own settings for the project, kept out of version control.
const CLAUDE_LOCAL: &str = ".claude/settings.local.json";

/// What [`approve`] did.
enum Approval {
    /// The project approves all its servers already.
    All,
    /// The servers approved one by one.
    Listed {
        /// Whether any was added.
        changed: bool,
        /// Those left off, as the user turned them off.
        off: Vec<String>,
    },
}

/// The servers listed as `legume init` writes them (`ours`, of `servers`)
/// approved for Claude Code ([`approve`]), and said; a failure is added to
/// `failed`.
fn approved(
    project: &Path,
    servers: &[(String, String)],
    ours: &[String],
    out: &mut dyn Write,
    failed: &mut Vec<&str>,
) -> Result<()> {
    let (changed, off) = match approve(project, ours) {
        Ok(Approval::All) => {
            writeln!(
                out,
                "{CLAUDE_LOCAL} approves all the project's servers for Claude Code already"
            )?;
            return Ok(());
        }
        Ok(Approval::Listed { changed, off }) => (changed, off),
        Err(e) => {
            writeln!(
                out,
                "Not approved for Claude Code (it asks to approve the project's servers): {e:#}"
            )?;
            failed.push(CLAUDE_LOCAL);
            return Ok(());
        }
    };
    let on_already = !changed && ours.iter().any(|n| !off.contains(n));
    if changed {
        writeln!(
            out,
            "Approved them for Claude Code in {CLAUDE_LOCAL} (it will not ask each time)"
        )?;
    } else if on_already {
        writeln!(out, "{CLAUDE_LOCAL} approves them for Claude Code already")?;
    }
    if !off.is_empty() {
        writeln!(
            out,
            "  left off, as you turned them off there: {}",
            off.join(", ")
        )?;
    }
    let asked: Vec<&str> = servers
        .iter()
        .map(|s| s.0.as_str())
        .filter(|n| !ours.iter().any(|o| o == n))
        .collect();
    if !asked.is_empty() {
        writeln!(
            out,
            "  not approved, as .mcp.json lists them otherwise than legume init writes them \
             (Claude Code asks about these): {}",
            asked.join(", ")
        )?;
    }
    Ok(())
}

/// The servers listed as `legume init` writes them (`ours`, from [`save`])
/// approved for Claude Code in the project ([`CLAUDE_LOCAL`]'s
/// `enabledMcpjsonServers`), so it does not ask each time it starts; one
/// `.mcp.json` lists otherwise is left for Claude Code to ask about. The
/// user's own entries are kept, a server in its `disabledMcpjsonServers`
/// stays off, and a project that approves all its servers is left as it
/// is. Claude Code applies the approvals only while git does not track the
/// file, so it is ignored ([`ignore_local`]). No git is run: the project
/// is not yet trusted, and its git settings could run a program.
fn approve(project: &Path, ours: &[String]) -> Result<Approval> {
    let path = project.join(CLAUDE_LOCAL);
    let dir = parent_folder(&path)?;
    let old = read_own(&path)?;
    let mut root = object(old.as_deref(), &path)?;
    if root.get("enableAllProjectMcpServers") == Some(&json!(true)) {
        return Ok(Approval::All);
    }
    let off = root
        .get("disabledMcpjsonServers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let on = root
        .entry("enabledMcpjsonServers")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .with_context(|| format!("{}'s enabledMcpjsonServers is not a list", path.display()))?;
    let (mut changed, mut left_off) = (false, Vec::new());
    for name in ours {
        let n = json!(name);
        if off.contains(&n) {
            left_off.push(name.clone());
        } else if !on.contains(&n) {
            on.push(n);
            changed = true;
        }
    }
    if changed {
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
        ignore_local(dir)?;
        replace(
            &path,
            (serde_json::to_string_pretty(&root)? + "\n").as_bytes(),
        )?;
    }
    Ok(Approval::Listed {
        changed,
        off: left_off,
    })
}

/// Whether `cmd` succeeds, run with nothing in and its output dropped.
fn quiet(cmd: &mut std::process::Command) -> bool {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `settings.local.json` named in the `.gitignore` of Claude Code's folder
/// `dir`, beside whatever the user keeps there: Claude Code ignores the
/// file for git only when it writes the file itself.
fn ignore_local(dir: &Path) -> Result<()> {
    let name = CLAUDE_LOCAL.rsplit('/').next().unwrap_or(CLAUDE_LOCAL);
    let path = dir.join(".gitignore");
    let mut text = read_own_text(&path)?.unwrap_or_default();
    if text.lines().any(|l| l.trim() == name) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(name);
    text.push('\n');
    replace(&path, text.as_bytes())
}

/// The file at `path`, if there is one and it is the user's own.
fn read_own(path: &Path) -> Result<Option<Vec<u8>>> {
    match crate::project::read_trusted(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| {
            format!(
                "{} is not your own file (or others can change it): not changed",
                path.display()
            )
        }),
    }
}

/// The text file at `path`, if there is one and it is the user's own.
fn read_own_text(path: &Path) -> Result<Option<String>> {
    read_own(path)?
        .map(|b| String::from_utf8(b).with_context(|| format!("{} is not UTF-8", path.display())))
        .transpose()
}

/// Whether Codex's settings already list the server `name`: as its own
/// table (`[mcp_servers.name]`, quoted or not, or one under it), or as a
/// key of an `[mcp_servers]` table.
fn toml_lists(text: &str, name: &str) -> bool {
    let mut section = "";
    text.lines().map(str::trim).any(|line| {
        if line.starts_with('[') {
            section = line;
            let t = line.trim_matches(|c| c == '[' || c == ']').replace(' ', "");
            let t = t.replace(&format!("\"{name}\""), name);
            return t == format!("mcp_servers.{name}")
                || t.starts_with(&format!("mcp_servers.{name}."));
        }
        section.replace(' ', "") == "[mcp_servers]"
            && [name.to_string(), format!("\"{name}\"")].iter().any(|k| {
                line.strip_prefix(k.as_str())
                    .is_some_and(|r| r.trim_start().starts_with('='))
            })
    })
}

/// The legume section of `AGENTS.md`: how to work with the person and the
/// tools, the guide's quick card at its heart.
fn instructions() -> String {
    let card = crate::brain::section("quick").map_or(String::new(), |s| s.body.trim().to_string());
    format!(
        "{BEGIN}\n\
         ## Working with the legume tools\n\n\
         This project is analysed with the legume tools, connected to you as MCP servers \
         by `legume init`. The `legume` server keeps the project's one notebook and its \
         records, and serves the guide to this work; the tools' own servers run the steps \
         (data-beans, senna, lupin, pinto, cocoa, chickpea, faba). Guide section 11 says \
         what each does and how they connect.\n\n\
         **The rule.** The person thinks; you run the tools, show the facts, ask and check. \
         Never write what a result means, a hypothesis, a verdict, a figure's message or a \
         summary, even when asked: ask, and record the person's own words with the legume \
         server's `notebook`. Research is how the person learns: explain what each number \
         measures, and ask what they expect before you show a result.\n\n\
         **Before your first step**, read the guide: the legume server's `guide` lists its \
         sections and gives any one; sections 4 to 11 are your rules. Its quick card:\n\n\
         {card}\n\n\
         ### Records\n\n\
         Everything is kept under `results/`, so the work can be checked, run again and \
         written up:\n\n\
         - `results/<day>/notebook.md`: the day's notebook, the person's words quoted.\n\
         - `results/<day>/runall.sh`: every step that wrote, to run again without you.\n\
         - `results/INDEX.tsv`: every output, with its inputs and key numbers.\n\
         - `results/DATA.tsv`, `HYPOTHESES.tsv`, `EVIDENCE.tsv`, `FIGURES.tsv`: the \
           person's data verdicts, hypotheses, evidence and figures.\n\
         - `results/<day>/report/`: the day's slides (`report.pdf`, `report.md`).\n\n\
         Run every step through the tools' servers, never their command lines in a shell, \
         so it is recorded and `runall.sh` repeats it; do not edit these files by hand. \
         The person's data goes under `data/`. The working day turns over at 5 a.m.\n\n\
         ### When something is missing\n\n\
         - **No legume tools among yours** (`status`, `notebook`, `guide` from the legume \
           server): this agent has not loaded the project's MCP settings, or they wait \
           for approval. Tell the person to start you in this folder and approve the \
           project's servers (or trust the folder); `legume init --help` says where each \
           agent looks. Do not install anything, and do not run the tools by hand instead.\n\
         - **A tool the work needs is not connected**: say which. `legume init`, run \
           again, lists each legume tool on the PATH that serves MCP; installing one is the \
           person's choice.\n\
         {END}\n"
    )
}

/// Claude Code's section of `CLAUDE.md`: it reads `AGENTS.md` only through
/// an import once a `CLAUDE.md` is there.
fn claude_md() -> String {
    format!(
        "{BEGIN}\nThe instructions for working with the legume tools, which every agent \
         reads:\n\n@{INSTRUCTIONS}\n{END}\n"
    )
}

/// `body` (from [`BEGIN`] to [`END`]) written into the instruction file at
/// `path`: in place of the section a past `legume init` wrote, or after the
/// user's own text. With `import`, a file that already imports
/// `AGENTS.md` is kept as it is (None). What was done, to say.
fn write_section(path: &Path, body: &str, import: bool) -> Result<Option<&'static str>> {
    let old = read_own_text(path)?;
    let text = match &old {
        None => body.to_string(),
        Some(old) if import && old.lines().any(|l| l.trim() == format!("@{INSTRUCTIONS}")) => {
            if !old.contains(BEGIN) {
                return Ok(None);
            }
            splice(old, body)
        }
        Some(old) if old.contains(BEGIN) => splice(old, body),
        Some(old) => {
            let gap = if old.is_empty() || old.ends_with("\n\n") {
                ""
            } else if old.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{old}{gap}{body}")
        }
    };
    if old.as_deref() == Some(text.as_str()) {
        return Ok(Some("Kept"));
    }
    replace(path, text.as_bytes())?;
    Ok(Some(match old {
        None => "Wrote",
        Some(o) if o.contains(BEGIN) => "Updated the legume section of",
        Some(_) => "Added the legume section to",
    }))
}

/// `old` with its legume section (from [`BEGIN`] to [`END`], or to its end
/// when the end line is gone) replaced by `body`.
fn splice(old: &str, body: &str) -> String {
    let at = old.find(BEGIN).unwrap_or(old.len());
    let rest = &old[at..];
    let after = rest.find(END).map_or("", |e| {
        rest[e + END.len()..]
            .strip_prefix('\n')
            .unwrap_or(&rest[e + END.len()..])
    });
    format!("{}{body}{after}", &old[..at])
}

/// `path` replaced by `bytes` at once (a new file beside it, renamed over
/// it), with the old file's permissions: settings that hold tokens stay
/// the user's alone, and a failed write leaves the old file whole.
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let name = path.file_name().context("a file name")?.to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.legume-{}", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode =
            std::fs::symlink_metadata(path).map_or(0o644, |m| m.permissions().mode() & 0o777);
        o.mode(mode);
    }
    let written = o
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_agents_get_the_servers_beside_the_projects_own() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"x"},"senna":{"command":"old"}}}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.path().join(".mcp.json");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let servers = vec![
            ("legume".to_string(), "legume".to_string()),
            ("senna".to_string(), "senna".to_string()),
        ];
        for a in AGENTS {
            save(dir.path(), a.file, a.format, &servers).unwrap();
        }
        let read = |f: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(f)).unwrap()).unwrap()
        };
        let claude = read(".mcp.json");
        assert_eq!(claude["mcpServers"]["other"]["command"], "x", "kept");
        assert_eq!(
            claude["mcpServers"]["senna"],
            json!({ "command": "old" }),
            "the user's own entry, kept as it is"
        );
        assert_eq!(claude["mcpServers"]["legume"]["args"], json!(["mcp"]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(dir.path().join(".mcp.json")).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o600, "its own permissions");
        }
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("legume-"))
            .collect();
        assert!(left.is_empty(), "no file of the write left behind");
        let cursor = read(".cursor/mcp.json");
        assert_eq!(cursor["mcpServers"]["legume"]["args"], json!(["mcp"]));
        assert!(cursor["mcpServers"].get("other").is_none());
    }

    #[test]
    fn each_agent_gets_the_servers_in_its_own_form() {
        let dir = tempfile::tempdir().unwrap();
        let write = |f: &str, text: &str| write(dir.path(), f, text);
        write(
            ".codex/config.toml",
            "model = \"o4\"\n\n[mcp_servers.senna]\ncommand = \"/opt/senna\"\nargs = [\"mcp\"]\n",
        );
        write(
            ".gemini/settings.json",
            r#"{"context":{"fileName":"GEMINI.md"}}"#,
        );
        let servers = vec![
            ("legume".to_string(), "legume".to_string()),
            ("senna".to_string(), "senna".to_string()),
            ("data-beans".to_string(), "data-beans".to_string()),
        ];
        for a in AGENTS {
            save(dir.path(), a.file, a.format, &servers).unwrap();
        }
        let toml = std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        assert!(
            toml.starts_with("model = \"o4\"\n\n[mcp_servers.senna]\ncommand = \"/opt/senna\""),
            "{toml}"
        );
        assert!(
            toml.contains("[mcp_servers.legume]\ncommand = \"legume\"\nargs = [\"mcp\"]\n"),
            "{toml}"
        );
        assert!(toml.contains("[mcp_servers.data-beans]\n"), "{toml}");
        assert_eq!(
            toml.matches("[mcp_servers.senna]").count(),
            1,
            "the user's own, kept"
        );
        assert!(toml.ends_with("args = [\"mcp\"]\n"), "{toml:?}");
        let read = |f: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(f)).unwrap()).unwrap()
        };
        assert_eq!(
            read(".vscode/mcp.json")["servers"]["legume"],
            json!({ "type": "stdio", "command": "legume", "args": ["mcp"] })
        );
        assert_eq!(
            read(".cursor/mcp.json")["mcpServers"]["senna"]["type"],
            "stdio"
        );
        assert_eq!(
            read("opencode.json")["mcp"]["data-beans"],
            json!({ "type": "local", "command": ["data-beans", "mcp"], "enabled": true })
        );
        assert_eq!(
            read(".agents/mcp_config.json")["mcpServers"]["legume"]["args"],
            json!(["mcp"])
        );
        let gemini = read(".gemini/settings.json");
        assert_eq!(
            gemini["context"]["fileName"],
            json!(["GEMINI.md", "AGENTS.md"])
        );
        assert_eq!(gemini["mcpServers"]["legume"]["command"], "legume");

        // Run again: nothing new, nothing doubled.
        for a in AGENTS {
            let s = save(dir.path(), a.file, a.format, &servers).unwrap();
            assert_eq!((s.kept.len(), s.changed), (3, false), "{}", a.name);
        }
        let again = std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        assert_eq!(again, toml);
        assert_eq!(
            read(".gemini/settings.json")["context"]["fileName"],
            json!(["GEMINI.md", "AGENTS.md"])
        );
    }

    /// `name`s served by commands of the same names.
    fn named(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .map(|n| (n.to_string(), n.to_string()))
            .collect()
    }

    /// `text` written to `f` in `dir`, its folders made.
    fn write(dir: &Path, f: &str, text: &str) {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// What [`approve`] did, for a project that approves servers one by one.
    fn listed(a: Approval) -> (bool, Vec<String>) {
        match a {
            Approval::Listed { changed, off } => (changed, off),
            Approval::All => panic!("all approved"),
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// `legume init --claude` in `dir`, with two servers whatever is on
    /// the PATH: whether it went, and what it said.
    fn init_claude(dir: &Path) -> (Result<()>, String) {
        let claude: Vec<&Agent> = AGENTS.iter().filter(|a| a.flag == "--claude").collect();
        let mut out = Vec::new();
        let r = connect(dir, &claude, &named(&["legume", "senna"]), &[], &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn claude_code_is_told_the_servers_are_approved_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CLAUDE_LOCAL);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let ignore = dir.path().join(".claude/.gitignore");
        std::fs::write(&ignore, "worktrees/").unwrap();
        std::fs::write(
            &path,
            r#"{"permissions":{"allow":["Bash(ls)"]},"enabledMcpjsonServers":["other"],"disabledMcpjsonServers":["senna"]}"#,
        )
        .unwrap();
        // The project came with its own entry for one of the names.
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"pinto":{"command":"sh","args":["-c","x"]}}}"#,
        )
        .unwrap();
        let servers = named(&["legume", "senna", "pinto", "cocoa"]);
        let saved = save(dir.path(), ".mcp.json", Format::McpServers, &servers).unwrap();
        assert_eq!(
            saved.ours,
            ["legume", "senna", "cocoa"],
            "not pinto: its own"
        );
        let (changed, off) = listed(approve(dir.path(), &saved.ours).unwrap());
        assert!(changed);
        assert_eq!(off, ["senna"], "said to be left off");
        let v = read_json(&path);
        assert_eq!(
            v["enabledMcpjsonServers"],
            json!(["other", "legume", "cocoa"]),
            "the user's own kept first; one they turned off stays off"
        );
        assert_eq!(
            v["permissions"]["allow"],
            json!(["Bash(ls)"]),
            "the rest kept"
        );
        assert_eq!(v["disabledMcpjsonServers"], json!(["senna"]));
        assert!(
            !listed(approve(dir.path(), &saved.ours).unwrap()).0,
            "nothing new the second time"
        );
        assert_eq!(
            std::fs::read_to_string(&ignore).unwrap(),
            "worktrees/\nsettings.local.json\n",
            "kept out of git, once, beside the user's own lines"
        );

        std::fs::write(&path, r#"{"enableAllProjectMcpServers":true}"#).unwrap();
        assert!(matches!(
            approve(dir.path(), &saved.ours).unwrap(),
            Approval::All
        ));
        assert_eq!(
            read_json(&path),
            json!({ "enableAllProjectMcpServers": true })
        );
    }

    #[test]
    fn a_fresh_project_gets_its_approvals_kept_out_of_git() {
        let dir = tempfile::tempdir().unwrap();
        let servers = named(&["legume", "senna"]);
        let saved = save(dir.path(), ".mcp.json", Format::McpServers, &servers).unwrap();
        assert!(listed(approve(dir.path(), &saved.ours).unwrap()).0);
        assert_eq!(
            read_json(&dir.path().join(CLAUDE_LOCAL)),
            json!({ "enabledMcpjsonServers": ["legume", "senna"] })
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".claude/.gitignore")).unwrap(),
            "settings.local.json\n"
        );

        // Nothing to approve: no folder made.
        let none = tempfile::tempdir().unwrap();
        assert!(!listed(approve(none.path(), &[]).unwrap()).0);
        assert!(!none.path().join(".claude").exists());
    }

    #[test]
    fn init_approves_for_claude_code_only_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let (r, out) = init_claude(dir.path());
        assert!(r.is_ok(), "{out}");
        assert!(
            out.contains(
                "Wrote .mcp.json for Claude Code\nApproved them for Claude Code in \
                 .claude/settings.local.json"
            ),
            "{out}"
        );
        let (_, out) = init_claude(dir.path());
        assert!(
            out.contains(".claude/settings.local.json approves them for Claude Code already"),
            "{out}"
        );

        // .mcp.json not written: nothing approved.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join(".mcp.json"), "not json").unwrap();
        let (r, out) = init_claude(other.path());
        assert!(r.is_err());
        assert!(!out.contains("pproved"), "{out}");
        assert!(!other.path().join(".claude").exists());

        // Approval not written: said, and init fails.
        let third = tempfile::tempdir().unwrap();
        std::fs::write(third.path().join(".claude"), "").unwrap();
        let (r, out) = init_claude(third.path());
        assert!(r.is_err());
        assert!(
            out.contains("Not approved for Claude Code (it asks")
                && out.contains("is not a folder"),
            "{out}"
        );
    }

    #[test]
    fn init_says_what_claude_code_will_ask_about() {
        // The project lists legume its own way; `settings` are Claude
        // Code's local settings there.
        let with = |settings: &str| {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), CLAUDE_LOCAL, settings);
            write(
                dir.path(),
                ".mcp.json",
                r#"{"mcpServers":{"legume":{"command":"sh"}}}"#,
            );
            let (r, out) = init_claude(dir.path());
            assert!(r.is_ok(), "{out}");
            out
        };
        let out = with(r#"{"disabledMcpjsonServers":["senna"]}"#);
        assert!(
            !out.contains("Approved them") && !out.contains("approves them"),
            "{out}"
        );
        assert!(
            out.contains("left off, as you turned them off there: senna"),
            "{out}"
        );
        assert!(
            out.contains("(Claude Code asks about these): legume"),
            "{out}"
        );

        // All approved already: nothing to ask about.
        let out = with(r#"{"enableAllProjectMcpServers":true}"#);
        assert!(out.contains("approves all the project's servers"), "{out}");
        assert!(!out.contains("asks about"), "{out}");
    }

    #[test]
    fn gemini_is_told_of_agents_md_when_its_servers_are_there() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".gemini/settings.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"mcpServers":{"legume":{"command":"legume"}}}"#).unwrap();
        let servers = vec![("legume".to_string(), "legume".to_string())];
        let s = save(
            dir.path(),
            ".gemini/settings.json",
            Format::Gemini,
            &servers,
        )
        .unwrap();
        assert_eq!((s.kept.len(), s.changed), (1, true));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["context"]["fileName"], json!(["AGENTS.md", "GEMINI.md"]));
    }

    #[test]
    fn codex_servers_listed_in_any_form_are_kept() {
        assert!(toml_lists("[mcp_servers.legume]\n", "legume"));
        assert!(toml_lists("[ mcp_servers.\"legume\" ]\n", "legume"));
        assert!(toml_lists(
            "[mcp_servers.legume.env]\nX = \"1\"\n",
            "legume"
        ));
        assert!(toml_lists(
            "[mcp_servers]\nlegume = { command = \"x\" }\n",
            "legume"
        ));
        assert!(!toml_lists("[mcp_servers.legume-x]\n", "legume"));
        assert!(!toml_lists("[other]\nlegume = 1\n", "legume"));
        assert!(!toml_lists("[mcp_servers]\nlegumes = {}\n", "legume"));
    }

    #[test]
    fn the_instructions_are_one_section_of_the_users_file() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join(INSTRUCTIONS);
        std::fs::write(&agents, "# My project\n\nUse R 4.4.").unwrap();
        let body = instructions();
        assert!(
            body.contains("**The rule.**") && body.contains("call `status`"),
            "{body}"
        );
        assert_eq!(
            write_section(&agents, &body, false).unwrap(),
            Some("Added the legume section to")
        );
        let text = std::fs::read_to_string(&agents).unwrap();
        assert!(text.starts_with("# My project\n\nUse R 4.4.\n\n<!-- legume init: begin"));
        assert!(text.ends_with("<!-- legume init: end -->\n"));

        // The user writes after it; init again rewrites only its own lines.
        std::fs::write(
            &agents,
            text.replace("Ask the person", "ASK") + "\nMy notes.\n",
        )
        .unwrap();
        assert_eq!(
            write_section(&agents, &body, false).unwrap(),
            Some("Updated the legume section of")
        );
        let text = std::fs::read_to_string(&agents).unwrap();
        assert!(!text.contains("ASK"));
        assert!(
            text.ends_with("<!-- legume init: end -->\n\nMy notes.\n"),
            "{text}"
        );
        assert_eq!(write_section(&agents, &body, false).unwrap(), Some("Kept"));
        assert_eq!(text.matches(BEGIN).count(), 1);

        let claude = dir.path().join(CLAUDE_MD);
        assert_eq!(
            write_section(&claude, &claude_md(), true).unwrap(),
            Some("Wrote")
        );
        assert!(std::fs::read_to_string(&claude)
            .unwrap()
            .contains("\n@AGENTS.md\n"));
        std::fs::write(&claude, "Be brief.\n@AGENTS.md\n").unwrap();
        assert_eq!(
            write_section(&claude, &claude_md(), true).unwrap(),
            None,
            "imports it already"
        );
        assert_eq!(
            std::fs::read_to_string(&claude).unwrap(),
            "Be brief.\n@AGENTS.md\n"
        );
    }

    #[test]
    fn the_root_folder_is_not_a_project() {
        let mut out = Vec::new();
        let e = main(&["--project".into(), "/".into()], &mut out).unwrap_err();
        assert!(e.to_string().contains("is not a project's folder"), "{e}");
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy().into_owned();
            assert!(main(&["--project".into(), home], &mut out).is_err());
        }
    }

    #[test]
    fn the_project_is_marked_once() {
        let dir = tempfile::tempdir().unwrap();
        mark(dir.path()).unwrap();
        mark(dir.path()).unwrap();
        assert!(dir.path().join(".legume/README.md").is_file());
    }

    #[test]
    fn settings_another_can_change_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, "{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
            let Err(e) = save(dir.path(), ".mcp.json", Format::McpServers, &[]) else {
                panic!("written");
            };
            assert!(format!("{e:#}").contains("not your own file"), "{e:#}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        }
    }
}
