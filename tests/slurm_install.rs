//! `scripts/slurm-install.sh`: the batch job it writes and submits, and
//! that job run here with stand-ins for `sbatch`, `cargo` and `rustup` on
//! PATH, so nothing is fetched or built.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/slurm-install.sh")
}

/// A folder of stand-in tools, first on PATH.
struct Bin {
    dir: tempfile::TempDir,
}

impl Bin {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn tool(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.dir.path().display())
    }
}

fn run(bin: &Bin, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(script())
        .args(args)
        .env("PATH", bin.path())
        .env_remove("SLURM_JOB_ID")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn dry_run_prints_the_job() {
    let bin = Bin::new();
    let prefix = tempfile::tempdir().unwrap();
    let p = prefix.path().to_str().unwrap();
    let o = run(
        &bin,
        &[
            "--dry-run",
            "--prefix",
            p,
            "--partition",
            "PART1",
            "--account",
            "ACCT1",
            "--cpus",
            "12",
            "--mem",
            "24",
            "--time",
            "6:00:00",
            "legume-mcp:benchmark",
            "pinto-rs@0.16.33",
        ],
    );
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    for want in [
        "#SBATCH --partition=PART1",
        "#SBATCH --account=ACCT1",
        "#SBATCH --cpus-per-task=12",
        "#SBATCH --mem=24G",
        "#SBATCH --time=6:00:00",
        "legume-mcp:benchmark",
        "pinto-rs@0.16.33",
    ] {
        assert!(t.contains(want), "missing {want:?} in\n{t}");
    }
    // Nothing is written or submitted on a dry run.
    assert_eq!(std::fs::read_dir(prefix.path()).unwrap().count(), 0);
}

#[test]
fn without_a_partition_none_is_asked() {
    let bin = Bin::new();
    let prefix = tempfile::tempdir().unwrap();
    let o = run(
        &bin,
        &["--dry-run", "--prefix", prefix.path().to_str().unwrap()],
    );
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(!t.contains("--partition"), "{t}");
    assert!(!t.contains("--account"), "{t}");
    // The default crates: the legume tools.
    for c in [
        "legume-mcp:benchmark",
        "pinto-rs",
        "lupin-rs",
        "senna-rs",
        "chickpea-rs",
        "faba",
        "data-beans",
    ] {
        assert!(t.contains(c), "{c} in {t}");
    }
}

#[test]
fn a_prefix_is_required_and_made_absolute() {
    let bin = Bin::new();
    let o = run(&bin, &["--dry-run"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("--prefix"), "{}", text(&o));

    let cwd = tempfile::tempdir().unwrap();
    let o = Command::new("bash")
        .arg(script())
        .args(["--dry-run", "--prefix", "tools"])
        .current_dir(cwd.path())
        .env("PATH", bin.path())
        .output()
        .unwrap();
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    let abs = cwd.path().canonicalize().unwrap().join("tools");
    assert!(t.contains(abs.to_str().unwrap()), "{t}");
}

#[test]
fn bad_options_are_refused() {
    let bin = Bin::new();
    let prefix = tempfile::tempdir().unwrap();
    let p = prefix.path().to_str().unwrap();
    for args in [
        vec!["--dry-run", "--prefix", p, "--cpus", "zero"],
        vec!["--dry-run", "--prefix", p, "--mem", "-1"],
        vec!["--dry-run", "--prefix", p, "--frobnicate"],
        vec!["--dry-run", "--prefix", p, "bad crate"],
    ] {
        let o = run(&bin, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o));
    }
}

#[test]
fn submit_writes_the_job_and_its_status() {
    let bin = Bin::new();
    bin.tool("sbatch", r#"echo "$@" > "$FAKE_SBATCH_ARGS"; echo 4242"#);
    let prefix = tempfile::tempdir().unwrap();
    let args_file = prefix.path().join("sbatch-args");
    let o = Command::new("bash")
        .arg(script())
        .args(["--prefix", prefix.path().to_str().unwrap()])
        .env("PATH", bin.path())
        .env("FAKE_SBATCH_ARGS", &args_file)
        .output()
        .unwrap();
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("4242"), "{t}");
    let runs: Vec<_> = std::fs::read_dir(prefix.path().join("install"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert!(run.join("install.sbatch").is_file());
    let status = std::fs::read_to_string(run.join("status")).unwrap();
    assert_eq!(status.trim(), "submitted 4242");
    let sent = std::fs::read_to_string(&args_file).unwrap();
    assert!(sent.contains("--parsable"), "{sent}");
    assert!(sent.contains("install.sbatch"), "{sent}");
}

/// The job, run here as Slurm would, with stand-ins: `cargo install`
/// writes a script named for the crate into `--root`/bin.
fn job_run(bin: &Bin, prefix: &Path, scratch: &Path, extra: &[&str]) -> (Output, PathBuf) {
    let mut args = vec![
        "--dry-run".to_string(),
        "--prefix".into(),
        prefix.to_str().unwrap().into(),
        "--build-tmp".into(),
        scratch.to_str().unwrap().into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let o = Command::new("bash")
        .arg(script())
        .args(&args)
        .env("PATH", bin.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let job = String::from_utf8(o.stdout).unwrap();
    let start = job.find("#!/bin/bash").expect("a job script");
    let file = prefix.join("job.sh");
    std::fs::write(&file, &job[start..]).unwrap();
    let run = prefix.join("install").join("RUN1");
    std::fs::create_dir_all(&run).unwrap();
    let o = Command::new("bash")
        .arg(&file)
        .env("PATH", bin.path())
        .env("SLURM_JOB_ID", "77")
        .env("SLURM_CPUS_PER_TASK", "3")
        .env("LEGUME_INSTALL_RUN", &run)
        .env("HOME", prefix)
        // Not the toolchain running this test (rustup sets these).
        .env_remove("CARGO_HOME")
        .env_remove("RUSTUP_HOME")
        .env_remove("RUSTUP_TOOLCHAIN")
        .output()
        .unwrap();
    (o, run)
}

fn fake_cargo(bin: &Bin) {
    // `cargo install ... --root R ... NAME`: R/bin/<bin of NAME>, which
    // prints a version; the arguments are kept for the test.
    bin.tool(
        "cargo",
        r#"
if [ "$1" = "--version" ]; then echo "cargo 1.95.0"; exit 0; fi
echo "$@" >> "$HOME/cargo-calls"
[ -n "$CARGO_TARGET_DIR" ] && mkdir -p "$CARGO_TARGET_DIR" && echo "$CARGO_TARGET_DIR" > "$HOME/target-dir"
root=""; prev=""; for a in "$@"; do [ "$prev" = "--root" ] && root="$a"; prev="$a"; done
name="$a"
case "$name" in legume-mcp) b=legume ;; pinto-rs) b=pinto ;; broken*) exit 101 ;; *) b="$name" ;; esac
mkdir -p "$root/bin"
printf '#!/bin/sh\necho "%s 9.9.9"\n' "$b" > "$root/bin/$b"
chmod +x "$root/bin/$b"
"#,
    );
    bin.tool("rustc", r#"echo "rustc 1.95.0""#);
}

#[test]
fn the_job_installs_each_crate_and_says_done() {
    let bin = Bin::new();
    fake_cargo(&bin);
    bin.tool("rustup", r#"echo "$@" >> "$HOME/rustup-calls""#);
    let prefix = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, run) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &["legume-mcp:benchmark", "pinto-rs@0.16.33"],
    );
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    let calls = std::fs::read_to_string(prefix.path().join("cargo-calls")).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(lines[0].contains("install --locked"), "{calls}");
    assert!(lines[0].contains("--features benchmark"), "{calls}");
    assert!(lines[0].contains("-j 3"), "{calls}");
    assert!(lines[0].ends_with("legume-mcp"), "{calls}");
    assert!(lines[1].contains("--version 0.16.33"), "{calls}");
    assert!(lines[1].ends_with("pinto-rs"), "{calls}");
    // The build ran in node scratch, which is removed after.
    let target = std::fs::read_to_string(prefix.path().join("target-dir")).unwrap();
    assert!(
        target.trim().starts_with(scratch.path().to_str().unwrap()),
        "{target}"
    );
    assert!(!Path::new(target.trim()).exists(), "scratch left behind");
    // The toolchain was brought up to date first.
    let rustup = std::fs::read_to_string(prefix.path().join("rustup-calls")).unwrap();
    assert!(rustup.contains("update stable"), "{rustup}");
    let status = std::fs::read_to_string(run.join("status")).unwrap();
    assert_eq!(status.trim(), "done");
    let versions = std::fs::read_to_string(run.join("versions.txt")).unwrap();
    assert!(versions.contains("legume 9.9.9"), "{versions}");
    assert!(versions.contains("pinto 9.9.9"), "{versions}");
    assert!(versions.contains("glibc"), "{versions}");
}

#[test]
fn no_rust_update_leaves_the_toolchain() {
    let bin = Bin::new();
    fake_cargo(&bin);
    bin.tool("rustup", r#"echo "$@" >> "$HOME/rustup-calls""#);
    let prefix = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, _) = job_run(&bin, prefix.path(), scratch.path(), &["--no-rust-update"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!prefix.path().join("rustup-calls").exists());
}

#[test]
fn a_failed_crate_fails_the_job_and_the_rest_install() {
    let bin = Bin::new();
    fake_cargo(&bin);
    let prefix = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, run) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &[
            "--no-rust-update",
            "broken",
            "legume-mcp:benchmark",
            "broken2",
        ],
    );
    assert!(!o.status.success(), "{}", text(&o));
    let status = std::fs::read_to_string(run.join("status")).unwrap();
    assert_eq!(status.trim(), "failed broken broken2");
    assert!(prefix.path().join("bin/legume").is_file());
    let versions = std::fs::read_to_string(run.join("versions.txt")).unwrap();
    assert!(versions.contains("legume 9.9.9"), "{versions}");
}
