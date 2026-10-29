//! A project's chat settings, `.legume/chat.json`, as `legume init` writes
//! them: which model, on which server (and how to start that server when
//! nothing answers), with which tools. `legume chat` reads them; its own
//! arguments win. `legume init` finds what is on this machine and never
//! installs anything.

use super::http::{self, Url};
use super::model::KNOWN;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a project keeps its chat settings.
pub const FILE: &str = ".legume/chat.json";

/// Model servers `legume init` names when none is running, and how each
/// is started.
const SERVERS: &[(&str, &str)] = &[
    (
        "llama-server",
        "llama-server -m <model.gguf> --port 8080 -c 8192 --jinja",
    ),
    ("ollama", "ollama serve"),
    ("lms", "lms server start"),
    ("mlx_lm.server", "mlx_lm.server --model <model> --port 8000"),
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub json: bool,
    /// The model server's command line, started by the chat when nothing
    /// answers at `url`, and stopped as it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Vec<String>>,
    /// The model's weights, for the record: its path and checksum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_file: Option<ModelFile>,
}

/// A model file as it was when its checksum was taken.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

impl ModelFile {
    /// The file at `path`, its checksum taken.
    pub(crate) fn of(path: &Path) -> Result<Self> {
        let path = crate::absolute(path);
        let bytes = std::fs::metadata(&path)
            .with_context(|| format!("{}", path.display()))?
            .len();
        let sha256 =
            super::sha256::file(&path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Self {
            path,
            bytes,
            sha256,
        })
    }

    /// Whether the file is still the one recorded, by its size.
    pub(crate) fn unchanged(&self) -> bool {
        std::fs::metadata(&self.path).is_ok_and(|m| m.len() == self.bytes)
    }
}

/// The project's settings; none when it has no `.legume/chat.json`. One
/// that is not this user's own regular file (a symlink, another's, or
/// writable by others) is refused: it names a command the chat may run.
pub fn load(project: &Path) -> Result<Config> {
    let path = project.join(FILE);
    match crate::project::read_trusted(&path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => anyhow::bail!(
            "{} is not your own file (or others can change it): not read; \
             check it, then make it yours (or run `legume init --force`)",
            path.display()
        ),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// `legume init`'s usage.
pub const USAGE: &str = "\
legume init [--url URL] [--model NAME] [--context TOKENS] [--tools a,b,...]
            [--server 'COMMAND ...'] [--model-file PATH] [--project DIR] [--force]

Set up this project for `legume chat`: finds the legume tools on the PATH and
a model server answering on this machine (or the command that starts one),
and writes .legume/chat.json. It installs nothing, and does not replace an
existing .legume/chat.json without --force.
  --server      the model server's command line, which the chat starts when
                nothing answers at --url and stops as it ends
  --model-file  the model's weights (a .gguf), recorded with their checksum";

/// `legume init`: what it found, and the settings it wrote.
pub fn init(args: &[String], out: &mut impl Write) -> Result<()> {
    let mut c = Config::default();
    let mut project: Option<PathBuf> = None;
    let mut force = false;
    let mut weights: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| {
            it.next()
                .cloned()
                .with_context(|| format!("{a} needs {what}"))
        };
        match a.as_str() {
            "--url" => c.url = Some(value("an address")?),
            "--model" => c.model = Some(value("a name")?),
            "--context" => c.context = Some(value("a number")?.parse().context("--context")?),
            "--tools" => {
                c.tools = Some(
                    value("names")?
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                )
            }
            "--server" => {
                c.server = Some(
                    value("a command line")?
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                )
            }
            "--model-file" => weights = Some(value("a path")?.into()),
            "--project" => project = Some(value("a folder")?.into()),
            "--force" => force = true,
            other => anyhow::bail!("unknown argument {other}\n\n{USAGE}"),
        }
    }
    let project = match project {
        Some(p) => crate::absolute(&p),
        None => std::env::current_dir()?,
    };
    let path = project.join(FILE);
    let there = std::fs::symlink_metadata(&path).is_ok();
    anyhow::ensure!(
        force || !there,
        "{} exists: --force to replace it\n{}",
        path.display(),
        std::fs::read_to_string(&path).unwrap_or_default()
    );
    writeln!(out, "Project: {}", project.display())?;
    if project.join("Cargo.toml").exists() && project.join("src").is_dir() {
        writeln!(
            out,
            "  This looks like a folder of source code, not a project: is it the right one?"
        )?;
    }

    // The tools.
    let found: Vec<&str> = super::TOOLS
        .iter()
        .copied()
        .filter(|t| crate::on_path(t).is_some())
        .collect();
    writeln!(
        out,
        "Tools on the PATH: {}",
        if found.is_empty() {
            "none".into()
        } else {
            found.join(", ")
        }
    )?;
    if let Some(wanted) = &c.tools {
        for t in wanted.iter().filter(|t| !found.contains(&t.as_str())) {
            writeln!(out, "  {t} is not on the PATH")?;
        }
    }

    // The model server: where the one the chat starts listens, unless told.
    if let (None, Some(cmd)) = (&c.url, &c.server) {
        c.url = Some(listens_at(cmd));
    }
    let answering = match &c.url {
        Some(u) => {
            let url = Url::parse(u)?;
            url.ensure_local()?;
            http::get(&url, "/models", Duration::from_secs(2)).map(|m| (url, m))
        }
        None => KNOWN.iter().find_map(|(_, u)| {
            let url = Url::parse(u).ok()?;
            http::get(&url, "/models", Duration::from_millis(400)).map(|m| (url, m))
        }),
    };
    match answering {
        Some((url, models)) => {
            let names: Vec<&str> = models["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["id"].as_str())
                .collect();
            writeln!(out, "Model server: {} ({})", url.show(), names.join(", "))?;
            if c.model.is_none() {
                c.model = names.first().map(|s| s.to_string());
            }
            c.url = Some(url.show());
        }
        None if c.server.is_some() => {
            writeln!(out, "Model server: none answering; the chat will start it")?;
        }
        None => {
            writeln!(out, "Model server: none answering on this machine.")?;
            let here: Vec<&(&str, &str)> = SERVERS
                .iter()
                .filter(|(b, _)| crate::on_path(b).is_some())
                .collect();
            if here.is_empty() {
                writeln!(
                    out,
                    "  None is installed either: llama.cpp (llama-server), Ollama, LM Studio (lms)\n  \
                     or mlx-lm serve a model at an OpenAI-compatible address. Install one, then\n  \
                     run `legume init` again, or give --url."
                )?;
            }
            for (bin, start) in here {
                writeln!(
                    out,
                    "  {bin} is installed: start it with `{start}`, or give it as --server."
                )?;
            }
        }
    }
    if let Some(w) = weights {
        writeln!(out, "Model file: {} (taking its checksum)", w.display())?;
        let f = ModelFile::of(&w)?;
        writeln!(out, "  sha256 {}", f.sha256)?;
        c.model_file = Some(f);
    }
    let text = serde_json::to_string_pretty(&c)? + "\n";
    write_new(&path, &text, there).with_context(|| format!("writing {}", path.display()))?;
    writeln!(
        out,
        "Wrote {}:\n{text}Start with `legume chat` here.",
        path.display()
    )?;
    Ok(())
}

/// `text` as a new file at `path`, its folder a real one of this project:
/// never through a symlink. What was there (`replace`) is removed first, a
/// symlink as a link, never what it points to.
fn write_new(path: &Path, text: &str, replace: bool) -> std::io::Result<()> {
    let dir = path.parent().expect("a file in .legume");
    if std::fs::symlink_metadata(dir).is_ok_and(|m| !m.is_dir()) {
        return Err(std::io::Error::other(format!(
            "{} is not a folder",
            dir.display()
        )));
    }
    std::fs::create_dir_all(dir)?;
    if replace {
        std::fs::remove_file(path)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o644);
    o.open(path)?.write_all(text.as_bytes())
}

/// Where the model server `cmd` starts listens: its `--port`, else its
/// program's default port, on this machine.
fn listens_at(cmd: &[String]) -> String {
    let port = cmd.iter().enumerate().find_map(|(i, w)| match w.as_str() {
        "--port" => cmd.get(i + 1).cloned(),
        w => w.strip_prefix("--port=").map(str::to_string),
    });
    let program = cmd
        .first()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|p| p.to_str())
        .unwrap_or_default();
    let port = port.unwrap_or_else(|| {
        match program {
            "ollama" => "11434",
            "lms" => "1234",
            "vllm" => "8000",
            // llama-server and mlx_lm.server
            _ => "8080",
        }
        .into()
    });
    format!("http://127.0.0.1:{port}/v1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_started_is_looked_for_where_it_listens() {
        let at = |c: &str| listens_at(&c.split(' ').map(String::from).collect::<Vec<_>>());
        assert_eq!(at("ollama serve"), "http://127.0.0.1:11434/v1");
        assert_eq!(
            at("mlx_lm.server --model m --port 8000"),
            "http://127.0.0.1:8000/v1"
        );
        assert_eq!(
            at("/opt/bin/llama-server -m m.gguf --port=9090"),
            "http://127.0.0.1:9090/v1"
        );
        assert_eq!(at("llama-server -m m.gguf"), "http://127.0.0.1:8080/v1");
    }

    #[test]
    fn settings_read_back_as_written_and_unset_ones_are_left_out() {
        let c = Config {
            url: Some("http://127.0.0.1:8080/v1".into()),
            server: Some(vec!["llama-server".into(), "-m".into(), "m.gguf".into()]),
            ..Default::default()
        };
        let text = serde_json::to_string(&c).unwrap();
        assert_eq!(
            text,
            r#"{"url":"http://127.0.0.1:8080/v1","server":["llama-server","-m","m.gguf"]}"#
        );
        assert_eq!(serde_json::from_str::<Config>(&text).unwrap(), c);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), Config::default());
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_settings_file_is_neither_read_nor_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, "{}").unwrap();
        let project = dir.path().join("p");
        std::fs::create_dir_all(project.join(".legume")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join(FILE)).unwrap();
        let e = load(&project).unwrap_err().to_string();
        assert!(e.contains("is not your own file"), "{e}");
        write_new(&project.join(FILE), "{\"json\":true}\n", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(&elsewhere).unwrap(),
            "{}",
            "the link's target untouched"
        );
        assert!(load(&project).unwrap().json);
        // Writable by others: not read.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(project.join(FILE), std::fs::Permissions::from_mode(0o666))
            .unwrap();
        assert!(load(&project).is_err());
    }
}
