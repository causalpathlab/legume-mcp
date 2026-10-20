//! The session script: a bash script that repeats what a session's tools
//! wrote. Each tool call is a comment (its name and arguments, read-only
//! calls included); each child a tool ran through [`crate::Call::run`] is
//! its command line, the server's own binary under the server's name; each
//! input a tool wrote through [`crate::Call::write_file`] is a heredoc. A
//! failed or cancelled child stays as a comment. The file is made on the
//! first call, so a session that calls nothing leaves none.

use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the script goes: `off`, or a file; unset, a new file under
/// [`DEFAULT_DIR`].
pub const ENV: &str = "LEGUME_MCP_JOURNAL";
/// Directory (under the server's working directory) of the default scripts.
pub const DEFAULT_DIR: &str = "mcp-journal";

pub(crate) struct Journal {
    path: PathBuf,
    name: String,
    version: String,
    started: bool,
}

impl Journal {
    pub(crate) fn new(path: PathBuf, name: &str, version: &str) -> Self {
        Self {
            path,
            name: name.into(),
            version: version.into(),
            started: false,
        }
    }

    /// Where [`ENV`] asks for the script: nowhere when `off`.
    pub(crate) fn path_from_env(name: &str) -> Option<PathBuf> {
        let path = match std::env::var(ENV) {
            Ok(v) if v.eq_ignore_ascii_case("off") => return None,
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let stamp = timestamp().replace([':', '-'], "");
                PathBuf::from(DEFAULT_DIR).join(format!("{name}-{stamp}-{}.sh", std::process::id()))
            }
        };
        Some(std::path::absolute(&path).unwrap_or(path))
    }

    /// Append `text`, starting the file first. The script is a record, not
    /// the work: a write that fails is logged, never the call's failure.
    pub(crate) fn append(&mut self, text: &str) {
        if let Err(e) = self.try_append(text) {
            log::warn!("mcp journal {}: {e}", self.path.display());
        }
    }

    fn try_append(&mut self, text: &str) -> std::io::Result<()> {
        let mut head = String::new();
        if !self.started {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let cwd = std::env::current_dir()?;
            head = format!(
                "#!/usr/bin/env bash\n\
                 # {} {} over MCP, session started {}.\n\
                 # Repeats what the session's tools wrote: `#` lines are the calls\n\
                 # (read-only ones too) and the steps that failed or were cancelled.\n\
                 set -euo pipefail\n\
                 cd {}\n",
                self.name,
                self.version,
                timestamp(),
                quote(&cwd.to_string_lossy())
            );
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        write!(f, "{head}{text}")?;
        self.started = true;
        Ok(())
    }

    /// The comment a tool call starts with.
    pub(crate) fn call(&mut self, tool: &str, args: &Value) {
        self.append(&format!("\n# {} {tool} {args}\n", timestamp()));
    }

    /// `cmd` as a shell line: its own directory and environment, and this
    /// server's binary under the server's name.
    pub(crate) fn command_line(&self, cmd: &Command) -> String {
        let program = Path::new(cmd.get_program());
        let own = std::env::current_exe().ok();
        let is_self =
            program == Path::new("/proc/self/exe") || own.as_deref().is_some_and(|e| e == program);
        let mut words: Vec<String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some(format!("{}={}", k.to_str()?, quote(&v?.to_string_lossy()))))
            .collect();
        words.push(if is_self {
            self.name.clone()
        } else {
            quote(&program.to_string_lossy())
        });
        words.extend(cmd.get_args().map(|a| quote(&a.to_string_lossy())));
        let line = words.join(" ");
        match cmd.get_current_dir() {
            Some(dir) => format!("(cd {} && {line})", quote(&dir.to_string_lossy())),
            None => line,
        }
    }

    /// A heredoc that writes `contents` to `path`.
    pub(crate) fn file(&mut self, path: &Path, contents: &str) {
        let mut tag = String::from("LEGUME_MCP_EOF");
        while contents.lines().any(|l| l == tag) {
            tag.push('_');
        }
        let nl = if contents.ends_with('\n') || contents.is_empty() {
            ""
        } else {
            "\n"
        };
        self.append(&format!(
            "cat > {} <<'{tag}'\n{contents}{nl}{tag}\n",
            quote(&path.to_string_lossy())
        ));
    }
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

/// Now, in UTC, as `YYYY-MM-DDTHH:MM:SSZ`.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
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
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
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
    fn timestamps_are_iso_utc() {
        let t = timestamp();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.starts_with("20") && t.ends_with('Z'), "{t}");
    }
}
