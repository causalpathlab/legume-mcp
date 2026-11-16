//! What the benchmark wrapper and executor tests share: stand-in tools,
//! and a plan task as `plan.json` lists it.

use serde_json::{json, Value};
use std::path::Path;

/// `body` written to `p` as an executable script.
pub fn write_exe(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A stand-in tool at `tool`, a sh script running `body`.
pub fn write_tool(tool: &Path, body: &str) {
    write_exe(tool, &format!("#!/bin/sh\n{body}\n"));
}

/// Task `id` as `plan.json` lists it: `argv` run in `dir`, with `threads`,
/// `mem_gb`, `timeout` seconds and `makes`.
pub fn plan_task(
    id: &str,
    argv: Value,
    dir: &Path,
    (threads, mem_gb, timeout): (u32, f64, u64),
    makes: &[&str],
) -> Value {
    json!({
        "id": id, "argv": argv, "env": {"RAYON_NUM_THREADS": threads.to_string()},
        "dir": dir, "timeout_secs": timeout, "makes": makes, "key": format!("key-{id}"),
        "mem_gb": mem_gb, "threads": threads, "gpus": 0, "point": {}, "rep": 0,
        "rep_seed": 1, "seed": 1, "stage": "s", "upstream": [],
    })
}
