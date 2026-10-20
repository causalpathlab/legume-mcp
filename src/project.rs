//! The project records every server keeps, so whatever agents do through
//! the tools can be found again and redone without them. The layout follows
//! Noble's guide to organizing computational biology projects [1]: inputs
//! under `data/` with where each came from; outputs under `results/`,
//! chronologically, one folder per day; a driver script that records every
//! operation, with relative paths and restartable steps; and a lab notebook
//! beside it. The notebook opens with the question, the data and how the
//! result will be checked, after [2].
//!
//! ```text
//! <project>/
//!   data/README.md           an input's source and date, as tools import it
//!   results/INDEX.tsv        one row per output: date, time, server, tool, output, summary
//!   results/<YYYY-MM-DD>/
//!     runall.sh              the day's steps from every server, in order
//!     notebook.md            the agents' account: plan, observations, conclusions
//!     …                      the tools' default outputs
//! ```
//!
//! `runall.sh` holds each tool call as a comment (read-only ones too), each
//! child a tool ran through [`crate::Call::run`] as its command line (the
//! server's own binary under the server's name, paths under the project
//! relative to it, skipped when its outputs exist), and each input a tool
//! wrote through [`crate::Call::write_file`] as a heredoc. Steps that failed
//! or were cancelled stay as comments. Dates and times are local.
//!
//! The project is the server's working directory, or [`ROOT_ENV`];
//! [`OFF_ENV`] set to `off` keeps no records.
//!
//! [1] W. S. Noble, "A quick guide to organizing computational biology
//!     projects", PLoS Comput Biol 5(7): e1000424 (2009),
//!     doi:10.1371/journal.pcbi.1000424.
//! [2] W. S. Noble, "Ten simple rules for defining a computational biology
//!     project", PLoS Comput Biol 19(1): e1010786 (2023),
//!     doi:10.1371/journal.pcbi.1010786.

use base64::Engine;
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The project root, when not the working directory.
pub const ROOT_ENV: &str = "LEGUME_MCP_PROJECT";
/// `off` keeps no records.
pub const OFF_ENV: &str = "LEGUME_MCP_JOURNAL";

pub(crate) struct Project {
    root: PathBuf,
    name: String,
    version: String,
}

/// Local date and time, `YYYY-MM-DD` and `HH:MM:SS`.
fn now() -> (String, String) {
    let local = Command::new("date")
        .arg("+%F %T")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok());
    let text = local.unwrap_or_else(utc);
    let (d, t) = text
        .trim()
        .split_once(' ')
        .unwrap_or(("1970-01-01", "00:00:00"));
    (d.into(), t.into())
}

impl Project {
    pub(crate) fn new(root: PathBuf, name: &str, version: &str) -> Self {
        Self {
            root: std::path::absolute(&root).unwrap_or(root),
            name: name.into(),
            version: version.into(),
        }
    }

    /// The project [`ROOT_ENV`] and [`OFF_ENV`] say: none when `off`.
    pub(crate) fn root_from_env() -> Option<PathBuf> {
        if std::env::var(OFF_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("off")) {
            return None;
        }
        match std::env::var(ROOT_ENV) {
            Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
            _ => std::env::current_dir().ok(),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Today's results folder, made if missing.
    pub(crate) fn day_dir(&self) -> std::io::Result<PathBuf> {
        let dir = self.root.join("results").join(now().0);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// `text` appended to `path`, which starts with `head` when new. Records
    /// are not the work: a write that fails is logged, never the call's
    /// failure. Every server appends to the same files, each write whole.
    fn append(&self, path: &Path, mode: u32, head: impl FnOnce() -> String, text: &str) {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut open = OpenOptions::new();
            open.create_new(true).write(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut open, mode);
            let block = match open.open(path) {
                Ok(mut f) => return f.write_all(format!("{}{text}", head()).as_bytes()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => text,
                Err(e) => return Err(e),
            };
            OpenOptions::new()
                .append(true)
                .open(path)?
                .write_all(block.as_bytes())
        };
        if let Err(e) = write() {
            log::warn!("mcp records {}: {e}", path.display());
        }
    }

    /// Append to today's `runall.sh`.
    pub(crate) fn script(&self, text: &str) {
        let Ok(dir) = self.day_dir() else { return };
        let head = || {
            format!(
                "#!/usr/bin/env bash\n\
                 # {}: the steps agents ran through the legume MCP servers, in order.\n\
                 # `#` lines are the calls (read-only ones too) and the steps that did not\n\
                 # succeed. Paths are relative to the project. A step whose outputs exist\n\
                 # is skipped: delete them to redo it.\n\
                 set -euo pipefail\n\
                 cd \"$(dirname \"$0\")/../..\"\n",
                now().0
            )
        };
        self.append(&dir.join("runall.sh"), 0o700, head, text);
    }

    /// The comment a tool call starts with.
    pub(crate) fn call(&self, tool: &str, args: &Value) {
        let args = self.rel(&args.to_string());
        self.script(&format!("\n# {} {} {tool} {args}\n", now().1, self.name));
    }

    /// `s` with the project's root dropped from the paths in it.
    pub(crate) fn rel(&self, s: &str) -> String {
        let root = self.root.to_string_lossy();
        if root == "/" {
            return s.into();
        }
        let s = s.replace(&format!("{root}/"), "");
        if s == root {
            ".".into()
        } else {
            s
        }
    }

    /// `cmd` as a shell line: its own directory and environment, this
    /// server's binary under the server's name, project paths relative, and
    /// skipped when every one of `outputs` exists.
    pub(crate) fn command_line(&self, cmd: &Command, outputs: &[PathBuf]) -> String {
        let word = |s: &str| quote(&self.rel(s));
        let program = Path::new(cmd.get_program());
        let own = std::env::current_exe().ok();
        let is_self =
            program == Path::new("/proc/self/exe") || own.as_deref().is_some_and(|e| e == program);
        let mut words: Vec<String> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                let k = k.to_str()?;
                let v = if is_secret(k) {
                    // Asked for at replay, never written down.
                    format!("\"${{{k}:?export {k}}}\"")
                } else {
                    word(&v?.to_string_lossy())
                };
                Some(format!("{k}={v}"))
            })
            .collect();
        words.push(if is_self {
            self.name.clone()
        } else {
            word(&program.to_string_lossy())
        });
        words.extend(cmd.get_args().map(|a| word(&a.to_string_lossy())));
        let mut line = words.join(" ");
        if let Some(dir) = cmd.get_current_dir() {
            line = format!("(cd {} && {line})", word(&dir.to_string_lossy()));
        }
        if outputs.is_empty() {
            return line;
        }
        let missing: Vec<String> = outputs
            .iter()
            .map(|o| format!("[ ! -e {} ]", word(&o.to_string_lossy())))
            .collect();
        let dirs = self.mkdirs(outputs.iter().map(PathBuf::as_path));
        format!("if {}; then\n  {dirs}{line}\nfi", missing.join(" || "))
    }

    /// `mkdir -p` of the folders `paths` go in, project-relative, with a
    /// line break and indent; empty when they all go in the project itself.
    fn mkdirs<'a>(&self, paths: impl Iterator<Item = &'a Path>) -> String {
        let mut dirs: Vec<String> = paths
            .filter_map(|p| Some(self.rel(&p.parent()?.to_string_lossy())))
            .filter(|d| !d.is_empty() && d != "." && d != &*self.root.to_string_lossy())
            .map(|d| quote(&d))
            .collect();
        dirs.dedup();
        if dirs.is_empty() {
            return String::new();
        }
        format!("mkdir -p {}\n  ", dirs.join(" "))
    }

    /// A heredoc that writes `contents` to `path`: as it is when it is plain
    /// text (tabs and newlines), else base64, so no byte bash reads
    /// differently can end the heredoc early.
    pub(crate) fn file(&self, path: &Path, contents: &str) {
        let dir = self.mkdirs(std::iter::once(path)).replace("\n  ", "\n");
        let path = quote(&self.rel(&path.to_string_lossy()));
        if contents
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            let mut b64 = base64::engine::general_purpose::STANDARD.encode(contents);
            // Lines of 76, as `base64` writes them; `_` is not in its alphabet.
            let mut wrapped = String::new();
            while !b64.is_empty() {
                let rest = b64.split_off(b64.len().min(76));
                wrapped += &b64;
                wrapped.push('\n');
                b64 = rest;
            }
            self.script(&format!(
                "{dir}base64 -d > {path} <<'LEGUME_MCP_EOF'\n{wrapped}LEGUME_MCP_EOF\n"
            ));
            return;
        }
        let mut tag = String::from("LEGUME_MCP_EOF");
        while contents.lines().any(|l| l == tag) {
            tag.push('_');
        }
        let nl = if contents.ends_with('\n') || contents.is_empty() {
            ""
        } else {
            "\n"
        };
        self.script(&format!(
            "{dir}cat > {path} <<'{tag}'\n{contents}{nl}{tag}\n"
        ));
    }

    /// A row of `results/INDEX.tsv` per output.
    pub(crate) fn index(&self, tool: &str, outputs: &[PathBuf], summary: &str) {
        let (date, time) = now();
        let summary = one_line(summary);
        let rows: String = outputs
            .iter()
            .map(|o| {
                let o = one_line(&self.rel(&o.to_string_lossy()));
                format!("{date}\t{time}\t{}\t{tool}\t{o}\t{summary}\n", self.name)
            })
            .collect();
        let head = || "date\ttime\tserver\ttool\toutput\tsummary\n".to_string();
        self.append(&self.root.join("results/INDEX.tsv"), 0o644, head, &rows);
    }

    /// An entry in today's `notebook.md`.
    pub(crate) fn notebook(&self, kind: &str, text: &str) {
        let Ok(dir) = self.day_dir() else { return };
        let (date, time) = now();
        let head = || {
            format!(
                "# Notebook, {date}\n\n\
                 The agents' account of the day's work: what was asked, done, seen and\n\
                 concluded. The facts are in the tools' outputs and `runall.sh`.\n"
            )
        };
        let entry = format!(
            "\n## {time} · {} · {kind}\n\n{}\n",
            self.name,
            text.trim_end()
        );
        self.append(&dir.join("notebook.md"), 0o644, head, &entry);
    }

    /// A line of `data/README.md`: where input `path` came from.
    pub(crate) fn provenance(&self, tool: &str, path: &Path, source: &str) {
        let line = format!(
            "- {} `{}`: {} ({} {} {tool})\n",
            now().0,
            self.rel(&path.to_string_lossy()),
            one_line(source),
            self.name,
            self.version
        );
        let head = || "# Data\n\nWhere each input came from, and when.\n\n".to_string();
        self.append(&self.root.join("data/README.md"), 0o644, head, &line);
    }
}

fn one_line(s: &str) -> String {
    s.split(['\t', '\n', '\r'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether environment variable `name` likely holds a secret.
fn is_secret(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    [
        "TOKEN",
        "KEY",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "AUTH",
        "CREDENTIAL",
        "COOKIE",
    ]
    .iter()
    .any(|w| n.contains(w))
}

/// `s` as one shell word.
pub(crate) fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:=@%+,-".contains(&b));
    if plain {
        s.into()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Now in UTC, `YYYY-MM-DD HH:MM:SS`, where `date` cannot say the local time.
fn utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_quoted_only_when_needed() {
        assert_eq!(quote("a/b.c-1"), "a/b.c-1");
        assert_eq!(quote("two words"), "'two words'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn commands_are_relative_restartable_and_keep_no_secrets() {
        let p = Project::new(PathBuf::from("/proj"), "tool", "0");
        let mut cmd = Command::new("/usr/bin/x");
        cmd.env("HF_TOKEN", "hf_abc").env("MODE", "fast").args([
            "-f",
            "/proj/data/a b.zarr",
            "-o",
            "/elsewhere/o",
        ]);
        let line = p.command_line(&cmd, &[PathBuf::from("/proj/results/d/o.json")]);
        assert!(!line.contains("hf_abc"), "{line}");
        assert!(
            line.contains(r#"HF_TOKEN="${HF_TOKEN:?export HF_TOKEN}""#),
            "{line}"
        );
        assert!(
            line.starts_with("if [ ! -e results/d/o.json ]; then\n  mkdir -p results/d\n  "),
            "{line}"
        );
        assert!(
            line.contains("-f 'data/a b.zarr' -o /elsewhere/o\nfi"),
            "{line}"
        );
        assert_eq!(p.rel("/proj"), ".");
        assert_eq!(p.rel("/project/x"), "/project/x", "only the root itself");
    }

    #[test]
    fn dates_are_iso() {
        let (d, t) = now();
        assert_eq!((d.len(), t.len()), (10, 8), "{d} {t}");
        assert_eq!(utc().len(), 19);
    }
}
