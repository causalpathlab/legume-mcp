//! `legume init`: a project connected to the agents that run the legume
//! tools. It lists the `legume` server and each legume tool on the PATH
//! that serves MCP (`<tool> mcp`) in the project's own MCP settings: Claude
//! Code's `.mcp.json` and Cursor's `.cursor/mcp.json` (`agent`). The agent
//! started in that folder then has them all. Servers already listed there
//! are kept; nothing is installed.

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

/// An agent whose MCP settings a project can hold.
struct Agent {
    name: &'static str,
    /// Its programs, any of which on the PATH says it is here.
    programs: &'static [&'static str],
    /// Its settings, in the project.
    file: &'static str,
    /// How it is started there.
    start: &'static str,
}

const AGENTS: &[Agent] = &[
    Agent {
        name: "Claude Code",
        programs: &["claude"],
        file: ".mcp.json",
        start: "claude",
    },
    Agent {
        name: "Cursor",
        programs: &["agent", "cursor-agent", "cursor"],
        file: ".cursor/mcp.json",
        start: "agent",
    },
];

/// `legume init`'s usage.
pub const USAGE: &str = "\
legume init [--project DIR] [--claude] [--cursor]

Connect this project to the agents that run the legume tools: the `legume`
server and each legume tool on the PATH that serves MCP are listed in the
project's MCP settings, .mcp.json for Claude Code (`claude`) and
.cursor/mcp.json for Cursor (`agent`); those on the PATH, else both, or the
ones named. Servers already listed there are kept as they are. The project
is marked (.legume/), so the legume server keeps its records in results/
whichever subfolder an agent starts in. Not your home folder or /: every
folder under them would be the project. Nothing is installed.";

/// `legume init ...` on the command line.
pub fn main(args: &[String], out: &mut dyn Write) -> Result<()> {
    let mut project = None;
    let mut named: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--project" => {
                project = Some(PathBuf::from(
                    it.next().context("--project needs a folder")?,
                ))
            }
            "--claude" => named.push("Claude Code"),
            "--cursor" => named.push("Cursor"),
            "-h" | "--help" => {
                writeln!(out, "{USAGE}")?;
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}\n\n{USAGE}"),
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
    let here: Vec<&Agent> = AGENTS
        .iter()
        .filter(|a| a.programs.iter().any(|p| crate::on_path(p).is_some()))
        .collect();
    let agents: Vec<&Agent> = if !named.is_empty() {
        AGENTS.iter().filter(|a| named.contains(&a.name)).collect()
    } else if here.is_empty() {
        AGENTS.iter().collect()
    } else {
        here
    };
    let (servers, left) = servers();
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
    mark(&project)?;
    for a in &agents {
        let (path, kept) = save(&project, a.file, &servers)?;
        writeln!(out, "Wrote {} for {}", rel(&project, &path), a.name)?;
        if !kept.is_empty() {
            writeln!(
                out,
                "  already listed there, kept as they are: {}",
                kept.join(", ")
            )?;
        }
    }
    writeln!(out)?;
    for a in &agents {
        writeln!(
            out,
            "Start `{}` in this folder: it asks once before it runs the project's servers.",
            a.start
        )?;
    }
    writeln!(
        out,
        "Then ask it to explore your data; in Claude Code, /mcp__legume__explore starts the \
         guided work."
    )?;
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
        let serves = crate::on_path(tool).is_some_and(|path| {
            std::process::Command::new(path)
                .args(["mcp", "--help"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        });
        if serves {
            out.push((tool.to_string(), tool.to_string()));
        } else {
            left.push(tool);
        }
    }
    (out, left)
}

/// The project's MCP settings at `file` with `servers` added (each
/// `<command> mcp`), the servers already there kept as they are (their
/// command, arguments and environment are the user's); where they went,
/// and the servers kept.
fn save(
    project: &Path,
    file: &str,
    servers: &[(String, String)],
) -> Result<(PathBuf, Vec<String>)> {
    let path = project.join(file);
    let mut settings = match crate::project::read_trusted(&path) {
        Ok(b) => serde_json::from_slice::<Value>(&b)
            .with_context(|| format!("reading {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "{} is not your own file (or others can change it): not changed",
                    path.display()
                )
            })
        }
    };
    let map = settings
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| format!("{}'s mcpServers is not an object", path.display()))?;
    let mut kept = Vec::new();
    for (name, command) in servers {
        if map.contains_key(name) {
            kept.push(name.clone());
        } else {
            map.insert(name.clone(), json!({ "command": command, "args": ["mcp"] }));
        }
    }
    let dir = path.parent().context("a file in the project")?;
    if std::fs::symlink_metadata(dir).is_ok_and(|m| !m.is_dir()) {
        anyhow::bail!("{} is not a folder", dir.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let text = serde_json::to_string_pretty(&settings)? + "\n";
    replace(&path, text.as_bytes())?;
    Ok((path, kept))
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
            save(dir.path(), a.file, &servers).unwrap();
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
            let e = save(dir.path(), ".mcp.json", &[]).unwrap_err();
            assert!(format!("{e:#}").contains("not your own file"), "{e:#}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        }
    }
}
