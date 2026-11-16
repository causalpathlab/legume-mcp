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

/// The script run by bash with `bin`'s stand-ins first on PATH.
fn script_cmd(bin: &Bin) -> Command {
    let mut c = Command::new("bash");
    c.arg(script()).env("PATH", bin.path());
    c
}

fn run(bin: &Bin, args: &[&str]) -> Output {
    script_cmd(bin).args(args).output().unwrap()
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
    let o = script_cmd(&bin)
        .args(["--dry-run", "--prefix", "tools"])
        .current_dir(cwd.path())
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
    // The status as sbatch is called: the job may start before it returns.
    bin.tool(
        "sbatch",
        r#"echo "$@" > "$FAKE_SBATCH_ARGS"
for a in "$@"; do last="$a"; done
cat "${last%/*}/status" > "$FAKE_SBATCH_ARGS.status"
echo 4242"#,
    );
    let prefix = tempfile::tempdir().unwrap();
    let args_file = prefix.path().join("sbatch-args");
    let o = script_cmd(&bin)
        .args(["--prefix", prefix.path().to_str().unwrap()])
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
    assert_eq!(status.trim(), "submitted");
    let at_sbatch = std::fs::read_to_string(prefix.path().join("sbatch-args.status")).unwrap();
    assert_eq!(at_sbatch.trim(), "submitted", "written before sbatch");
    let jid = std::fs::read_to_string(run.join("job_id")).unwrap();
    assert_eq!(jid.trim(), "4242");
    let sent = std::fs::read_to_string(&args_file).unwrap();
    assert!(sent.contains("--parsable"), "{sent}");
    assert!(sent.contains("install.sbatch"), "{sent}");
    // The run folder is in the job, not passed through --export.
    assert!(!sent.contains("--export"), "{sent}");
    let job = std::fs::read_to_string(run.join("install.sbatch")).unwrap();
    assert!(job.contains(run.to_str().unwrap()), "{job}");
}

#[test]
fn a_prefix_with_a_comma_reaches_the_job_whole() {
    let bin = Bin::new();
    bin.tool("sbatch", "echo 4242");
    let top = tempfile::tempdir().unwrap();
    let prefix = top.path().join("A1,B1");
    let o = script_cmd(&bin)
        .args(["--prefix", prefix.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let run = std::fs::read_dir(prefix.join("install"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let job = std::fs::read_to_string(run.join("install.sbatch")).unwrap();
    assert!(job.contains("A1\\,B1") || job.contains("A1,B1"), "{job}");
}

#[test]
fn an_option_without_its_value_is_refused() {
    let bin = Bin::new();
    let prefix = tempfile::tempdir().unwrap();
    let p = prefix.path().to_str().unwrap();
    for args in [
        vec!["--dry-run", "--prefix"],
        vec!["--prefix", "--dry-run"],
        vec!["--dry-run", "--prefix", p, "--partition"],
        vec!["--dry-run", "--prefix", p, "--partition", "PART1 --x"],
    ] {
        let o = run(&bin, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o));
        assert!(!text(&o).contains("#SBATCH"), "{args:?} printed a job");
    }
    let o = run(&bin, &["--dry-run", "--prefix"]);
    assert!(text(&o).contains("--prefix needs a value"), "{}", text(&o));
}

/// The job, run here as Slurm would, with stand-ins: `cargo install`
/// writes a script named for the crate into `--root`/bin.
fn job_run(bin: &Bin, prefix: &Path, scratch: &Path, extra: &[&str]) -> (Output, PathBuf) {
    let job = job_script(bin, prefix, scratch, extra);
    let o = job_cmd(bin, prefix, &job).output().unwrap();
    (o, prefix.join("install").join("RUN1"))
}

/// The job the script writes for `extra`, saved in `prefix`.
fn job_script(bin: &Bin, prefix: &Path, scratch: &Path, extra: &[&str]) -> PathBuf {
    let mut args = vec![
        "--dry-run".to_string(),
        "--prefix".into(),
        prefix.to_str().unwrap().into(),
        "--build-tmp".into(),
        scratch.to_str().unwrap().into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let o = script_cmd(bin).args(&args).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let job = String::from_utf8(o.stdout).unwrap();
    let start = job.find("#!/bin/bash").expect("a job script");
    let file = prefix.join("job.sh");
    std::fs::write(&file, &job[start..]).unwrap();
    file
}

/// `job` run as Slurm would, in the run folder `prefix`/install/RUN1, with
/// only `bin`'s stand-ins and none of this machine's toolchain or libclang.
fn job_cmd(bin: &Bin, prefix: &Path, job: &Path) -> Command {
    let run = prefix.join("install").join("RUN1");
    std::fs::create_dir_all(&run).unwrap();
    let mut c = Command::new("bash");
    c.arg(job)
        .env("PATH", bin.path())
        .env("SLURM_JOB_ID", "77")
        .env("SLURM_CPUS_PER_TASK", "3")
        .env("LEGUME_INSTALL_RUN", &run)
        .env("HOME", prefix)
        // Not the toolchain running this test (rustup sets these).
        .env_remove("CARGO_HOME")
        .env_remove("RUSTUP_HOME")
        .env_remove("RUSTUP_TOOLCHAIN")
        // Nor this machine's libclang.
        .env_remove("LIBCLANG_PATH")
        .env_remove("BINDGEN_EXTRA_CLANG_ARGS")
        .env_remove("LD_LIBRARY_PATH");
    c
}

/// Stand-ins for the Rust toolchain: `cargo install ... --root R ...
/// NAME` writes R/bin/<bin of NAME>, which prints a version; `rustc`
/// prints one; `rustup` keeps its arguments in $HOME/rustup-calls.
fn fake_toolchain(bin: &Bin) {
    bin.tool(
        "cargo",
        r#"
if [ "$1" = "--version" ]; then echo "cargo 1.95.0"; exit 0; fi
echo "$@" >> "$HOME/cargo-calls"
echo "${LIBCLANG_PATH:-}|${BINDGEN_EXTRA_CLANG_ARGS:-}" >> "$HOME/cargo-env"
[ -n "$CARGO_TARGET_DIR" ] && mkdir -p "$CARGO_TARGET_DIR" && echo "$CARGO_TARGET_DIR" > "$HOME/target-dir"
root=""; prev=""; for a in "$@"; do [ "$prev" = "--root" ] && root="$a"; prev="$a"; done
name="$a"
case "$name" in
legume-mcp) b=legume ;; pinto-rs) b=pinto ;; broken*) exit 101 ;;
stopped) kill -TERM "$PPID"; sleep 1; : > "$HOME/build-finished"; exit 0 ;;
*) b="$name" ;;
esac
mkdir -p "$root/bin"
printf '#!/bin/sh\necho "%s 9.9.9"\n' "$b" > "$root/bin/$b"
chmod +x "$root/bin/$b"
"#,
    );
    bin.tool("rustc", r#"echo "rustc 1.95.0""#);
    bin.tool("rustup", r#"echo "$@" >> "$HOME/rustup-calls""#);
    // Never the real ones: python3 would fetch from PyPI. This python3
    // unpacks a libclang the way PyPI's wheel lays it out.
    bin.tool(
        "python3",
        r#"echo "$@" >> "$HOME/python-calls"
cat > /dev/null
d="$2/libclang-18.data/platlib/clang/native"; mkdir -p "$d" && : > "$d/libclang.so""#,
    );
    bin.tool(
        "gcc",
        r#"[ "$1" = "-print-file-name=include" ] && echo /GCC1/include"#,
    );
    // No libclang anywhere, and no download.
    bin.tool("ldconfig", "true");
    bin.tool("llvm-config", "exit 1");
    bin.tool("curl", r#"echo "$@" >> "$HOME/curl-calls"; exit 7"#);
}

/// The libclang variables cargo saw, one line per install.
fn cargo_env(prefix: &Path) -> Vec<String> {
    let t = std::fs::read_to_string(prefix.join("cargo-env")).unwrap();
    t.lines().map(str::to_string).collect()
}

#[test]
fn the_job_installs_each_crate_and_says_done() {
    let bin = Bin::new();
    fake_toolchain(&bin);
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
    fake_toolchain(&bin);
    let prefix = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, _) = job_run(&bin, prefix.path(), scratch.path(), &["--no-rust-update"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!prefix.path().join("rustup-calls").exists());
}

#[test]
fn a_failed_crate_fails_the_job_and_the_rest_install() {
    let bin = Bin::new();
    fake_toolchain(&bin);
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

#[test]
fn faba_gets_a_libclang_when_the_node_has_none() {
    // faba's htslib bindings need libclang; the node has none (no
    // ldconfig entry, no LIBCLANG_PATH), so the wheel is fetched into the
    // prefix once, and bindgen gets gcc's built-in headers.
    let bin = Bin::new();
    fake_toolchain(&bin);
    let prefix = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, run) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &["--no-rust-update", "legume-mcp:benchmark", "faba"],
    );
    assert!(o.status.success(), "{}", text(&o));
    let fetches = std::fs::read_to_string(prefix.path().join("python-calls")).unwrap();
    assert_eq!(fetches.lines().count(), 1, "{fetches}");
    let want = prefix
        .path()
        .join("libclang/libclang-18.data/platlib/clang/native");
    let env = cargo_env(prefix.path());
    assert_eq!(env.len(), 2);
    for line in &env {
        assert_eq!(line, &format!("{}|-I/GCC1/include", want.display()));
    }
    assert_eq!(
        std::fs::read_to_string(run.join("status")).unwrap().trim(),
        "done"
    );
    // A second run finds the one already fetched.
    let (o, _) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &["--no-rust-update", "faba"],
    );
    assert!(o.status.success(), "{}", text(&o));
    let fetches = std::fs::read_to_string(prefix.path().join("python-calls")).unwrap();
    assert_eq!(fetches.lines().count(), 1, "fetched again: {fetches}");
}

#[test]
fn a_libclang_on_the_node_is_used_as_it_is() {
    let bin = Bin::new();
    fake_toolchain(&bin);
    // Found early in a long listing: grep must not stop ldconfig short,
    // which pipefail would read as no match.
    bin.tool(
        "ldconfig",
        r#"echo "	libclang.so.18 (libc6,x86-64) => /usr/lib64/libclang.so.18"
i=0; while [ $i -lt 4000 ]; do echo "	libfiller$i.so.1 (libc6,x86-64) => /usr/lib64/libfiller$i.so.1"; i=$((i+1)); done"#,
    );
    let prefix = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, _) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &["--no-rust-update", "faba"],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(!prefix.path().join("python-calls").exists());
    // Nothing is added to the node's own clang.
    assert_eq!(cargo_env(prefix.path()), vec!["|".to_string()]);
}

#[test]
fn no_libclang_is_fetched_without_faba_or_with_no_libclang() {
    for extra in [
        &["--no-rust-update", "legume-mcp:benchmark"][..],
        &["--no-rust-update", "--no-libclang", "faba"][..],
    ] {
        let bin = Bin::new();
        fake_toolchain(&bin);
        let prefix = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let (o, _) = job_run(&bin, prefix.path(), scratch.path(), extra);
        assert!(o.status.success(), "{extra:?}: {}", text(&o));
        assert!(!prefix.path().join("python-calls").exists(), "{extra:?}");
        assert_eq!(cargo_env(prefix.path()), vec!["|".to_string()], "{extra:?}");
    }
}

#[test]
fn a_libclang_from_llvm_config_or_ld_library_path_is_used_as_it_is() {
    for how in ["llvm-config", "LD_LIBRARY_PATH"] {
        let bin = Bin::new();
        fake_toolchain(&bin);
        let lib = tempfile::tempdir().unwrap();
        std::fs::write(lib.path().join("libclang.so.18"), "").unwrap();
        if how == "llvm-config" {
            bin.tool("llvm-config", &format!("echo {}", lib.path().display()));
        }
        let prefix = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let job = job_script(
            &bin,
            prefix.path(),
            scratch.path(),
            &["--no-rust-update", "faba"],
        );
        let mut c = job_cmd(&bin, prefix.path(), &job);
        if how == "LD_LIBRARY_PATH" {
            c.env("LD_LIBRARY_PATH", lib.path());
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{how}: {}", text(&o));
        assert!(
            !prefix.path().join("python-calls").exists(),
            "{how}: fetched"
        );
    }
}

#[test]
fn a_stopped_job_says_it_failed() {
    // Slurm's time limit or scancel: TERM to the job while it builds.
    let bin = Bin::new();
    fake_toolchain(&bin);
    let prefix = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, run) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &["--no-rust-update", "stopped", "legume-mcp:benchmark"],
    );
    assert!(!o.status.success(), "{}", text(&o));
    let status = std::fs::read_to_string(run.join("status")).unwrap();
    assert!(status.starts_with("failed: the job stopped"), "{status}");
    // The build was stopped with it, and the crate after it never started.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        !prefix.path().join("build-finished").exists(),
        "build left running"
    );
    let calls = std::fs::read_to_string(prefix.path().join("cargo-calls")).unwrap();
    assert!(!calls.contains("legume-mcp"), "{calls}");
}

#[test]
fn a_toolchain_in_the_prefix_is_used_with_its_rustup_home() {
    // No cargo on PATH; one rustup put in the prefix on an earlier run,
    // whose proxy works only with RUSTUP_HOME set.
    let bin = Bin::new();
    fake_toolchain(&bin);
    std::fs::remove_file(bin.dir.path().join("cargo")).unwrap();
    let prefix = tempfile::tempdir().unwrap();
    let cargo_bin = prefix.path().join("rust/cargo/bin");
    std::fs::create_dir_all(&cargo_bin).unwrap();
    let want = prefix.path().join("rust/rustup");
    let proxy = format!(
        "#!/bin/sh\n[ \"$RUSTUP_HOME\" = \"{}\" ] || exit 1\n[ \"$1\" = --version ] && echo cargo 1.95.0\nexit 0\n",
        want.display()
    );
    let cargo = cargo_bin.join("cargo");
    std::fs::write(&cargo, proxy).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, run) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &["--no-rust-update", "legume-mcp:benchmark"],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        !prefix.path().join("curl-calls").exists(),
        "rustup fetched again"
    );
    assert_eq!(
        std::fs::read_to_string(run.join("status")).unwrap().trim(),
        "done"
    );
}

#[test]
fn versions_name_only_what_this_job_installed() {
    let bin = Bin::new();
    fake_toolchain(&bin);
    let prefix = tempfile::tempdir().unwrap();
    let other = prefix.path().join("bin/OTHER1");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    let ran = prefix.path().join("other-ran");
    std::fs::write(&other, format!("#!/bin/sh\n: > {}\n", ran.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (o, run) = job_run(
        &bin,
        prefix.path(),
        scratch.path(),
        &["--no-rust-update", "legume-mcp:benchmark"],
    );
    assert!(o.status.success(), "{}", text(&o));
    let versions = std::fs::read_to_string(run.join("versions.txt")).unwrap();
    assert!(versions.contains("legume 9.9.9"), "{versions}");
    assert!(!versions.contains("OTHER1"), "{versions}");
    assert!(!ran.exists(), "an unrelated program was run");
}
