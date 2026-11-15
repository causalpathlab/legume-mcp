//! The tools a benchmark runs: each resolved through PATH, keyed by its
//! bytes, and copied to `tools/<content_key>/<basename>` in the
//! benchmark's folder, from where it runs. A reinstall cannot change the
//! bytes under a key, and two builds of one tool (CPU and CUDA) sit side
//! by side. `versions.tsv` says what each one is: its source, content
//! key, what runs, and `exec --version` (within [`VERSION_TIMEOUT`], the
//! tool's whole process group killed after; informational only).
//!
//! A later plan keeps a tool's snapshot when the installed one has
//! changed, and says so; `--retool` takes the installed one, rekeying its
//! tasks. `snapshot: false` runs the installed binary itself.

use super::key;
use super::plan::{base, LEGUME};
use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How long `--version` may take.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// `versions.tsv`'s header.
pub const VERSIONS_HEAD: &str = "tool\tsource\tcontent_key\texec\tversion";

/// A tool found: its word, where it is, and whether it is snapshot.
#[derive(Clone, Debug)]
pub struct Found {
    pub word: String,
    pub source: PathBuf,
    pub snapshot: bool,
}

/// A tool as the plan runs it, and as `plan.json` lists it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Resolved {
    #[serde(rename = "tool")]
    pub word: String,
    pub source: PathBuf,
    pub content_key: String,
    /// The snapshot, or the source with `snapshot: false`.
    pub exec: PathBuf,
    /// In `versions.tsv`, not `plan.json`.
    #[serde(skip)]
    pub version: Option<String>,
}

/// An executable file.
fn executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    meta.is_file()
}

/// `word` as a program: a path from the project's folder `root` when it
/// has a `/` (as an input's path is), else the first executable of that
/// name in an absolute folder of `path`. Absolute.
pub fn which(word: &str, path: Option<&OsStr>, root: &Path) -> Option<PathBuf> {
    if word.contains('/') {
        let p = crate::absolute(&root.join(word));
        return executable(&p).then_some(p);
    }
    crate::on_path_in(word, path?, executable)
}

/// Each of `words` (word, snapshot, where in the spec) found on `path` or
/// from `root` ([`which`]), [`LEGUME`] as `legume`; and those not found,
/// with where they are named.
pub fn find(
    words: &[(String, bool, String)],
    path: Option<&OsStr>,
    legume: &Path,
    root: &Path,
) -> (Vec<Found>, Vec<(String, String)>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for (word, snapshot, at) in words {
        let source = if word == LEGUME {
            Some(legume.to_path_buf())
        } else {
            which(word, path, root)
        };
        match source {
            Some(source) => found.push(Found {
                word: word.clone(),
                source,
                snapshot: *snapshot,
            }),
            None => missing.push((word.clone(), at.clone())),
        }
    }
    (found, missing)
}

/// The name a tool's snapshot takes: its word's last part, or `legume`.
fn basename(f: &Found) -> &str {
    if f.word == LEGUME {
        "legume"
    } else {
        base(&f.word)
    }
}

/// `source`, whose bytes have `content_key`, copied to
/// `dir/tools/<content_key>/<name>` unless it is there already; that path.
pub fn snapshot(dir: &Path, source: &Path, content_key: &str, name: &str) -> Result<PathBuf> {
    let folder = dir.join("tools").join(content_key);
    let target = folder.join(name);
    if target.is_file() {
        return Ok(target);
    }
    std::fs::create_dir_all(&folder).with_context(|| format!("making {}", folder.display()))?;
    let tmp = folder.join(format!(".{name}.{}", std::process::id()));
    let copied = (|| {
        std::fs::copy(source, &tmp)
            .with_context(|| format!("copying {} to {}", source.display(), tmp.display()))?;
        if key::file_key(&tmp)? != content_key {
            bail!(
                "{} changed while it was copied; plan again",
                source.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("setting permissions on {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &target).with_context(|| format!("placing {}", target.display()))
    })();
    if copied.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    copied.map(|()| target)
}

/// The first line `exec --version` prints, when it exits 0 within
/// `timeout`; its process group is killed either way. A tool still open
/// for writing (just copied, its file held a moment by a child another
/// thread forked) is started again until it can be, within `timeout`.
pub fn version(exec: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let out = loop {
        let mut cmd = Command::new(exec);
        cmd.arg("--version");
        match crate::run_child(cmd, &|| Instant::now() >= deadline, &|_| {}) {
            Err(e) if is_busy(&e) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            ran => break ran.ok()?,
        }
    };
    let line = out.lines().find(|l| !l.trim().is_empty())?;
    Some(
        line.trim()
            .replace(['\t', '\r'], " ")
            .chars()
            .take(200)
            .collect(),
    )
}

/// Whether `e` is a program that could not start while open for writing.
fn is_busy(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ExecutableFileBusy)
    })
}

/// A row of a `versions.tsv`.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub tool: String,
    pub source: String,
    pub content_key: String,
    pub exec: String,
}

/// The rows of `dir/versions.tsv`; none when it is not there.
pub fn read_versions(dir: &Path) -> Vec<Row> {
    let Ok(text) = std::fs::read_to_string(dir.join("versions.tsv")) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 4).then(|| Row {
                tool: f[0].into(),
                source: f[1].into(),
                content_key: f[2].into(),
                exec: f[3].into(),
            })
        })
        .collect()
}

/// `versions.tsv` for `tools`.
pub fn versions_tsv(tools: &[Resolved]) -> String {
    let mut s = format!("{VERSIONS_HEAD}\n");
    for t in tools {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            t.word,
            t.source.display(),
            t.content_key,
            t.exec.display(),
            t.version.as_deref().unwrap_or("NA")
        ));
    }
    s
}

/// The first 16 hex digits of a key, as shown.
pub fn short(key: &str) -> &str {
    &key[..key.len().min(16)]
}

/// Each found tool keyed, snapshot into `dir` and asked its version
/// (within [`VERSION_TIMEOUT`]), as the plan runs it; with what changed since the plan before (its
/// `versions.tsv`). A changed tool keeps its snapshot unless `retool`.
pub fn settle(dir: &Path, found: &[Found], retool: bool) -> Result<(Vec<Resolved>, Vec<String>)> {
    let before = read_versions(dir);
    let mut tools = Vec::new();
    let mut drift = Vec::new();
    for f in found {
        let now = key::file_key(&f.source)?;
        let old = before.iter().find(|r| r.tool == f.word);
        if let Some(old) = old.filter(|o| o.source != f.source.display().to_string()) {
            drift.push(format!(
                "{}: now {}, was {}",
                f.word,
                f.source.display(),
                old.source
            ));
        }
        let changed = old.filter(|o| o.content_key != now);
        let kept = changed.filter(|o| {
            f.snapshot
                && !retool
                && Path::new(&o.exec).starts_with(dir)
                && Path::new(&o.exec).is_file()
        });
        if let Some(o) = changed {
            let (from, to) = (short(&o.content_key), short(&now));
            let how = if kept.is_some() {
                "planned with its snapshot, --retool plans with the one installed"
            } else if f.snapshot {
                "planned with the one installed, which rekeys its tasks"
            } else {
                "snapshot false, so its tasks are rekeyed"
            };
            drift.push(format!(
                "{}: changed since the last plan ({from} -> {to}); {how}",
                f.word
            ));
        }
        let (content_key, exec) = match kept {
            Some(o) => (o.content_key.clone(), PathBuf::from(&o.exec)),
            None if f.snapshot => {
                let exec = snapshot(dir, &f.source, &now, basename(f))?;
                (now, exec)
            }
            None => (now, f.source.clone()),
        };
        let version = version(&exec, VERSION_TIMEOUT);
        tools.push(Resolved {
            word: f.word.clone(),
            source: f.source.clone(),
            content_key,
            exec,
            version,
        });
    }
    Ok((tools, drift))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// An executable shell script `name` in `dir` running `body`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// TOOL1 found at `source`, snapshot.
    fn found(source: PathBuf) -> [Found; 1] {
        [Found {
            word: "TOOL1".into(),
            source,
            snapshot: true,
        }]
    }

    #[test]
    fn a_word_resolves_through_path_and_a_path_as_itself() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("TOOL1"), "not executable").unwrap();
        let t = script(&b, "TOOL1", "true");
        let path = std::env::join_paths([&a, &b]).unwrap();
        let root = Path::new("/R");
        assert_eq!(which("TOOL1", Some(&path), root), Some(t.clone()));
        assert_eq!(which("TOOL2", Some(&path), root), None);
        assert_eq!(which(&t.display().to_string(), None, root), Some(t.clone()));
        assert_eq!(
            which(&a.join("TOOL1").display().to_string(), None, root),
            None
        );
        // A path is from the project, as an input's is, wherever legume
        // runs from.
        assert_eq!(which("b/TOOL1", None, d.path()), Some(t));
        // A relative folder of PATH is passed over, so the project's own
        // files are never taken for a program.
        let up = std::env::current_dir().unwrap().components().count() - 1;
        let rel = Path::new(&"../".repeat(up)).join(b.strip_prefix("/").unwrap());
        assert!(rel.is_relative() && rel.join("TOOL1").is_file());
        assert_eq!(which("TOOL1", Some(rel.as_os_str()), root), None);
    }

    // no_version_still_keys (the tool side): a tool that fails --version,
    // or hangs in it with a child holding its stdout, has no version, and
    // is stopped within the timeout, its whole group killed.
    #[test]
    fn a_version_is_the_first_line_and_a_hang_is_killed() {
        let d = tempfile::tempdir().unwrap();
        let ok = script(d.path(), "ok", "echo; echo 'TOOL1 1.2.3'; echo more");
        assert_eq!(
            version(&ok, VERSION_TIMEOUT).as_deref(),
            Some("TOOL1 1.2.3")
        );
        let fails = script(d.path(), "fails", "echo 'usage'; exit 2");
        assert_eq!(version(&fails, VERSION_TIMEOUT), None);
        let pid = d.path().join("pid");
        let hangs = script(
            d.path(),
            "hangs",
            &format!("sleep 30 & echo $! > {}; echo TOOL1; wait", pid.display()),
        );
        let start = Instant::now();
        assert_eq!(version(&hangs, Duration::from_millis(500)), None);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
        let child: i32 = std::fs::read_to_string(&pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: signal 0 only asks whether the process is there.
        let alive = unsafe { libc::kill(child, 0) } == 0;
        assert!(!alive, "the hung tool's child {child} was left running");
    }

    // A tool still open for writing somewhere (a copy just made, its file
    // held by a child another thread forked) cannot start yet: it is
    // asked again until it can, within the timeout.
    #[test]
    fn a_tool_busy_being_written_is_asked_again() {
        let d = tempfile::tempdir().unwrap();
        let t = script(d.path(), "TOOL1", "echo 'TOOL1 1.2.3'");
        let held = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        assert_eq!(version(&t, VERSION_TIMEOUT).as_deref(), Some("TOOL1 1.2.3"));
        closer.join().unwrap();
    }

    // rebuild_mid_benchmark_keeps_snapshot (the tool side): a snapshot is
    // keyed by the bytes; a rebuild leaves the snapshot's bytes alone and
    // the next plan keeps it, saying so, unless retool.
    #[test]
    fn a_snapshot_keeps_its_bytes_through_a_rebuild() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        let bench = d.path().join("bench");
        std::fs::create_dir_all(&bin).unwrap();
        let t = script(&bin, "TOOL1", "echo v1");
        let found = found(t.clone());
        let (first, drift) = settle(&bench, &found, false).unwrap();
        assert!(drift.is_empty(), "{drift:?}");
        let k1 = key::file_key(&t).unwrap();
        assert_eq!(first[0].content_key, k1);
        assert_eq!(first[0].exec, bench.join("tools").join(&k1).join("TOOL1"));
        assert_eq!(first[0].version.as_deref(), Some("v1"));
        std::fs::write(bench.join("versions.tsv"), versions_tsv(&first)).unwrap();

        script(&bin, "TOOL1", "echo v2");
        let (kept, drift) = settle(&bench, &found, false).unwrap();
        assert_eq!(kept[0].exec, first[0].exec);
        assert_eq!(kept[0].content_key, k1);
        assert_eq!(kept[0].version.as_deref(), Some("v1"));
        assert_eq!(drift.len(), 1, "{drift:?}");
        assert!(
            drift[0].starts_with("TOOL1: changed since the last plan ("),
            "{drift:?}"
        );
        assert!(
            drift[0].ends_with("planned with its snapshot, --retool plans with the one installed")
        );

        let (retooled, _) = settle(&bench, &found, true).unwrap();
        assert_ne!(retooled[0].content_key, k1);
        assert_eq!(retooled[0].version.as_deref(), Some("v2"));
        assert_eq!(
            std::fs::read_to_string(&first[0].exec).unwrap(),
            "#!/bin/sh\necho v1\n"
        );
    }

    // A changed tool keeps only a snapshot that is still a file in the
    // benchmark's folder: one deleted, or one versions.tsv names outside
    // it, gives way to a new snapshot of the one installed, which rekeys.
    #[test]
    fn a_lost_or_outside_snapshot_is_not_kept() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        let bench = d.path().join("bench");
        std::fs::create_dir_all(&bin).unwrap();
        let t = script(&bin, "TOOL1", "echo v1");
        let found = found(t.clone());
        let (first, _) = settle(&bench, &found, false).unwrap();
        script(&bin, "TOOL1", "echo v2");
        let k2 = key::file_key(&t).unwrap();
        let fresh = bench.join("tools").join(&k2).join("TOOL1");
        let rekeyed = "planned with the one installed, which rekeys its tasks";

        std::fs::remove_dir_all(bench.join("tools")).unwrap();
        std::fs::write(bench.join("versions.tsv"), versions_tsv(&first)).unwrap();
        let (lost, drift) = settle(&bench, &found, false).unwrap();
        assert_eq!((&lost[0].content_key, &lost[0].exec), (&k2, &fresh));
        assert!(drift[0].ends_with(rekeyed), "{drift:?}");

        std::fs::remove_dir_all(bench.join("tools")).unwrap();
        let outside = script(d.path(), "TOOL1-old", "echo v1");
        let mut row = first.clone();
        row[0].exec = outside;
        std::fs::write(bench.join("versions.tsv"), versions_tsv(&row)).unwrap();
        let (moved, drift) = settle(&bench, &found, false).unwrap();
        assert_eq!((&moved[0].content_key, &moved[0].exec), (&k2, &fresh));
        assert!(drift[0].ends_with(rekeyed), "{drift:?}");
    }

    // A tool found somewhere else than the plan before found it is said,
    // though its bytes are the same.
    #[test]
    fn a_tool_found_elsewhere_is_said() {
        let d = tempfile::tempdir().unwrap();
        let bench = d.path().join("bench");
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (ta, tb) = (script(&a, "TOOL1", "true"), script(&b, "TOOL1", "true"));
        let (first, _) = settle(&bench, &found(ta.clone()), false).unwrap();
        std::fs::write(bench.join("versions.tsv"), versions_tsv(&first)).unwrap();
        let (_, drift) = settle(&bench, &found(tb.clone()), false).unwrap();
        assert_eq!(
            drift,
            [format!("TOOL1: now {}, was {}", tb.display(), ta.display())]
        );
    }

    // A tool whose bytes are not its key once copied (rebuilt meanwhile)
    // is refused, and leaves no copy behind.
    #[test]
    fn a_tool_changed_while_copied_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let t = script(d.path(), "TOOL1", "true");
        let k = "0".repeat(32);
        let e = snapshot(&d.path().join("bench"), &t, &k, "TOOL1").unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("{} changed while it was copied; plan again", t.display())
        );
        let folder = d.path().join("bench/tools").join(&k);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    }
}
