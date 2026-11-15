//! What the tests of an in-memory MCP session share: a writer to read the
//! replies back from, and the message of a tool call.

use serde_json::{json, Value};
use std::io::Write;
use std::sync::{Arc, Mutex};

/// A writer the test reads back.
#[derive(Clone, Default)]
pub struct Sink(pub Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The message calling the tool `name` with `args`, as request `id`.
pub fn call(id: u64, name: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args } })
}
