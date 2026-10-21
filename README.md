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

## Connecting any client

A legume server is a plain MCP server on stdio, so any MCP client and any model
can use it: `lupin mcp`, `senna mcp`, `pinto mcp`, `data-beans mcp`. A client
needs the command and where the project is. The records go to the server's
working directory, or to `LEGUME_MCP_PROJECT` when set; set it whenever the
client may start servers elsewhere (desktop apps usually do). Most clients
(Claude Desktop, Cursor, Gemini CLI, and others) take this shape:

```json
{
  "mcpServers": {
    "lupin": {
      "command": "lupin",
      "args": ["mcp"],
      "env": { "LEGUME_MCP_PROJECT": "/path/to/project" }
    }
  }
}
```

Claude Code: `claude mcp add lupin -e LEGUME_MCP_PROJECT="$PWD" -- lupin mcp`.
VS Code takes `"servers"` with `"type": "stdio"` in `.vscode/mcp.json`; Codex
CLI takes `[mcp_servers.lupin]` with `command`, `args` and `env` in
`~/.codex/config.toml`. Key names differ between clients; the server needs only
the command, its arguments and, optionally, the environment.

Each server sends its workflow to the agent when it connects (the MCP
`instructions`, with the records note below added). A client that does not pass
those to its model can be given the same text in the agent's own instructions
(an `AGENTS.md`, a system prompt); each tool's README has it. Every server also
offers the `notebook` tool for the agent's plan, observations and conclusions.

## Long steps as jobs

A step longer than a client waits on one call (a model fit, a large pass)
runs as a background job: the tool calls `Call::spawn_job(cmd, outputs,
summary)` in place of `run_making` and returns the job's id (`job-N`) at once.
Every server offers the agent `job_status`, `job_wait` (blocks up to 120 s; call
again while it runs) and `job_cancel`, and its instructions say how to use
them. A job's log goes to the feed under its id, where a person can stop it
too; a job that succeeds is recorded when it ends, like any step (`runall.sh`,
`INDEX.tsv`); one cancelled or failed is not. Jobs end with their server: a
step longer than a client session is the tool's own command line's to run.

While a job runs, its outputs are held: a step writing one of them (the same
file, or one inside the other, however spelled) is refused, naming the job,
until the job ends. `run_making` and `spawn_job` check this themselves; a tool
that writes an input file beside its outputs first, or chooses an output name
itself, asks `Call::ensure_free(outputs)` before it does.

## A tool's command line

Most tools run their own CLI as the child of a step. A few helpers keep that
the same from server to server:

- `serve_in_project(info, handler)` is `serve` from inside the project: the
  server changes into `project::root()` first, so the relative paths an agent
  gives are read from the project, as the records are. `project::enter` does
  that step alone (making the folder when it is new), for a server built on
  `serve_with`; `project::root_from_env` gives the records' root as an
  absolute path.
- `parse_args(args)` reads a call's arguments into the tool's type, or says
  what is wrong with them; `shown_path` gives a path from the root, and
  `command_line(argv)` a line to paste into a shell.
- `Call::free_prefix(base, plan, taken)` picks the first of `base`, `base-2`,
  ... in today's results folder whose planned outputs neither exist nor are
  held by a job, for a run the agent did not name.

With the `clap` feature, `legume_mcp::cli` lists a clap command's flags for the
agent (`settable`, `describe`) and turns the agent's JSON map of flags into the
words clap reads (`to_argv`: switches named when true, values as one
`--flag=value` word, lists by the flag's delimiter, null left at the default;
the tool's own arguments, such as its data files and output prefix, pointed
to by name). `check` parses the result as the tool would, returning clap's
error as it renders it.

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

A viewer follows with `watch::Agent`, which has no UI in it: the calls and
their status, the notebook, the tool's view (`take_new_state` hands over the
latest view once, when it changed since the last take), the steps recorded,
following or paused, and steering through
`act(Action)` and `note`. `lines(n, width)` gives a sidebar. With the `tui`
feature, `watch::view::AgentPane` draws it as a pane, with keys the host can
rebind (`Keys`), and `watch::view::run` gives `<tool> mcp watch`.

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
