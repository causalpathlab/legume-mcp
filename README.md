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

`cargo install legume-mcp` installs the `legume` command: `legume init`
connects a project to the agents ([below](#connecting-a-project-legume-init)),
and the agents start the [legume server](#the-legume-server) (`legume mcp`).
The person asks the agent; there is nothing else to type.

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
(an `AGENTS.md`, a system prompt); each tool's README has it. The project's
one notebook is the `legume` server's (below); the tools' servers have none.

## The legume server

The tools' servers run the steps. What they share, the project's notebook,
is kept by one more server in this crate: `legume mcp` (binary `legume`),
which `legume init` connects. It runs no steps. Its rule for agents: the
person thinks; the agent runs the tools, shows the facts, asks and checks,
and records the person's words, never its own interpretation. Its tools:

- `guide`: the guide to working with the legume tools and the person
  ([docs/collaboration.md](docs/collaboration.md)), its sections listed or
  one given.
- `status`: where the project stands: every file under `data/` with where it
  came from and the person's verdict, recent outputs and their key numbers,
  today's notebook, the hypotheses with how far each got, the figures kept
  for the paper, and what is open for the person; with `hypothesis`, its
  triangulation table.
- `report`: the day's slides ([below](#the-report)).
- `notebook`: the project's notebook ([below](#project-records)).

Its instructions are the guide's quick card; the rest is a `guide` call away.

It offers two recipes as MCP prompts (`/mcp__legume__explore` and
`/mcp__legume__triangulate` in Claude Code): `explore` (a question and data,
both optional) starts from where the project stands and works in steps,
asking the person what they expect before each result and what it means
after, until their summary and the slides; `triangulate` (a hypothesis, optional) tests one against new
sources of evidence until the person stops.

## Connecting a project: legume init

`legume init`, in a project's folder, lists the `legume` server and each
legume tool on the PATH that serves MCP (`<tool> mcp`) in the project's own
MCP settings: `.mcp.json` for Claude Code and `.cursor/mcp.json` for Cursor's
`agent`. It writes those of the agents on the PATH (both when neither is
there; `--claude` or `--cursor` to choose); servers already listed there are
kept, and nothing is installed. The agent started in that folder then has
them all, after asking once before it runs a project's servers.

```text
$ legume init
Project: /Users/me/work/pbmc
Servers: legume, data-beans, senna, lupin, pinto, cocoa, chickpea, faba
  not included (not on the PATH, or no MCP server): mung
Wrote .mcp.json for Claude Code
Wrote .cursor/mcp.json for Cursor

Start `claude` in this folder: it asks once before it runs the project's servers.
Start `agent` in this folder: it asks once before it runs the project's servers.
```

## The report

The `legume` server's `report` makes a day's work into slides, from the
records alone, in `results/<day>/report/`: `report.pdf`, typeset by `legume`
itself (Typst is built in, with its fonts: nothing to install; feature
`pdf`, on by default), `report.md`, the same slides as text for any editor,
and `graph.svg`, the trail of the day. The folder is the report's
own: it replaces no tool's output, and building again replaces only what it
made. In order:

1. the title, which says the work is exploratory: hypotheses, not findings;
2. the person's summary of the day, in their words;
3. their question: the day's plans;
4. the data: each file under `data/`, where it came from and their verdict;
5. the trail: drawn like a hidden Markov model, the person's entries in
   order (their question, verdicts, hypotheses, evidence, figures,
   summary), each over the steps that ran and what was seen after it;
   steps that came before any entry stand under "No question yet". Then
   each output with what it was made from and the numbers it came with,
   the flagged ones marked and listed again;
6. the figures they kept, by number, with their take-home, then the day's
   other images, each with the line of `runall.sh` that made it;
7. what was seen: the observations and notes;
8. each hypothesis, as they put it: its rivals, what would nullify it, its
   triangulation table with their reason for each source, the verb it
   allows and the kinds of source not yet used;
9. what is open for the person;
10. replay: `bash results/<day>/runall.sh`, the tools' versions and where
    each record is.

The agent writes none of it. The trail (section 5) draws a step that
flagged a number in red, the agent's observations dashed, and a faint line
where a step read a file another made. Every word from the records goes
into the Typst source as a string and into the Markdown escaped, so no
record, however written, is read as markup or code.

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
  "Next, if the person agrees", to propose rather than run; a client can
  read them under the result's `_meta` as `legume/next`.
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
server begin another); the readers (`status` and the report) read those
rows too.

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

Every server keeps the same records under the project (the nearest folder
`legume init` marked with `.legume/`, from the working directory up; else
the working directory, or `LEGUME_MCP_PROJECT`), laid out after Noble's
guide to organizing computational biology projects [1], so an agent's work
can be found again, redone without the agent, and written up:

```text
<project>/
  .legume/                 marks the project (legume init)
  data/README.md           an input's source and date, as a tool imported it (Call::provenance)
                           or as the person said it
  results/INDEX.tsv        one row per output: date, time, server, tool, output, summary,
                           inputs (`;` between), facts (JSON)
  results/DATA.tsv         the person's verdict on each data file, and why
  results/HYPOTHESES.tsv   the person's hypotheses, H1, H2, ..., with rivals and what would
                           nullify each (a revision is a new row; the latest counts)
  results/EVIDENCE.tsv     what each source of evidence says of a hypothesis, and why the
                           person judged it so
  results/FIGURES.tsv      the figures and tables the person kept for the paper
  results/<YYYY-MM-DD>/    one folder per working day (it turns over at 5 a.m.)
    runall.sh              every step that wrote something, in order, from every server
    notebook.md            the day's account
    report/                the day's slides
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
- `notebook` is the `legume` server's, the project's only one. The agent
  writes `observation`s (what it did and saw, with numbers) and `note`s.
  Every other kind is the person's, in their own words (`words`), which the
  notebook requires and quotes: `plan` (their question as a contrast, and
  what would change their mind, after [2]), `data` (where a file came from
  and their verdict: good, caveats, bad), `hypothesis` (their claim, the
  `rivals` they name and what would `nullify` it; numbered H1, H2, ... for
  the project; revised by number), `evidence` (what one source says of a
  hypothesis: `source`, `class`, the person's `verdict`, the numbers in
  `text` and their reason in `words`), `figure` (an `output` they keep for
  the paper, its `number` and take-home) and `summary` (their day). A
  missing rival, "would nullify it" or claim for a figure is not refused:
  the result tells the agent what to ask, and `status` lists it as open.
  From the evidence the notebook builds the hypothesis's triangulation
  table, one row per source (several tools on the same data are one
  source), says which verb the agreeing sources allow (marks or associated
  in these donors; associated, replicated, once sources that share no cells
  agree; affects, with genetics or a perturbation too), names what
  disagrees, and tells the agent the kinds of source not yet used, to put
  to the person.
- `LEGUME_MCP_RECORDS=off` keeps no records.

[1] W. S. Noble, "A quick guide to organizing computational biology projects",
PLoS Comput Biol 5(7): e1000424 (2009). doi:10.1371/journal.pcbi.1000424

[2] W. S. Noble, "Ten simple rules for defining a computational biology
project", PLoS Comput Biol 19(1): e1010786 (2023). doi:10.1371/journal.pcbi.1010786
