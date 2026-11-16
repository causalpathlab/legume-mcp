//! The end of a file, read without reading the rest: a log's last lines.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The last `max_bytes` of the file at `path` at most, as text (bytes not
/// UTF-8 replaced), from the start of a line: a line cut by the limit is
/// left out. Empty when the file cannot be read.
pub(crate) fn tail(path: &Path, max_bytes: u64) -> String {
    tail_from(path, 0, max_bytes)
}

/// [`tail`] of the bytes of the file at `path` from offset `from` on: the
/// last `max_bytes` of them at most, from the start of a line (or from
/// `from` itself, when it is within reach).
pub(crate) fn tail_from(path: &Path, from: u64, max_bytes: u64) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = from.max(len.saturating_sub(max_bytes));
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.take(max_bytes).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    // Cut inside a line: from the next one.
    let bytes = if start > from {
        match bytes.iter().position(|&b| b == b'\n') {
            Some(i) => &bytes[i + 1..],
            None => &[][..],
        }
    } else {
        &bytes[..]
    };
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_holds_whole_lines_within_its_limit() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f.log");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        assert_eq!(tail(&p, 100), "one\ntwo\nthree\n");
        // "o\nthree\n" is in reach: the cut line goes.
        assert_eq!(tail(&p, 8), "three\n");
        assert_eq!(tail(&p, 3), "");
        assert_eq!(tail_from(&p, 4, 100), "two\nthree\n");
        assert_eq!(tail(&d.path().join("none"), 10), "");
    }
}
