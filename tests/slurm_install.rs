//! `scripts/slurm-install.sh`: the batch job it writes and submits, and
//! that job run here by a stand-in `sbatch`, with stand-ins for every tool
//! it may call, so nothing is fetched or built.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/slurm-install.sh")
}

/// An executable shell script at `path` running `body`.
fn write_exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        write_exe(&self.dir.path().join(name), body);
    }
}

/// The script run by bash with nothing of this environment but `bin`'s
/// stand-ins first on PATH and `home` as HOME.
fn script_cmd(bin: &Bin, home: &Path) -> Command {
    let mut c = Command::new("bash");
    c.arg(script())
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", bin.dir.path().display()),
        )
        .env("HOME", home);
    c
}

fn run(bin: &Bin, args: &[&str]) -> Output {
    script_cmd(bin, bin.dir.path()).args(args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// What `o` printed, once it is known to have succeeded.
fn ok(o: &Output) -> String {
    let t = text(o);
    assert!(o.status.success(), "{t}");
    t
}

/// The one run folder in `prefix`/install.
fn only_run(prefix: &Path) -> PathBuf {
    let runs: Vec<_> = std::fs::read_dir(prefix.join("install"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.into_iter().next().unwrap()
}

#[test]
fn dry_run_prints_the_job() {
    let bin = Bin::new();
    let prefix = tempfile::tempdir().unwrap();
    let p = prefix.path().to_str().unwrap();
    let t = ok(&run(
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
    ));
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
    let t = ok(&run(
        &bin,
        &["--dry-run", "--prefix", prefix.path().to_str().unwrap()],
    ));
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
    let t = ok(&script_cmd(&bin, cwd.path())
        .args(["--dry-run", "--prefix", "tools"])
        .current_dir(cwd.path())
        .output()
        .unwrap());
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
fn a_prefix_with_a_comma_reaches_the_job_whole() {
    let bin = Bin::new();
    bin.tool("sbatch", "echo 4242");
    let top = tempfile::tempdir().unwrap();
    let prefix = top.path().join("A1,B1");
    ok(&script_cmd(&bin, top.path())
        .args(["--prefix", prefix.to_str().unwrap()])
        .output()
        .unwrap());
    let job = std::fs::read_to_string(only_run(&prefix).join("install.sbatch")).unwrap();
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

/// A job submitted by the script and run at once by a stand-in `sbatch`,
/// with stand-ins for the Rust toolchain and everything else it calls.
/// HOME is the prefix, where the stand-ins keep what they were asked.
struct Job {
    bin: Bin,
    prefix: tempfile::TempDir,
    scratch: tempfile::TempDir,
}

impl Job {
    /// Stand-ins: `cargo install ... --root R ... NAME` writes R/bin/<bin of
    /// NAME> and adds it to a record that `cargo install --list --root R`
    /// prints as cargo does; `rustc` prints a version; `rustup` keeps its
    /// arguments in $HOME/rustup-calls. `sbatch` keeps its arguments and
    /// the status it was called with, runs the job with bash into its
    /// --output, keeps the job's exit code in $HOME/job-exit and prints a
    /// job id.
    fn new() -> Self {
        let bin = Bin::new();
        bin.tool(
            "cargo",
            r#"
if [ "$1" = "--version" ]; then echo "cargo 1.95.0"; exit 0; fi
root=""; prev=""; for a in "$@"; do [ "$prev" = "--root" ] && root="$a"; prev="$a"; done
name="$a"
record="$root/.crates.txt"
if [ "$2" = "--list" ]; then
    [ -f "$record" ] || exit 0
    sort -u "$record" | while read -r n b; do printf '%s v9.9.9:\n    %s\n' "$n" "$b"; done
    exit 0
fi
echo "$@" >> "$HOME/cargo-calls"
echo "${LIBCLANG_PATH:-}|${BINDGEN_EXTRA_CLANG_ARGS:-}" >> "$HOME/cargo-env"
[ -n "$CARGO_TARGET_DIR" ] && mkdir -p "$CARGO_TARGET_DIR" && echo "$CARGO_TARGET_DIR" > "$HOME/target-dir"
case "$name" in
legume-mcp) b=legume ;; pinto-rs) b=pinto ;; broken*) exit 101 ;;
stopped) kill -TERM "$PPID"; sleep 1; : > "$HOME/build-finished"; exit 0 ;;
*) b="$name" ;;
esac
mkdir -p "$root/bin"
printf '#!/bin/sh\n' > "$root/bin/$b"
chmod +x "$root/bin/$b"
echo "$name $b" >> "$record"
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
        // The job keeps this stand-in's stderr open on fd 9, so the
        // script's output() also waits for any build the job left behind.
        bin.tool(
            "sbatch",
            r#"out=""
for a in "$@"; do case "$a" in --output=*) out="${a#--output=}" ;; esac; last="$a"; done
echo "$@" > "$HOME/sbatch-args"
cat "${last%/*}/status" > "$HOME/sbatch-status"
SLURM_JOB_ID=77 bash "$last" 9>&2 > "${out:-${last%/*}/slurm.log}" 2>&1
echo $? > "$HOME/job-exit"
echo 4242"#,
        );
        Self {
            bin,
            prefix: tempfile::tempdir().unwrap(),
            scratch: tempfile::tempdir().unwrap(),
        }
    }

    fn prefix(&self) -> &Path {
        self.prefix.path()
    }

    /// The script submitting the job for `extra`, with 3 cores.
    fn cmd(&self, extra: &[&str]) -> Command {
        let mut c = script_cmd(&self.bin, self.prefix());
        c.env("SLURM_CPUS_PER_TASK", "3")
            .arg("--prefix")
            .arg(self.prefix())
            .arg("--build-tmp")
            .arg(self.scratch.path())
            .args(extra);
        c
    }

    fn run(&self, extra: &[&str]) -> Output {
        self.cmd(extra).output().unwrap()
    }

    /// A file in the prefix, if it is there.
    fn file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.prefix().join(name)).ok()
    }

    /// A file in the job's run folder.
    fn run_file(&self, name: &str) -> String {
        let run = only_run(self.prefix());
        std::fs::read_to_string(run.join(name)).unwrap_or_default()
    }

    fn status(&self) -> String {
        self.run_file("status").trim().to_string()
    }

    /// The job's exit code, as sbatch's stand-in saw it.
    fn exit(&self) -> String {
        self.file("job-exit").unwrap_or_default().trim().to_string()
    }

    /// The libclang variables cargo saw, one line per install.
    fn cargo_env(&self) -> Vec<String> {
        let t = self.file("cargo-env").unwrap();
        t.lines().map(str::to_string).collect()
    }
}

#[test]
fn submit_writes_the_job_and_its_status() {
    let job = Job::new();
    let t = ok(&job.run(&["--no-rust-update", "legume-mcp:benchmark"]));
    assert!(t.contains("4242"), "{t}");
    let run = only_run(job.prefix());
    assert!(run.join("install.sbatch").is_file());
    // The status as sbatch is called: the job may start before it returns.
    assert_eq!(job.file("sbatch-status").unwrap().trim(), "submitted");
    // The job's own once it has run.
    assert_eq!(job.status(), "done", "{}", job.run_file("slurm.log"));
    assert_eq!(job.run_file("job_id").trim(), "4242");
    let sent = job.file("sbatch-args").unwrap();
    assert!(sent.contains("--parsable"), "{sent}");
    assert!(sent.contains("install.sbatch"), "{sent}");
    // The run folder is in the job, not passed through --export.
    assert!(!sent.contains("--export"), "{sent}");
    let script = job.run_file("install.sbatch");
    assert!(script.contains(run.to_str().unwrap()), "{script}");
}

#[test]
fn the_job_installs_each_crate_and_says_done() {
    let job = Job::new();
    ok(&job.run(&["legume-mcp:benchmark", "pinto-rs@0.16.33"]));
    assert_eq!(job.status(), "done", "{}", job.run_file("slurm.log"));
    assert_eq!(job.exit(), "0");
    let calls = job.file("cargo-calls").unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(lines[0].contains("install --locked"), "{calls}");
    assert!(lines[0].contains("--features benchmark"), "{calls}");
    assert!(lines[0].contains("-j 3"), "{calls}");
    assert!(lines[0].ends_with("legume-mcp"), "{calls}");
    assert!(lines[1].contains("--version 0.16.33"), "{calls}");
    assert!(lines[1].ends_with("pinto-rs"), "{calls}");
    // The build ran in node scratch, which is removed after.
    let target = job.file("target-dir").unwrap();
    assert!(
        target
            .trim()
            .starts_with(job.scratch.path().to_str().unwrap()),
        "{target}"
    );
    assert!(!Path::new(target.trim()).exists(), "scratch left behind");
    // The toolchain was brought up to date first.
    let rustup = job.file("rustup-calls").unwrap();
    assert!(rustup.contains("update stable"), "{rustup}");
    let versions = job.run_file("versions.txt");
    assert!(
        versions.contains("legume-mcp v9.9.9:\n    legume\n"),
        "{versions}"
    );
    assert!(
        versions.contains("pinto-rs v9.9.9:\n    pinto\n"),
        "{versions}"
    );
    assert!(versions.contains("glibc"), "{versions}");
}

#[test]
fn no_rust_update_leaves_the_toolchain() {
    let job = Job::new();
    ok(&job.run(&["--no-rust-update"]));
    assert_eq!(job.status(), "done", "{}", job.run_file("slurm.log"));
    assert!(job.file("rustup-calls").is_none());
}

#[test]
fn a_failed_crate_fails_the_job_and_the_rest_install() {
    let job = Job::new();
    ok(&job.run(&[
        "--no-rust-update",
        "broken",
        "legume-mcp:benchmark",
        "broken2",
    ]));
    assert_eq!(job.exit(), "1");
    assert_eq!(job.status(), "failed broken broken2");
    assert!(job.prefix().join("bin/legume").is_file());
    let versions = job.run_file("versions.txt");
    assert!(versions.contains("legume-mcp v9.9.9"), "{versions}");
}

#[test]
fn faba_gets_a_libclang_when_the_node_has_none() {
    // faba's htslib bindings need libclang; the node has none (no
    // ldconfig entry, no LIBCLANG_PATH), so the wheel is fetched into the
    // prefix once, and bindgen gets gcc's built-in headers.
    let job = Job::new();
    ok(&job.run(&["--no-rust-update", "legume-mcp:benchmark", "faba"]));
    assert_eq!(job.file("python-calls").unwrap().lines().count(), 1);
    let want = job
        .prefix()
        .join("libclang/libclang-18.data/platlib/clang/native");
    let env = job.cargo_env();
    assert_eq!(env.len(), 2);
    for line in &env {
        assert_eq!(line, &format!("{}|-I/GCC1/include", want.display()));
    }
    assert_eq!(job.status(), "done");
    // A second run finds the one already fetched. Its run folder may have
    // the first one's name, made in the same second, so that one goes.
    std::fs::remove_dir_all(job.prefix().join("install")).unwrap();
    ok(&job.run(&["--no-rust-update", "faba"]));
    assert_eq!(job.status(), "done");
    let fetches = job.file("python-calls").unwrap();
    assert_eq!(fetches.lines().count(), 1, "fetched again: {fetches}");
}

#[test]
fn a_libclang_on_the_node_is_used_as_it_is() {
    let job = Job::new();
    // Found early in a long listing: grep must not stop ldconfig short,
    // which pipefail would read as no match.
    job.bin.tool(
        "ldconfig",
        r#"echo "	libclang.so.18 (libc6,x86-64) => /usr/lib64/libclang.so.18"
i=0; while [ $i -lt 4000 ]; do echo "	libfiller$i.so.1 (libc6,x86-64) => /usr/lib64/libfiller$i.so.1"; i=$((i+1)); done"#,
    );
    ok(&job.run(&["--no-rust-update", "faba"]));
    assert_eq!(job.status(), "done");
    assert!(job.file("python-calls").is_none());
    // Nothing is added to the node's own clang.
    assert_eq!(job.cargo_env(), vec!["|".to_string()]);
}

#[test]
fn no_libclang_is_fetched_without_faba_or_with_no_libclang() {
    for extra in [
        &["--no-rust-update", "legume-mcp:benchmark"][..],
        &["--no-rust-update", "--no-libclang", "faba"][..],
    ] {
        let job = Job::new();
        ok(&job.run(extra));
        assert_eq!(job.status(), "done", "{extra:?}");
        assert!(job.file("python-calls").is_none(), "{extra:?}");
        assert_eq!(job.cargo_env(), vec!["|".to_string()], "{extra:?}");
    }
}

#[test]
fn a_libclang_from_llvm_config_or_ld_library_path_is_used_as_it_is() {
    for how in ["llvm-config", "LD_LIBRARY_PATH"] {
        let job = Job::new();
        let lib = tempfile::tempdir().unwrap();
        std::fs::write(lib.path().join("libclang.so.18"), "").unwrap();
        let mut c = job.cmd(&["--no-rust-update", "faba"]);
        if how == "llvm-config" {
            job.bin
                .tool("llvm-config", &format!("echo {}", lib.path().display()));
        } else {
            c.env("LD_LIBRARY_PATH", lib.path());
        }
        ok(&c.output().unwrap());
        assert_eq!(job.status(), "done", "{how}");
        assert!(job.file("python-calls").is_none(), "{how}: fetched");
    }
}

#[test]
fn a_stopped_job_says_it_failed() {
    // Slurm's time limit or scancel: TERM to the job while it builds.
    let job = Job::new();
    ok(&job.run(&["--no-rust-update", "stopped", "legume-mcp:benchmark"]));
    assert_eq!(job.exit(), "143");
    let status = job.status();
    assert!(status.starts_with("failed: the job stopped"), "{status}");
    // The build was stopped with it, and the crate after it never started.
    // output() waited for the build too, which holds sbatch's stderr.
    assert!(job.file("build-finished").is_none(), "build left running");
    let calls = job.file("cargo-calls").unwrap();
    assert!(!calls.contains("legume-mcp"), "{calls}");
}

#[test]
fn a_toolchain_in_the_prefix_is_used_with_its_rustup_home() {
    // No cargo on PATH; one rustup put in the prefix on an earlier run,
    // whose proxy works only with RUSTUP_HOME set.
    let job = Job::new();
    std::fs::remove_file(job.bin.dir.path().join("cargo")).unwrap();
    let cargo_bin = job.prefix().join("rust/cargo/bin");
    std::fs::create_dir_all(&cargo_bin).unwrap();
    let want = job.prefix().join("rust/rustup");
    write_exe(
        &cargo_bin.join("cargo"),
        &format!(
            "[ \"$RUSTUP_HOME\" = \"{}\" ] || exit 1\n[ \"$1\" = --version ] && echo cargo 1.95.0\nexit 0",
            want.display()
        ),
    );
    ok(&job.run(&["--no-rust-update", "legume-mcp:benchmark"]));
    assert!(job.file("curl-calls").is_none(), "rustup fetched again");
    assert_eq!(job.status(), "done", "{}", job.run_file("slurm.log"));
}

#[test]
fn versions_name_only_what_this_job_installed() {
    let job = Job::new();
    // An earlier install in cargo's record, and a program cargo never
    // installed.
    let bin = job.prefix().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(job.prefix().join(".crates.txt"), "pinto-rs pinto\n").unwrap();
    write_exe(&bin.join("pinto"), "");
    let ran = job.prefix().join("other-ran");
    write_exe(&bin.join("OTHER1"), &format!(": > {}", ran.display()));
    ok(&job.run(&["--no-rust-update", "legume-mcp:benchmark"]));
    assert_eq!(job.status(), "done");
    let versions = job.run_file("versions.txt");
    assert!(
        versions.contains("legume-mcp v9.9.9:\n    legume\n"),
        "{versions}"
    );
    assert!(!versions.contains("pinto"), "{versions}");
    assert!(!versions.contains("OTHER1"), "{versions}");
    assert!(!ran.exists(), "an unrelated program was run");
}
