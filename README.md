# legume-mcp

A small [Model Context Protocol](https://modelcontextprotocol.io) server over
stdio for the legume tools (lupin, senna, pinto, data-beans). A tool binary
implements `Handler` (its tools and how each is called) and hands it to
`serve`; agents such as Claude Code then run the tool's own workflow.

- JSON-RPC 2.0, one message per line; `initialize`, `ping`, `tools/list`,
  `tools/call`, and `prompts/list` and `prompts/get` when the handler has
  prompts.
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
offers the `notebook` tool for the agent's plan and observations; the
`legume` server's takes the interpretation too (below).

## The legume server

The tools' servers run the steps. What they share, the reasoning, is kept by
one more server in this crate: `legume mcp` (binary `legume`), registered
like the others (`claude mcp add legume -e LEGUME_MCP_PROJECT="$PWD" --
legume mcp`). It runs no steps. Its tools:

- `guide`: the guide to working with the legume tools and the person
  ([docs/collaboration.md](docs/collaboration.md)), its sections listed or
  one given, so a small model's context holds what it needs; `legume guide
  [section]` prints the same.
- `status`: where the project stands: its data, recent outputs, today's
  notebook, a question still waiting for the person's reply, and the
  hypotheses with how far each got.
- `hypotheses`: the hypotheses, or one's triangulation table.
- `notebook`, with every kind: a tool's own notebook takes `plan`,
  `observation` and `note`, and refuses the reasoning (`interpretation`,
  `answer`, `evidence`, `offer`, `choice`) with a pointer to this server.

Its instructions are the guide's quick card, short enough for a small local
model; the rest is a `guide` call away.

It offers two recipes as MCP prompts (`/mcp__legume__explore` and
`/mcp__legume__triangulate` in Claude Code): `explore` (a question and data,
both optional) works through a data set stage by stage, from a survey to
hypotheses; `triangulate` (a hypothesis, optional) tests one against new
sources of evidence until the person stops.

## legume chat

`legume chat` (feature `chat`: `cargo install legume-mcp --features chat`)
runs the legume tools with a model on this machine, through any
OpenAI-compatible server: llama-server, LM Studio, Ollama, MLX, vLLM. It starts the `legume` server and every legume tool on the PATH
(`<tool> mcp`, for the project), asks the model what to do, and routes each
call to its server; Ctrl-C stops the call or answer in progress.

`legume init` sets a project up for it: it finds the legume tools on the
PATH and a model server answering on this machine (or names the installed
ones and how to start them), and writes `.legume/chat.json` with the
model, its address, its context and the tools. With `--server 'llama-server
-m model.gguf --port 8080 -c 8192 --jinja'` the chat starts that server when
nothing answers, and stops it as it ends (its address, unless `--url` says,
is where that server listens: its `--port`, else its default); with `--model-file` the weights
are recorded with their SHA-256. It installs nothing, and replaces an
existing `.legume/chat.json` only with `--force`. `legume chat`'s own
arguments win over the file.

```text
legume init --model-file ~/models/qwen3-8b.gguf
legume chat                                  # the first local server that answers
legume chat --url http://127.0.0.1:8080/v1   # llama-server
legume chat --model qwen3-8b --context 32768 --tools data-beans,senna,lupin
```

A model that reads at most 16384 tokens (8192 unless `--context` says
more) gets the small chat, built to fit:

- a short card in place of the guide's quick card, and the project's
  standing from `status` (a question waiting for the person, the last
  notebook entries, flagged numbers, hypotheses) at the top of each turn: the
  records, not the conversation, are its memory, and the oldest turns are
  dropped to fit;
- single-purpose tools in place of the notebook's long schema: `plan`,
  `observe`, `propose`, `answer`, `evidence`, `offer`, `choice`, `status`;
- `toolbox` to list the servers and their tools, and `use_tools` to load a
  few (with that server's `job_wait`), so a server's tools are read only when
  needed;
- results cut to about 1200 characters, saying what was left out.

Built with feature `screen` (`cargo install legume-mcp --features
screen`), `legume chat` in a terminal has a screen of its own (`--plain`
keeps it to lines):

```text
 legume chat · qwen3-8b · 8192 tokens · small chat · pbmc      ⚑ 1  H 2  working 12s
┌ chat ─────────────────────────────────┐┌ steps · notebook · hypotheses ──────┐
│you  why is cluster 4 apart?           ││10:02 data-beans qc → pbmc.qc.zarr   │
│· plan {"text":"Why cluster 4 rather…  ││   ← data/pbmc.h5                    │
│  → Done.                              ││   cells 9812 of 11204 (3 MADs)      │
│· data-beans__qc {"input":"data/pb…    ││   ⚑ mito 0.31 (flagged: above 0.2)  │
│  … 41% of cells read                  ││                                     │
└───────────────────────────────────────┘└─────────────────────────────────────┘
 › ▏
 Ctrl-C stops the work · Tab records · PgUp/PgDn scroll · Ctrl-D leaves · Ctrl+Alt+D detach
```

The conversation is on the left, and the project's records beside it, read
from the files as they change. Tab goes from one to the next:

- **steps**: each output in `INDEX.tsv`, what it was made from (`←`) and
  the numbers it came with, a flagged one marked `⚑`;
- **notebook**: today's entries, and the question waiting for you;
- **hypotheses**: each source's word on each hypothesis (`✓` agrees, `✗`
  disagrees, `~` mixed), how far they take it (level 0 to 3) and the kinds
  of source not yet used.

The top line counts the flagged numbers and the hypotheses, and says when a
question waits for you or the model is working. A step that needs your
leave asks on the line below the chat: `y`, `n` or `a`. The chat runs in a
session you can leave running (Ctrl+Alt+D) while a long step or a question
waits, and come back to with `legume attach`; `legume sessions` lists them,
with what each is doing. `--no-detach` keeps it in this terminal only.

A larger model gets every server's tools (`<server>__<tool>`) and
instructions. A model server that takes no tools (or `--json`) is asked for
each action as one JSON object (`{"say": ..., "call": {"tool": ..., "args":
...}}`), which the chat checks before it runs anything; it switches to this
by itself when the server refuses tools.

What the chat does to stay safe, since a model's answers run tools on your
account:

- **Your leave for steps.** A call that only reads (a tool marked
  read-only, `job_status`, `job_wait`) or records the work (the `legume`
  server, the small chat's own tools) runs at once. Any other call waits
  for you: `y` runs it, `a` runs it and the tool's later calls, anything
  else declines and tells the model so.
- **Your words for your consent.** An `answer` or a `choice` is recorded
  only after you confirm what the model read in your line ("Record that
  you chose R1. …? [y]es, [n]o"); it quotes your line, not the model's
  account of it, and answers a question once. No notebook entry from the
  model may claim to be yours (`by` other than `agent` is dropped, in every
  server's notebook).
- **A model on this machine only.** The model server's address must be
  this machine's (localhost, 127.0.0.1, ::1): the chat sends it the
  conversation in plain text. On a shared machine, anyone can listen on a
  local port first; give `--url` for a server you started.
- **A settings file you own.** `.legume/chat.json` is read only when it is
  your own regular file that no one else can change, and the model server
  command in it runs only when you say so (or with `--start-server`).
  Tool names, and the model server's program, are found in the absolute
  folders of the PATH.
- **Clean output and records.** Control characters from a model or a tool
  never reach the terminal; what they say is quoted in `chat.md`, and text
  in any record cannot start an entry of its own or fetch an image.
- **Its children stopped whole.** The servers and the model server run in
  process groups of their own (Ctrl-C stops a call, not them); as the chat
  ends, each tool server is let finish its jobs, and a hangup or a
  terminate stops them all. Their logs are in the project's private
  `.legume/logs/`.

Each chat is recorded in `results/<day>/chat.md`: as it starts, the model
(its name, server, context and mode; its file and SHA-256 when known) and
the tools; then each line of the person's, each call with the first line of
its result, and each answer. The steps are in `runall.sh` as ever, so the
work is redone without the model; the reasoning is in `notebook.md`.

## legume report and legume graph

`legume report` makes a day's work into slides, from the records alone:
`report.typ` ([Typst](https://typst.app)) and the provenance graph
`graph.svg` in `results/<day>/report/`, and `report.pdf` when `typst` is on
the PATH (nothing is installed; without it, `typst compile --root .
results/<day>/report/report.typ` from the project folder makes the PDF
later). The folder is the report's own: it replaces no tool's output, and
building again replaces only what it made. In order:

1. the title, which says the work is exploratory: hypotheses, not findings;
2. the story, the one part an agent (or the person) writes, labelled as
   theirs: from `--story FILE`, or the `legume` server's `report` tool
   (`story`), kept in `results/<day>/report/story.md` (a story replaced
   is kept as `story.1.md`, `story.2.md`, ...);
3. the question: the day's plans;
4. the data: `data/README.md`;
5. the steps: the graph, then each output with what it was made from and
   the numbers it came with, the flagged ones in red and listed again;
6. the figures (images the steps wrote), each with the line of `runall.sh`
   that made it;
7. what was seen: the observations and notes;
8. the interpretation: the person's (accepted readings, overrides), or
   "not yet interpreted" with the readings proposed; a question still
   waiting; the next steps the person chose;
9. each hypothesis: its triangulation table, the verb it allows, its
   standing and the kinds of source not yet used;
10. replay: `bash results/<day>/runall.sh`, the tools' versions, the model
    of the chat (not needed to replay) and where each record is.

Every word from the records goes into the Typst source as a string, so no
record, however written, is read as markup or code.

`legume graph` gives the graph on its own, from `results/INDEX.tsv`: each
step (the rows one tool wrote at one time) between the files it read and
made, data no step made in grey, a step that flagged a number in red.

```text
legume report                       # today's
legume report --day 2026-10-28 --story story.md --no-pdf
legume graph --format text          # or svg (the default), dot; --all for every day
```

## Hints for the agent and the client

- **Annotations.** `ToolSpec::new(..).read_only()` for a tool that changes
  nothing, `.writes()` for one that writes new files and replaces none (every
  legume step), `.destructive()` for one that removes or stops something;
  `.idempotent()`, `.open_world()` and `.titled(..)` too. They go to
  `tools/list` as MCP's `annotations`, which clients use to decide what to
  ask the person first. A tool without them sends none. The built-in tools
  have theirs (`job_cancel` is destructive; `notebook` writes).
- **Next.** A result may end with `Content::Next(vec![Next::new(tool, args,
  why)])`: the calls that could come next, on this server or another
  (`"lupin annotate_run"`). The agent reads them as a numbered list headed
  "Next, if the person agrees", to propose rather than run; a client such as
  `legume chat` reads them under the result's `_meta` as `legume/next`.
  `job_status` and `job_wait` give theirs: wait while a job runs, record an
  observation once it is done or failed.
- **Prompts.** `Handler::prompts` (none by default) gives recipes as MCP
  prompts: `Prompt::new(name, title, description, text).arg(name,
  description, required)`, with `{name}` in the text where each argument
  goes; one not given reads "not given".

## Steps: inputs and facts

A step's row in `INDEX.tsv` names what it read and the key numbers it made,
so each output traces back to its data and a reader sees the numbers that
matter without opening it. `run_making` and `spawn_job` take them from a
`Step`, through `Call::run_step` and `Call::spawn_step`:

```rust
let step = Step::new(&outputs, "senna fit, 12 topics")
    .inputs(&[counts, batches])                    // else: read from the command
    .fact(Fact::count("cells", 9812).of(11204).rule("3 MADs"))
    .after(|stdout, outputs| vec![Fact::value("loss", 1.83).flag("did not converge")]);
cx.run_step(cmd, step)?;                           // or cx.spawn_step(cmd, step)
```

- Without `inputs`, they are the existing files and folders the command line
  names (`--flag=value` too), other than the outputs and the folders they are
  in (`legume_mcp::inputs_of`). A session job's are read from its `record`.
- `after` finds the numbers known once the step is done; it runs only when
  the step succeeded. A session job's TUI gives them as it ends:
  `Reporter::finish_with(end, facts)`.
- A fact is a count or a value, out of how many (`of`), by which `rule`, and
  a `flag` when it looks wrong. The `legume` server's `status` shows each
  output's facts and lists the flagged ones apart, for the agent to raise
  with the user before building on them.

`INDEX.tsv` keeps the columns `date time server tool output summary inputs
facts` (`legume_mcp::project::INDEX_HEAD`). A file begun before 0.5.0 (six
columns) is renamed `INDEX.before-0.5.tsv` when the next row is written, so
the new one is a whole table (`INDEX.before-0.5.2.tsv`, ... should an older
server begin another); the readers (`status`, the screen, `legume report`
and `legume graph`) read those rows too.

## Long steps as jobs

A step longer than a client waits on one call (a model fit, a large pass)
runs as a background job: the tool calls `Call::spawn_job(cmd, outputs,
summary)` in place of `run_making` and returns the job's id (`job-N`) at once.
Every server offers the agent `job_status`, `job_wait` (blocks up to 120 s; call
again while it runs) and `job_cancel`, and its instructions say how to use
them. A job's log goes to the feed under its id, where a person can stop it
too; a job that succeeds is recorded when it ends, like any step (`runall.sh`,
`INDEX.tsv`); one cancelled or failed is not. These jobs end with their
server.

With feature `sessions` (Unix), a tool can run the step in a detached session
of its own TUI instead: `Call::start_session_job(SessionJob { tool, screen,
label, watch, record, outputs, summary })` starts `watch` (the tool's own
binary, as `senna run` or `lupin annotate` runs it on screen) in a new
legume-tui session and returns the job's id and the session's. A person runs
`<tool> attach <session>` to see the usual screen; the agent follows it with
the same `job_status`, `job_wait` and `job_cancel` (its status adds `session`
and `attach`), and a cancel stops the session as `<tool> kill` does. The TUI
reports through `job::Reporter::from_env()`: `line` for its log, and `finish`
with how it ended, which records the step (`record`, the command without the
screen) as `spawn_job` does: run only when it is done and every output is
there. Such a job outlives its server: it records itself as it ends, and the
next server for the same project takes up the ones still running or ended
unseen. Their folders are in `jobs` beside the watch folders.

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
  held by a job, for a run the agent did not name. `free_prefix(dir, base,
  plan, taken)` picks the same way in any folder (the same closures, no job
  holds), for a tool's own screen with no server, so its runs number as an
  agent's do.

With the `clap` feature, `legume_mcp::cli` lists a clap command's flags for the
agent (`settable`, `describe`) and turns the agent's JSON map of flags into the
words clap reads (`to_argv`: switches named when true, values as one
`--flag=value` word, lists by the flag's delimiter, null left at the default;
the tool's own arguments, such as its data files and output prefix, pointed
to by name). `check` parses the result as the tool would, returning clap's
error on one line under the name of the command it refuses (`app sub` for
a subcommand): what is wrong, the choices and clap's tips, without the usage
and the pointer to `--help`; help asked for comes back whole.

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
  results/INDEX.tsv        one row per output: date, time, server, tool, output, summary,
                           inputs (`;` between), facts (JSON)
  results/HYPOTHESES.tsv   the readings the person chose, numbered H1, H2, ... for the project
  results/EVIDENCE.tsv     what each source of evidence says of a hypothesis
  results/<YYYY-MM-DD>/    one folder per day, shared by every server
    runall.sh              every step that wrote something, in order, from every server
    notebook.md            the agents' account: plan, observations, interpretation
    chat.md                `legume chat`'s conversations: the model, who said and called what
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
  records what it did and saw. What the results mean is the person's to say,
  and is kept by the `legume` server's notebook: the agent proposes two or more `readings` (each a `claim`, its `evidence` as
  typed items `{kind, term}`, the `alternatives` with `why_not`, a
  `rationale` and the `check` that would tell it from the others), which the
  notebook numbers R1, R2, … under `interpretation (proposed)`, and asks which
  hold. A number, a short line or a pick from a multiple-choice question (when
  the client has them) is an answer. The agent records it as kind `answer`:
  `chose` (the reading numbers, none when none) and `words` (the reply
  exactly as given). The notebook links it to the day's latest proposal,
  checks the numbers against it, and says who decided in lupin's
  `decided_by` terms: `accepted by the user` (agent_proposed_user_accepted)
  when readings were chosen, `user override` when none were. Nothing is left
  to the agent to pick, so a small model gets it right. Other choices the
  agent puts to the person (which step next, whether to look further) are
  an `offer` of numbered `options`, and the reply a `choice` (`chose` and
  `words`), linked and checked the same way. A reply of the wrong kind for
  the latest question is refused, so a "2" to an offer is never taken as
  R2. The readings chosen become the project's hypotheses, H1, H2, ...
  (`results/HYPOTHESES.tsv`; a claim already there keeps its number). The
  agent records what each source of evidence says of one as kind
  `evidence`: `hypothesis`, `source` (a name for the data, e.g. "held-out
  donors 5-6"), `class` (our_cells, same_cells, same_donors, other_donors,
  other_study, genetics, perturbation, curated), `verdict` (agrees,
  disagrees, mixed) and its numbers in `text`, kept in
  `results/EVIDENCE.tsv`. The notebook builds the hypothesis's
  triangulation table from them, one row per source (several tools on the
  same data are one source), says which verb the agreeing sources allow
  (marks or associated in these donors; associated, replicated, once
  sources that share no cells agree; affects, with genetics or a
  perturbation too) and names what disagrees; the agent is also told the
  kinds of source not yet used, to offer next. A note in the person's own
  words takes `by` `user`. An agent's interpretation without
  readings is refused; the old kind `conclusion` is taken as a proposed
  `interpretation` with text alone.
- `LEGUME_MCP_RECORDS=off` keeps no records.

[1] W. S. Noble, "A quick guide to organizing computational biology projects",
PLoS Comput Biol 5(7): e1000424 (2009). doi:10.1371/journal.pcbi.1000424

[2] W. S. Noble, "Ten simple rules for defining a computational biology
project", PLoS Comput Biol 19(1): e1010786 (2023). doi:10.1371/journal.pcbi.1010786
