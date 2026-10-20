# legume-mcp

A small [Model Context Protocol](https://modelcontextprotocol.io) server over
stdio for the legume tools (lupin, senna, pinto, data-beans). A tool binary
implements `Handler` (its tools and how each is called) and hands it to
`serve`; agents such as Claude Code then run the tool's own workflow.

- JSON-RPC 2.0, one message per line; `initialize`, `ping`, `tools/list`,
  `tools/call`. Tools only.
- A tool's failure is a result with `isError` and the error chain as text.
- `Call::progress` sends progress notifications when the client asks for them;
  `Call::run` runs a child process, passes its stderr on as progress, and kills
  it when the client cancels.
- stdout carries the protocol: tools must log to stderr and run anything that
  prints as a child.
- Only serde_json, anyhow, base64 and log; no async runtime.

```rust
use legume_mcp::{serve, Call, Content, Handler, ServerInfo, ToolSpec};
use serde_json::{json, Value};

struct Tools;

impl Handler for Tools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::new("echo", "Return the arguments", json!({ "type": "object" }))]
    }
    fn call(&mut self, _name: &str, args: Value, _cx: &Call) -> anyhow::Result<Vec<Content>> {
        Ok(vec![Content::Json(args)])
    }
}

fn main() -> anyhow::Result<()> {
    serve(ServerInfo { name: "demo".into(), version: "0.1.0".into(), instructions: None }, Tools)
}
```

Register a server with a client, e.g. `claude mcp add lupin -- lupin mcp`.
