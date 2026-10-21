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
use std::sync::Arc;

use crate::watch::{Body, Feed};

/// The project root, when not the working directory.
pub const ROOT_ENV: &str = "LEGUME_MCP_PROJECT";
/// `off` keeps no records.
pub const OFF_ENV: &str = "LEGUME_MCP_RECORDS";

/// The project's folder: [`ROOT_ENV`], else the working directory, as an
/// absolute path (records kept or not).
pub fn root() -> Option<PathBuf> {
    match std::env::var_os(ROOT_ENV) {
        Some(v) if !v.is_empty() => std::path::absolute(v).ok(),
        _ => std::env::current_dir().ok(),
    }
}

/// The project whose records a server keeps, as [`ROOT_ENV`] and
/// [`OFF_ENV`] say: [`root`], or none when records are `off`. Absolute, so
/// a server that changes into it reads the same folder from inside.
pub fn root_from_env() -> Option<PathBuf> {
    records_on().then(root).flatten()
}

/// Whether records are kept: [`OFF_ENV`] is not `off`.
fn records_on() -> bool {
    !std::env::var(OFF_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("off"))
}

/// Change into the project's folder ([`root`]), making it when it is not
/// there yet, and return the records' root ([`root_from_env`]): what a
/// server does first so the relative paths an agent gives are the project's.
pub fn enter() -> anyhow::Result<Option<PathBuf>> {
    // Resolved once, before the change: a relative root read again from
    // inside would name a folder a level deeper.
    let root = root();
    if let Some(root) = &root {
        std::fs::create_dir_all(root)
            .and_then(|()| std::env::set_current_dir(root))
            .map_err(|e| anyhow::anyhow!("entering the project {}: {e}", root.display()))?;
    }
    Ok(root.filter(|_| records_on()))
}

pub(crate) struct Project {
    root: PathBuf,
    name: String,
    version: String,
    /// The server's feed, which hears of each script block and notebook
    /// entry too.
    feed: Option<Arc<Feed>>,
}

/// Local date and time, `YYYY-MM-DD` and `HH:MM:SS`, and the offset from
/// UTC in seconds (UTC where the local time cannot be had).
fn local_now() -> (String, String, i64) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r only writes the struct it is given.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return (
                format!(
                    "{:04}-{:02}-{:02}",
                    tm.tm_year + 1900,
                    tm.tm_mon + 1,
                    tm.tm_mday
                ),
                format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
                // c_long: i64 on 64-bit targets, i32 on 32-bit ones.
                #[allow(clippy::useless_conversion)]
                i64::from(tm.tm_gmtoff),
            );
        }
    }
    let (d, t) = utc(secs);
    (d, t, 0)
}

/// Local date and time, `YYYY-MM-DD` and `HH:MM:SS`.
fn now() -> (String, String) {
    let (d, t, _) = local_now();
    (d, t)
}

/// Now, local, as RFC 3339 (`2026-10-20T12:03:33-07:00`).
pub(crate) fn rfc3339() -> String {
    let (d, t, off) = local_now();
    let sign = if off < 0 { '-' } else { '+' };
    let off = off.abs();
    format!("{d}T{t}{sign}{:02}:{:02}", off / 3600, off % 3600 / 60)
}

impl Project {
    pub(crate) fn new(root: PathBuf, name: &str, version: &str) -> Self {
        Self {
            root: crate::absolute(&root),
            name: name.into(),
            version: version.into(),
            feed: None,
        }
    }

    /// The project, telling `feed` of what it records.
    pub(crate) fn watched(mut self, feed: Option<Arc<Feed>>) -> Self {
        self.feed = feed;
        self
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
    /// A file someone else could have planted (not a regular file of this
    /// user's, or writable by others) is left alone: the records go to
    /// `{stem}-{pid}.{ext}` beside it instead.
    fn append(&self, path: &Path, mode: u32, head: impl FnOnce() -> String, text: &str) {
        let mut head = Some(head);
        let mut write = |path: &Path| -> std::io::Result<bool> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut open = OpenOptions::new();
            open.create_new(true).write(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut open, mode);
            match open.open(path) {
                Ok(mut f) => {
                    let head = head.take().map(|h| h()).unwrap_or_default();
                    f.write_all(format!("{head}{text}").as_bytes())?;
                    return Ok(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            let mut open = OpenOptions::new();
            open.append(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::custom_flags(&mut open, libc::O_NOFOLLOW);
            let Ok(mut f) = open.open(path) else {
                return Ok(false); // a symlink, or not ours to open
            };
            if !trusted(&f.metadata()?) {
                return Ok(false);
            }
            f.write_all(text.as_bytes())?;
            Ok(true)
        };
        let own = || {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let ext = path
                .extension()
                .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
            path.with_file_name(format!("{stem}-{}{ext}", std::process::id()))
        };
        let result = write(path).and_then(|done| {
            if done {
                return Ok(());
            }
            let other = own();
            log::warn!(
                "mcp records: {} is not this user's own file; writing {}",
                path.display(),
                other.display()
            );
            match write(&other)? {
                true => Ok(()),
                false => Err(std::io::Error::other("untrusted file")),
            }
        });
        if let Err(e) = result {
            log::warn!("mcp records {}: {e}", path.display());
        }
    }

    /// Append to today's `runall.sh`.
    pub(crate) fn script(&self, text: &str) {
        self.script_step(text, None);
    }

    /// Append to today's `runall.sh` a block that runs `command` (the bare
    /// command line, for followers that want it without the guard).
    pub(crate) fn script_step(&self, text: &str, command: Option<&str>) {
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
        if let Some(f) = &self.feed {
            f.emit(Body::Record {
                line: text.trim_matches('\n').into(),
                command: command.map(str::to_string),
            });
        }
    }

    /// The comment a tool call starts with.
    pub(crate) fn call(&self, tool: &str, args: &Value) {
        let args = self.rel(&args.to_string());
        let (name, version) = (&self.name, &self.version);
        self.script(&format!("\n# {} {name} {version} {tool} {args}\n", now().1));
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

    /// `cmd` as a shell line ([`Self::command`]), skipped when every one of
    /// `outputs` exists.
    #[cfg(test)]
    pub(crate) fn command_line(&self, cmd: &Command, outputs: &[PathBuf]) -> String {
        self.guarded(&self.command(cmd), outputs)
    }

    /// `cmd` as a shell line: its own directory and environment, this
    /// server's binary under the server's name, project paths relative.
    pub(crate) fn command(&self, cmd: &Command) -> String {
        let word = |s: &str| quote(&self.rel(s));
        let program = Path::new(cmd.get_program());
        let is_self = crate::own_exe().is_some_and(|e| e == program);
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
        let line = words.join(" ");
        match cmd.get_current_dir() {
            Some(dir) => format!("(cd {} && {line})", word(&dir.to_string_lossy())),
            None => line,
        }
    }

    /// `line`, skipped when every one of `outputs` exists and run with
    /// their folders made.
    pub(crate) fn guarded(&self, line: &str, outputs: &[PathBuf]) -> String {
        if outputs.is_empty() {
            return line.into();
        }
        let word = |s: &str| quote(&self.rel(s));
        let missing: Vec<String> = outputs
            .iter()
            .map(|o| format!("[ ! -e {} ]", word(&o.to_string_lossy())))
            .collect();
        let dirs = self
            .mkdirs(outputs.iter().map(PathBuf::as_path))
            .map_or(String::new(), |m| format!("{m}\n  "));
        format!("if {}; then\n  {dirs}{line}\nfi", missing.join(" || "))
    }

    /// `mkdir -p` of the folders `paths` go in, project-relative; none when
    /// they all go in the project itself.
    fn mkdirs<'a>(&self, paths: impl Iterator<Item = &'a Path>) -> Option<String> {
        let mut dirs: Vec<String> = paths
            .filter_map(|p| Some(self.rel(&p.parent()?.to_string_lossy())))
            .filter(|d| !d.is_empty() && d != "." && d != &*self.root.to_string_lossy())
            .map(|d| quote(&d))
            .collect();
        dirs.dedup();
        (!dirs.is_empty()).then(|| format!("mkdir -p {}", dirs.join(" ")))
    }

    /// A heredoc that writes `contents` to `path`: as it is when it is plain
    /// text (tabs and newlines), else base64, so no byte bash reads
    /// differently can end the heredoc early.
    pub(crate) fn file(&self, path: &Path, contents: &str) {
        let dir = self
            .mkdirs(std::iter::once(path))
            .map_or(String::new(), |m| format!("{m}\n"));
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
        if let Some(f) = &self.feed {
            f.emit(Body::Notebook {
                entry: kind.into(),
                text: text.into(),
            });
        }
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

/// A regular file of this user's that no one else can write.
#[cfg(unix)]
fn trusted(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    m.is_file() && m.uid() == me && m.mode() & 0o022 == 0
}

#[cfg(not(unix))]
fn trusted(m: &std::fs::Metadata) -> bool {
    m.is_file()
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

/// `secs` since 1970 in UTC, as [`now`] gives it.
fn utc(secs: u64) -> (String, String) {
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
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{:02}:{:02}:{:02}", rem / 3_600, rem % 3_600 / 60, rem % 60),
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
    fn planted_files_are_left_alone() {
        let dir = std::env::temp_dir().join(format!("legume-mcp-planted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = Project::new(dir.clone(), "t", "0");
        let target = dir.join("elsewhere.txt");
        std::fs::write(&target, "x\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dir.join("log.md")).unwrap();
        p.append(&dir.join("log.md"), 0o644, || "head\n".into(), "line\n");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "x\n",
            "a symlink is not followed"
        );
        let own = dir.join(format!("log-{}.md", std::process::id()));
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "head\nline\n");
        // Writable by others: left alone too.
        let open = dir.join("open.md");
        std::fs::write(&open, "o\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o666)).unwrap();
            p.append(&open, 0o644, String::new, "line\n");
            assert_eq!(std::fs::read_to_string(&open).unwrap(), "o\n");
        }
        // Our own file is appended to.
        p.append(&own, 0o644, String::new, "more\n");
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "head\nline\nmore\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dates_are_iso() {
        let (d, t) = now();
        assert_eq!((d.len(), t.len()), (10, 8), "{d} {t}");
        assert_eq!(
            utc(86_400 * 366 + 3_661),
            ("1971-01-02".into(), "01:01:01".into())
        );
    }
}
