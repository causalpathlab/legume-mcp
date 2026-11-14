//! What the tests of `legume score` and `legume universe` share: the
//! binary run with its output captured, or fed through a pipe, and the TSV
//! fixtures it reads.

use flate2::write::GzEncoder;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// `legume` with `args`, its output captured.
pub fn legume(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_legume"))
        .args(args)
        .output()
        .unwrap()
}

/// `legume` with `args`, each argument `piped` replaced by `/dev/stdin` and
/// `input` fed through it: a stream that cannot be opened twice, as
/// `<(...)` is.
pub fn legume_fed(
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    piped: &Path,
    input: Vec<u8>,
) -> Output {
    let piped = piped.as_os_str();
    let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    assert!(args.iter().any(|a| a == piped), "{piped:?} is no argument");
    let args = args.iter().map(|a| {
        if a == piped {
            OsStr::new("/dev/stdin")
        } else {
            a
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_legume"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // A run that stops reading closes the pipe: that write error is the
    // run's answer, not the test's.
    let feed = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let o = child.wait_with_output().unwrap();
    feed.join().unwrap();
    o
}

/// A TSV with a header, gzipped when the name ends in `.gz`.
pub fn tsv(path: &Path, header: &str, rows: &[impl AsRef<str>]) {
    let mut text = format!("{header}\n");
    for r in rows {
        text.push_str(r.as_ref());
        text.push('\n');
    }
    if path.extension().is_some_and(|e| e == "gz") {
        let mut gz = GzEncoder::new(std::fs::File::create(path).unwrap(), Default::default());
        gz.write_all(text.as_bytes()).unwrap();
        gz.finish().unwrap();
    } else {
        std::fs::write(path, text).unwrap();
    }
}
