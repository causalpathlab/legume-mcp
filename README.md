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

## Watching and steering

While an agent works, each server writes what it is doing to an owner-only
folder, `$XDG_RUNTIME_DIR/legume-mcp/<server>-<pid>/`: `meta.json`, a feed of
events (`feed.jsonl`: calls, progress, results, the tool's view, steps added
to `runall.sh`, notebook entries, notes), the tool's current view
(`state.json`, published with `Call::state`) and a control socket. A person
follows it from a terminal with `legume_mcp::watch` (`live`, `follow`,
`state`) and can steer through `watch::control`: `stop` a call (its child is
killed and the agent is told the user stopped it) or leave a `note` (it comes
with the agent's next tool result and goes into the notebook). The feed is
for watching; the records below stay the account to redo the work from.
`LEGUME_MCP_WATCH=off` writes none; a path puts the folders under it.

## Project records

Every server keeps the same records under the project (its working
directory, or `LEGUME_MCP_PROJECT`), laid out after Noble's guide to
organizing computational biology projects [1], so an agent's work can be
found again and redone without the agent:

```text
<project>/
  data/README.md           an input's source and date, as tools import it (Call::provenance)
  results/INDEX.tsv        one row per output: date, time, server, tool, output, summary
  results/<YYYY-MM-DD>/    one folder per day, shared by every server
    runall.sh              every step that wrote something, in order, from every server
    notebook.md            the agents' account: plan, observations, conclusions
    …                      outputs the agent did not name (Call::results_dir)
```

- `runall.sh` holds each tool call as a comment, each `Call::run` /
  `Call::run_making` child as its command line (the server's binary under its
  name, project paths relative, skipped when its outputs exist, their folders
  made first), and each `Call::write_file` input as a heredoc (base64 when it
  holds control bytes). Steps that did not succeed stay as comments. A copy of
  the project replays it: `bash results/<day>/runall.sh`.
- Environment values whose names look secret (`*TOKEN*`, `*KEY*`, …) are not
  written down; the script asks for them at replay. The script is owner-only.
- `notebook` is a built-in tool: the agent opens the day with a plan (the
  question, the data, and how the result will be checked, after [2]), then
  records what it did and saw, and its conclusions.
- `LEGUME_MCP_RECORDS=off` keeps no records.

[1] W. S. Noble, "A quick guide to organizing computational biology projects",
PLoS Comput Biol 5(7): e1000424 (2009). doi:10.1371/journal.pcbi.1000424

[2] W. S. Noble, "Ten simple rules for defining a computational biology
project", PLoS Comput Biol 19(1): e1010786 (2023). doi:10.1371/journal.pcbi.1010786
