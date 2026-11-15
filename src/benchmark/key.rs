//! Content keys: FNV-1a 128 over canonical JSON (keys sorted here, not by
//! serde_json's map, whose order a feature elsewhere in the build can
//! change), over a file's bytes, and over a folder's listing. The seed
//! mixers (splitmix64 and FNV-1a 64) are here too, so a seed is the same
//! wherever it is derived.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

/// FNV-1a 128, fed in pieces.
struct Fnv128(u128);

impl Fnv128 {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u128::from(b)).wrapping_mul(Self::PRIME);
        }
    }

    fn hex(&self) -> String {
        format!("{:032x}", self.0)
    }
}

impl std::io::Write for Fnv128 {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// FNV-1a 64 of `bytes`.
pub fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}

/// splitmix64's output for the state `x`: `x` plus the golden gamma, mixed.
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// The key of `bytes`: FNV-1a 128 as 32 hex digits.
pub fn key(bytes: &[u8]) -> String {
    let mut h = Fnv128::new();
    h.update(bytes);
    h.hex()
}

/// `v` as compact JSON with every object's keys sorted by their bytes.
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// A spec's file: its canonical JSON ending in a newline, as the heredoc
/// that records it writes it back (a heredoc ends its text with one).
pub fn spec_file(v: &Value) -> String {
    format!("{}\n", canonical(v))
}

/// The key of `v`'s canonical JSON.
pub fn key_of(v: &Value) -> String {
    key(canonical(v).as_bytes())
}

/// The key of the bytes of the file at `path`, read in pieces.
pub fn file_key(path: &Path) -> Result<String> {
    let mut h = Fnv128::new();
    std::fs::File::open(path)
        .and_then(|mut f| std::io::copy(&mut f, &mut h))
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(h.hex())
}

/// The key of the folder at `path`: of its listing, every file and folder
/// under it (path from it, size, mtime in ns), sorted. Not of the bytes.
pub fn dir_key(path: &Path) -> Result<String> {
    let mut rows = Vec::new();
    list(path, "", &mut rows)?;
    rows.sort();
    let mut h = Fnv128::new();
    for row in &rows {
        h.update(row.as_bytes());
        h.update(b"\n");
    }
    Ok(h.hex())
}

/// `dir`'s entries under it, as `rel\tsize\tmtime_ns`, into `rows`.
fn list(dir: &Path, rel: &str, rows: &mut Vec<String>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))?;
    for e in entries {
        let e = e.with_context(|| format!("listing {}", dir.display()))?;
        let name = format!("{rel}{}", e.file_name().to_string_lossy());
        let meta = std::fs::symlink_metadata(e.path())
            .with_context(|| format!("reading {}", e.path().display()))?;
        rows.push(format!("{name}\t{}\t{}", meta.len(), mtime_ns(&meta)));
        if meta.is_dir() {
            list(&e.path(), &format!("{name}/"), rows)?;
        }
    }
    Ok(())
}

/// When `meta`'s file was last changed, in ns from the epoch; 0 when the
/// system cannot say.
fn mtime_ns(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Published vectors: FNV-1a of "" is the offset basis; of "a" the
    // values below. splitmix64 seeded 0 gives 0xe220a8397b1dcdaf first.
    #[test]
    fn hashes_match_their_published_vectors() {
        assert_eq!(key(b""), format!("{:032x}", Fnv128::OFFSET));
        assert_eq!(key(b""), "6c62272e07bb014262b821756295c58d");
        assert_eq!(fnv64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(splitmix64(0), 0xe220a8397b1dcdaf);
        assert_eq!(key(b"a"), "d228cb696f1a8caf78912b704e4a8964");
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let a = json!({"b": [{"z": 1, "a": "x"}], "a": {"d": null, "c": true}});
        assert_eq!(
            canonical(&a),
            r#"{"a":{"c":true,"d":null},"b":[{"a":"x","z":1}]}"#
        );
        // Another order of the same object is the same text and key;
        // another array order is not.
        let b: Value =
            serde_json::from_str(r#"{"a": {"d": null, "c": true}, "b": [{"a": "x", "z": 1}]}"#)
                .unwrap();
        assert_eq!(key_of(&a), key_of(&b));
        assert_ne!(key_of(&json!([1, 2])), key_of(&json!([2, 1])));
        assert_eq!(canonical(&json!("a\"\n")), r#""a\"\n""#);
    }

    #[test]
    fn a_file_is_keyed_by_its_bytes_and_a_folder_by_its_listing() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&a, b"GENE1\n").unwrap();
        std::fs::write(&b, b"GENE1\n").unwrap();
        assert_eq!(file_key(&a).unwrap(), key(b"GENE1\n"));
        assert_eq!(file_key(&a).unwrap(), file_key(&b).unwrap());
        std::fs::write(&a, b"GENE2 \n").unwrap();
        assert_eq!(file_key(&a).unwrap(), key(b"GENE2 \n"));

        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let before = dir_key(d.path()).unwrap();
        assert_eq!(before, dir_key(d.path()).unwrap());
        std::fs::write(sub.join("c"), b"").unwrap();
        assert_ne!(
            before,
            dir_key(d.path()).unwrap(),
            "a nested file is listed"
        );

        // A rewrite of the same size is seen by its mtime.
        let c = std::fs::File::options()
            .write(true)
            .open(sub.join("c"))
            .unwrap();
        let listed = dir_key(d.path()).unwrap();
        c.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        assert_ne!(listed, dir_key(d.path()).unwrap(), "a nested mtime");
    }

    // A folder's key is of its listing in name order, whatever order the
    // system lists it in: each entry's path, size and mtime in ns.
    #[test]
    fn a_folder_is_keyed_by_its_listing_in_name_order() {
        let d = tempfile::tempdir().unwrap();
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(1_000_000_007);
        let mut rows = Vec::new();
        for i in (0..16).rev() {
            let name = format!("F{i:02}");
            std::fs::write(d.path().join(&name), b"GENE1").unwrap();
            std::fs::File::options()
                .write(true)
                .open(d.path().join(&name))
                .unwrap()
                .set_modified(t)
                .unwrap();
            rows.push(format!("{name}\t5\t1000000007\n"));
        }
        rows.sort();
        assert_eq!(dir_key(d.path()).unwrap(), key(rows.concat().as_bytes()));
    }
}
