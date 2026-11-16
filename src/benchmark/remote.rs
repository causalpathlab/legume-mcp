//! Where a benchmark runs when it is not here: a remote, a cluster this
//! machine reaches over SSH, and the [`Shell`] its commands run in.
//!
//! Remotes are kept in `remotes.toml` in this person's config folder,
//! never in a project: each is a section named for the remote, with the
//! SSH target (a host or an alias from `~/.ssh/config`, using this
//! person's own keys; no password is kept), the folder on the cluster
//! the benchmarks go in, the folder of legume's tools there (an install's
//! `bin`, when they are not on PATH), and the Slurm partition and account,
//! when the cluster wants them:
//!
//! ```toml
//! [R1]
//! ssh = "HOST1"
//! folder = "/path/on/the/cluster"
//! bin = "/path/on/the/cluster/legume/bin"
//! partition = "PART1"
//! account = "ACCT1"
//! ```

use crate::command::refuse;
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A cluster this machine reaches over SSH.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Remote {
    pub name: String,
    /// The SSH target: a host, `user@host`, or an alias.
    pub ssh: String,
    /// The folder on the cluster that benchmarks go in.
    pub folder: String,
    /// The folder of legume's tools on the cluster; `None` when they are
    /// on its PATH.
    pub bin: Option<String>,
    pub partition: Option<String>,
    pub account: Option<String>,
}

impl Remote {
    /// The install its bin is the bin of, when bin is `PREFIX/bin`, as
    /// `legume benchmark install` makes it.
    pub fn prefix(&self) -> Option<&str> {
        self.bin
            .as_deref()
            .map(|b| b.trim_end_matches('/'))
            .and_then(|b| b.strip_suffix("/bin"))
            .filter(|p| !p.is_empty())
    }
}

/// `remotes.toml`: `$LEGUME_REMOTES`, or in `$XDG_CONFIG_HOME/legume`,
/// or in `~/.config/legume`.
pub fn config_file() -> Option<PathBuf> {
    if let Some(f) = std::env::var_os("LEGUME_REMOTES").filter(|f| !f.is_empty()) {
        return Some(PathBuf::from(f));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(config.join("legume").join("remotes.toml"))
}

/// What a question to [`ask`] wants.
enum Answer {
    Needed,
    Optional,
    NeededPath,
    OptionalPath,
}

/// What a remote's folders must be.
const NOT_ABSOLUTE: &str = "an absolute path, as the cluster has it (no ~)";

/// Whether `p` is a path the cluster reads as it is.
fn absolute(p: &str) -> bool {
    p.starts_with('/')
}

/// Remote `name`, from `remotes.toml`, or asked for on stdin and kept.
pub fn named(name: &str, out: &mut dyn Write) -> Result<Remote> {
    let file =
        config_file().ok_or_else(|| refuse("no config folder: set HOME, or LEGUME_REMOTES"))?;
    if let Some(r) = read(&file)?.into_iter().find(|r| r.name == name) {
        return Ok(r);
    }
    let r = ask(name, &mut std::io::stdin().lock(), out)?;
    save(&file, &r)?;
    writeln!(out, "kept remote {name} in {}", file.display())?;
    Ok(r)
}

/// The remotes in `text`, a `remotes.toml`: sections of `key = "value"`
/// lines, with `#` comments. An unknown key, or a section without `ssh`
/// or `folder`, is refused.
pub fn parse(text: &str) -> Result<Vec<Remote>> {
    let mut remotes: Vec<Remote> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let at = || format!("remotes.toml line {}", n + 1);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim();
            check_name(name).with_context(at)?;
            remotes.push(Remote {
                name: name.to_string(),
                ..Remote::default()
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("{}: not a [section] or key = \"value\"", at());
        };
        let value = unquote(value.trim()).with_context(at)?;
        let Some(r) = remotes.last_mut() else {
            bail!("{}: {} before any [section]", at(), key.trim());
        };
        match key.trim() {
            "ssh" => r.ssh = value,
            "folder" => r.folder = value,
            "bin" => r.bin = Some(value),
            "partition" => r.partition = Some(value),
            "account" => r.account = Some(value),
            k => bail!("{}: unknown key {k}", at()),
        }
    }
    for r in &remotes {
        if r.ssh.is_empty() || r.folder.is_empty() {
            bail!("remotes.toml [{}]: needs ssh and folder", r.name);
        }
        for p in std::iter::once(&r.folder).chain(&r.bin) {
            if !absolute(p) {
                bail!("remotes.toml [{}]: {p} is not {NOT_ABSOLUTE}", r.name);
            }
        }
    }
    Ok(remotes)
}

/// A remote's name: letters, digits, `-`, `_` and `.`.
fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(refuse(format!(
            "remote name {name:?}: letters, digits, -, _ and . only"
        )))
    }
}

/// A TOML basic string's value: `"..."` with `\"` and `\\`, then
/// nothing but a `#` comment.
fn unquote(v: &str) -> Result<String> {
    let Some(inner) = v.strip_prefix('"') else {
        bail!("a value is in double quotes, not {v}");
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, e @ ('"' | '\\'))) => out.push(e),
                other => bail!("unknown escape \\{}", other.map_or(' ', |o| o.1)),
            },
            '"' => {
                let rest = inner[at + 1..].trim();
                if rest.is_empty() || rest.starts_with('#') {
                    return Ok(out);
                }
                bail!("after a value's closing quote, {rest}");
            }
            c => out.push(c),
        }
    }
    bail!("a value is in double quotes, not {v}")
}

fn quote_toml(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `remotes` as `remotes.toml`.
pub fn to_toml(remotes: &[Remote]) -> String {
    let mut out = String::new();
    for r in remotes {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("[{}]\n", r.name));
        out.push_str(&format!("ssh = {}\n", quote_toml(&r.ssh)));
        out.push_str(&format!("folder = {}\n", quote_toml(&r.folder)));
        for (key, v) in [
            ("bin", &r.bin),
            ("partition", &r.partition),
            ("account", &r.account),
        ] {
            if let Some(v) = v {
                out.push_str(&format!("{key} = {}\n", quote_toml(v)));
            }
        }
    }
    out
}

/// The remotes in `file`; none when it is not there.
pub fn read(file: &Path) -> Result<Vec<Remote>> {
    match std::fs::read_to_string(file) {
        Ok(text) => parse(&text).with_context(|| file.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

/// `remote` saved in `file`, in place of one of its name.
pub fn save(file: &Path, remote: &Remote) -> Result<()> {
    let mut remotes = read(file)?;
    match remotes.iter_mut().find(|r| r.name == remote.name) {
        Some(r) => *r = remote.clone(),
        None => remotes.push(remote.clone()),
    }
    let dir = file.parent().context("remotes.toml's folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let name = file.file_name().context("remotes.toml's name")?;
    crate::job::replace(dir, &name.to_string_lossy(), to_toml(&remotes).as_bytes())
        .with_context(|| format!("writing {}", file.display()))
}

/// Remote `name` asked of a person on `input`, each question said on
/// `out`: its SSH target and cluster folder, then a partition and an
/// account, which may be left empty.
pub fn ask(name: &str, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Remote> {
    check_name(name)?;
    writeln!(
        out,
        "Remote {name} is new. legume reaches it with ssh, as you do, and keeps what you \
         say here in your own config, never in a project."
    )?;
    // An answer, empty only when it is not needed, and an absolute path
    // when it is one.
    let mut answer = |question: &str, kind: Answer| -> Result<String> {
        let needed = matches!(kind, Answer::Needed | Answer::NeededPath);
        let path = matches!(kind, Answer::NeededPath | Answer::OptionalPath);
        loop {
            write!(out, "{question}: ")?;
            out.flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 {
                return Err(refuse(format!("remote {name}: no answer to {question}")));
            }
            let line = line.trim();
            if path && !line.is_empty() && !absolute(line) {
                writeln!(out, "Wanted: {NOT_ABSOLUTE}.")?;
            } else if !line.is_empty() || !needed {
                return Ok(line.to_string());
            }
        }
    };
    let ssh = answer(
        "SSH target (a host or an alias from ~/.ssh/config)",
        Answer::Needed,
    )?;
    let folder = answer("Folder on the cluster for benchmarks", Answer::NeededPath)?;
    let some = |s: String| Some(s).filter(|s| !s.is_empty());
    let bin = some(answer(
        "Folder of legume's tools on the cluster, an install's bin (empty if on PATH)",
        Answer::OptionalPath,
    )?);
    let partition = some(answer(
        "Slurm partition (empty for the default)",
        Answer::Optional,
    )?);
    let account = some(answer("Slurm account (empty for none)", Answer::Optional)?);
    let r = Remote {
        name: name.to_string(),
        ssh,
        folder,
        bin,
        partition,
        account,
    };
    if r.bin.is_some() && r.prefix().is_none() {
        writeln!(
            out,
            "Runs find legume there; legume benchmark install wants PREFIX/bin, to install into."
        )?;
    }
    Ok(r)
}

/// Where a benchmark's commands run: this machine, or a remote over one
/// SSH connection that every call shares (OpenSSH's ControlMaster, kept
/// 10 minutes after the last).
#[derive(Clone)]
pub struct Shell {
    /// The SSH target and the shared connection's socket; `None` for this
    /// machine.
    ssh: Option<(String, String)>,
    /// Environment the shell, or ssh, starts with, over this process's.
    env: Vec<(OsString, OsString)>,
}

/// What a script printed and how it exited.
pub struct Ran {
    pub stdout: String,
    pub stderr: String,
    /// `None` when a signal ended it.
    pub code: Option<i32>,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

impl Shell {
    /// This machine.
    pub fn local() -> Shell {
        Shell {
            ssh: None,
            env: Vec::new(),
        }
    }

    /// The remote at SSH `target`.
    pub fn ssh(target: &str) -> Shell {
        // The socket's path is kept short: a Unix socket's is at most
        // about 100 bytes. %C is OpenSSH's hash of the connection.
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        Shell {
            ssh: Some((target.to_string(), format!("/tmp/legume-{uid}-%C"))),
            env: Vec::new(),
        }
    }

    /// The shell with `key` set to `value` where it starts (on this
    /// machine: where ssh starts).
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Shell {
        self.env.push((key.into(), value.into()));
        self
    }

    /// ssh over the shared connection at `control`, its options given.
    fn ssh_command(&self, control: &str) -> Command {
        let mut c = Command::new("ssh");
        c.envs(self.env.iter().map(|(k, v)| (k, v)))
            .arg("-o")
            .arg(format!("ControlPath={control}"));
        c
    }

    fn command(&self) -> Command {
        let mut c = match &self.ssh {
            None => {
                let mut c = Command::new("bash");
                c.arg("-s").envs(self.env.iter().map(|(k, v)| (k, v)));
                c
            }
            Some((target, control)) => {
                let mut c = self.ssh_command(control);
                c.args([
                    "-o",
                    "ControlMaster=auto",
                    "-o",
                    "ControlPersist=10m",
                    "-o",
                    "ServerAliveInterval=60",
                    "--",
                    target,
                    "bash",
                    "-s",
                ]);
                c
            }
        };
        // A group of its own: Ctrl-C at the terminal is the runner's to
        // act on, not the end of the call it is making.
        std::os::unix::process::CommandExt::process_group(&mut c, 0);
        c
    }

    /// `script` run by bash, fed on its stdin: what it printed and how it
    /// exited. Only a shell that cannot start (no bash, no ssh) is an
    /// error; ssh exits 255 when it cannot connect.
    pub fn run(&self, script: &str) -> Result<Ran> {
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| match &self.ssh {
                None => "starting bash".to_string(),
                Some((t, _)) => format!("starting ssh to {t}"),
            })?;
        let mut stdin = child.stdin.take().context("a shell's stdin")?;
        // Fed from a thread, so a long script never waits on output that
        // fills its pipe. A shell that exits before reading all of it
        // ends the feed with a broken pipe: its exit says why.
        let Output {
            status,
            stdout,
            stderr,
        } = std::thread::scope(|scope| {
            scope.spawn(move || {
                let _ = stdin.write_all(script.as_bytes());
            });
            child.wait_with_output()
        })?;
        Ok(Ran {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            code: status.code(),
        })
    }

    /// `script` run, refused unless it exits 0: its stdout.
    pub fn check(&self, script: &str, what: &str) -> Result<String> {
        let ran = self.run(script)?;
        if !ran.ok() {
            let to = self
                .ssh
                .as_ref()
                .map_or(String::new(), |(t, _)| format!(" on {t}"));
            bail!(
                "{what}{to}: exit {}: {}",
                ran.code.map_or("by a signal".into(), |c| c.to_string()),
                ran.stderr.trim()
            );
        }
        Ok(ran.stdout)
    }

    /// The shared SSH connection closed; nothing for this machine.
    pub fn close(&self) {
        if let Some((target, control)) = &self.ssh {
            let _ = self
                .ssh_command(control)
                .args(["-O", "exit", "--", target])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r1() -> Remote {
        Remote {
            name: "R1".into(),
            ssh: "HOST1".into(),
            folder: "/a/b \"c\"".into(),
            bin: Some("/a/bin".into()),
            partition: Some("PART1".into()),
            account: None,
        }
    }

    #[test]
    fn remotes_round_trip_through_toml() {
        let text = to_toml(&[
            r1(),
            Remote {
                name: "R2".into(),
                ssh: "u@h".into(),
                folder: "/x".into(),
                bin: None,
                partition: None,
                account: Some("ACCT1".into()),
            },
        ]);
        let back = parse(&text).unwrap();
        assert_eq!(back[0], r1());
        assert_eq!(back[1].account.as_deref(), Some("ACCT1"));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let r = parse("# mine\n\n[R1]\nssh = \"HOST1\"  # a host\nfolder = \"/x\"\n").unwrap();
        assert_eq!((r[0].ssh.as_str(), r[0].folder.as_str()), ("HOST1", "/x"));
    }

    #[test]
    fn bad_remotes_are_refused() {
        for (text, says) in [
            ("[R1]\nssh = \"h\"\n", "needs ssh and folder"),
            (
                "[R1]\nssh = \"h\"\nfolder = \"~/x\"\n",
                "is not an absolute path",
            ),
            (
                "[R1]\nssh = \"h\"\nfolder = \"/x\"\nport = \"22\"\n",
                "unknown key port",
            ),
            ("ssh = \"h\"\n", "before any [section]"),
            ("[R1]\nssh = h\n", "double quotes"),
            ("[R1]\nssh = \"h\" x\n", "after a value's closing quote"),
            ("[R 1]\n", "letters, digits"),
        ] {
            let e = format!("{:#}", parse(text).unwrap_err());
            assert!(e.contains(says), "{text:?}: {e}");
        }
    }

    #[test]
    fn a_bin_outside_an_install_is_kept_and_said() {
        let mut input: &[u8] = b"HOST1\n/x\n/x/tools\n\n\n";
        let mut said = Vec::new();
        let r = ask("R1", &mut input, &mut said).unwrap();
        assert_eq!((r.bin.as_deref(), r.prefix()), (Some("/x/tools"), None));
        assert!(String::from_utf8(said)
            .unwrap()
            .contains("wants PREFIX/bin"));
        let r = Remote {
            bin: Some("/x/bin/".into()),
            ..r
        };
        assert_eq!(r.prefix(), Some("/x"));
    }

    #[test]
    fn asking_repeats_until_a_needed_answer_and_leaves_optional_ones_empty() {
        let mut input: &[u8] = b"\nHOST1\n~/x\n/x\n\n\nACCT1\n";
        let mut said = Vec::new();
        let r = ask("R1", &mut input, &mut said).unwrap();
        assert_eq!(
            r,
            Remote {
                name: "R1".into(),
                ssh: "HOST1".into(),
                folder: "/x".into(),
                bin: None,
                partition: None,
                account: Some("ACCT1".into()),
            }
        );
        let said = String::from_utf8(said).unwrap();
        assert_eq!(said.matches("SSH target").count(), 2, "{said}");
        assert_eq!(said.matches("Folder on the cluster").count(), 2, "{said}");
    }

    #[test]
    fn no_answer_is_refused() {
        let mut input: &[u8] = b"HOST1\n";
        let e = ask("R1", &mut input, &mut Vec::new()).unwrap_err();
        assert_eq!(crate::command::code(&e), 3);
    }

    #[test]
    fn save_replaces_a_remote_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("legume").join("remotes.toml");
        save(&file, &r1()).unwrap();
        let mut again = r1();
        again.ssh = "HOST2".into();
        save(&file, &again).unwrap();
        let all = read(&file).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].ssh, "HOST2");
    }

    #[test]
    fn local_shell_runs_a_script_with_its_env() {
        let sh = Shell::local().with_env("V1", "x y");
        let ran = sh.run("echo \"$V1\"; echo err >&2; exit 3").unwrap();
        assert_eq!(
            (ran.stdout.as_str(), ran.stderr.trim(), ran.code),
            ("x y\n", "err", Some(3))
        );
        let e = sh
            .check("echo no >&2; exit 4", "M1")
            .unwrap_err()
            .to_string();
        assert!(e.contains("M1: exit 4: no"), "{e}");
    }
}
