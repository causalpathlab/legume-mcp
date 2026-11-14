//! Content keys: FNV-1a 128 over canonical JSON (keys sorted here, not by
//! serde_json's map, whose order a feature elsewhere in the build can
//! change), over a file's bytes, and over a folder's listing. The seed
//! mixers (splitmix64 and FNV-1a 64) are here too, so a seed is the same
//! wherever it is derived.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// FNV-1a 128 of `bytes`.
pub fn fnv128(bytes: &[u8]) -> u128 {
    let mut h = Fnv128::new();
    h.write(bytes);
    h.0
}

/// FNV-1a 128, fed in pieces.
struct Fnv128(u128);

impl Fnv128 {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u128::from(b)).wrapping_mul(Self::PRIME);
        }
    }

    fn hex(&self) -> String {
        format!("{:032x}", self.0)
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
    format!("{:032x}", fnv128(bytes))
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
    let mut f = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut h = Fnv128::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            return Ok(h.hex());
        }
        h.write(&buf[..n]);
    }
}

/// The key of the folder at `path`: of its listing, every file and folder
/// under it (path from it, size, mtime in ns), sorted. Not of the bytes.
pub fn dir_key(path: &Path) -> Result<String> {
    let mut rows = Vec::new();
    list(path, "", &mut rows)?;
    rows.sort();
    let mut h = Fnv128::new();
    for row in &rows {
        h.write(row.as_bytes());
        h.write(b"\n");
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

/// Declared inputs' content keys, each file read once per change: cached
/// by (device, inode, size, mtime).
#[derive(Default)]
pub struct Inputs {
    seen: HashMap<(u64, u64, u64, u128), String>,
}

impl Inputs {
    /// The key of the input at `path`: its bytes, or with `dir` its
    /// listing ([`dir_key`]).
    pub fn key(&mut self, path: &Path, dir: bool) -> Result<String> {
        if dir {
            return dir_key(path);
        }
        let meta =
            std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
        let id = file_id(&meta);
        if let Some(k) = id.and_then(|id| self.seen.get(&id)) {
            return Ok(k.clone());
        }
        let k = file_key(path)?;
        if let Some(id) = id {
            self.seen.insert(id, k.clone());
        }
        Ok(k)
    }
}

#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> Option<(u64, u64, u64, u128)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino(), meta.size(), mtime_ns(meta)))
}

#[cfg(not(unix))]
fn file_id(_: &std::fs::Metadata) -> Option<(u64, u64, u64, u128)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Published vectors: FNV-1a of "" is the offset basis; of "a" the
    // values below. splitmix64 seeded 0 gives 0xe220a8397b1dcdaf first.
    #[test]
    fn hashes_match_their_published_vectors() {
        assert_eq!(fnv128(b""), 0x6c62272e07bb014262b821756295c58d);
        assert_eq!(fnv128(b"a"), 0xd228cb696f1a8caf78912b704e4a8964);
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
        let mut inputs = Inputs::default();
        assert_eq!(inputs.key(&a, false).unwrap(), key(b"GENE1\n"));
        // A rewrite (a new size) is seen, not served from the cache.
        std::fs::write(&a, b"GENE2 \n").unwrap();
        assert_eq!(inputs.key(&a, false).unwrap(), key(b"GENE2 \n"));

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
    }
}
