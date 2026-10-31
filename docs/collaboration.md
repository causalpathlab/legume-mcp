# Working together: people, agents and the legume tools

A white paper on how a person and an AI agent analyse data together with the
legume tools (faba, data-beans, senna, pinto, lupin, chickpea, cocoa), and how
the work is kept so anyone can check it, run it again and write it up. The
tools run the steps; the `legume` server (`legume mcp`, from legume-mcp)
keeps the project's notebook, and serves this paper a section at a time (its
`guide` tool).

It is written for two readers:

- **People** who run analyses with an agent and want to know what to expect.
- **Agents**. The quick card below and sections 4 to 11 are the rules an
  agent follows. They are plain steps on purpose. If you are an agent, read
  them fully before you call a tool.

Some features are new in legume-mcp 0.5.0 or still planned; section 14 lists
what is available now. For how to call a tool's own functions (its
arguments, what `out` does), follow that tool's instructions. For the
notebook, follow this paper.

**One notebook.** The `legume` server's `notebook` is the project's only
one: the tools' servers have none. It writes `results/<day>/notebook.md`.

**The rule.** The person thinks; the agent runs the tools, shows the facts,
asks and checks. The agent never writes what a result means, a hypothesis, a
verdict, a figure's message or a summary: it asks, and records the person's
words. Science is also how the person learns, so the agent explains what
each number measures and asks the person to predict before it shows a result.

---

## Quick card for agents

```text
 1. First, call `status` on the legume server and tell the person where the
    project stands, in plain words: the data and what is not recorded about
    it, what was done, the hypotheses, and what is open for them. If the
    project folder looks wrong (source code, your home folder), say so.
 2. Ask the person's question, as a contrast (this rather than that), and
    what result would change their mind. Record their words: notebook kind
    "plan" with "words".
 3. Data whose origin or quality they have not given: run the read-only
    checks, explain each number (what it measures, what is typical), say
    what argues against the data, and ask. Record their words: kind "data"
    with "path", "words" and their "verdict" (good, caveats, bad).
 4. Read-only tools before steps. Never overwrite; if a tool refuses, read
    why and change the call. Before a step, ask what they expect to see.
    A cutoff is the person's to choose: show what each choice removes (a
    dry run with its figure, or the tool's own view, section 7), then run
    the step with their settings, so it is recorded.
 5. A long step runs as a job: follow it with job_wait until it is done, and
    tell the person plainly what is running and about how long it takes.
 6. After a step: kind "observation", with numbers and thresholds. This is
    yours: what was done and seen, not what it means.
 7. Never say what a result means, even if asked to. Ask. When the person
    states a hypothesis, ask what else could explain it and what would
    nullify it. Record their words: kind "hypothesis" with "words",
    "rivals" and "nullify"; it is numbered H1, H2, ... (section 5).
 8. To test a hypothesis, tell the person which kinds of source could
    corroborate or nullify it and what each shares with our cells
    (section 8); they choose. Show the numbers, ask whether it agrees,
    disagrees or is mixed, and why. Record kind "evidence" with
    "hypothesis", "source", "class", "verdict", the numbers in "text" and
    their reason in "words". The result is its table, the verb it allows
    and the sources not yet used.
 9. Use only the verb the table allows. "Drives" only with genetic,
    perturbation or applied-treatment evidence.
10. When the person wants a figure or table for the paper: have the tool
    write it as SVG or PDF (data-beans `qc` or `statistics` with `figure`
    ending .svg; senna or pinto `view_export` with `pdf`), so it stays sharp
    on slides and in print. Then kind "figure" with "output", their "number" (2B) and
    their take-home in "words"; ask which hypothesis it supports.
11. At the end of the day: ask for their summary (what changed, what argues
    against it, what is next) and record their words, kind "summary". Then
    build the slides with `report` and tell them where the records and
    the slides are. A result that ends "Next, if the person agrees" lists
    calls to propose, not to run.
```

---

## 1. The idea in one paragraph

The aim is **causal triangulation**: a claim about why cells, donors or
conditions differ, supported by several independent sources of evidence
(section 8): other donors, another measurement, another study, another kind
of study, each with its own bias. One data set carries its own bias however
many tools read it, so several tools on the same data are one source. When
sources with different biases agree, the claim is stronger; when they
disagree, the disagreement shows which bias to look at. The tools compute
facts from a source. The agent runs the tools, shows the facts, asks the
questions that make the person think, and checks what they write against the
records. The person decides what the results mean, which sources to pursue,
and how far to go, and writes it in their own words. Everything is written
down in the project folder, so the work can be checked, run again and
written up as a paper. The records follow two guides by W. S. Noble: how to
organise a computational biology project (2009) and how to define one
(2023).

## 2. Who does what

| | Does | Never does |
|---|---|---|
| **Tool** | Computes and states facts: counts, fits, scores, tables, figures. Records every step it runs, with its inputs and key numbers, and flags a number that looks wrong. Refuses bad input with a message that says what to fix. | Decides what a result means. Overwrites an earlier result. |
| **Agent** | Runs the tools, reads results, explains what each number measures, asks what the person expects and what they think, records what was done and seen, records the person's words, checks them against the records, and names what argues against. | Says what a result means. Writes the person's hypothesis, verdict, take-home or summary. Rewrites their words. Hides a failed or doubtful step. |
| **Person** | Sets the question. Judges the data. States hypotheses and their rivals. Judges each source of evidence. Keeps the figures for the paper. Summarises the day. | Has to remember what was done: the records do that. |

The short version: **tools state facts, the agent asks and checks, the
person thinks and writes.**

## 3. The project folder

All work happens in one project folder. Every tool writes its records there.

```text
<project>/
  .legume/                marks the project (legume init)
  data/                   inputs
  data/README.md          where each input came from, in the person's words or a tool's
  results/
    INDEX.tsv             one row per output: date, time, server, tool, file, summary,
                          the files it was made from, its key numbers
    DATA.tsv              the person's verdict on each data file, and why
    HYPOTHESES.tsv        the person's hypotheses, H1, H2, ..., with their rivals and
                          what would nullify each
    EVIDENCE.tsv          what each source of evidence says of a hypothesis, and why
                          the person judged it so
    FIGURES.tsv           the figures and tables the person kept for the paper
    <YYYY-MM-DD>/         one folder per working day (it turns over at 5 a.m.)
      runall.sh           every step of the day, in order; running it repeats the work
      notebook.md         the day's account: plan, data, observations, hypotheses,
                          evidence, figures, summary
      report/             the day's slides: report.pdf, report.md, graph.svg
      ...                 outputs the agent did not name
```

What each record is for:

- **`runall.sh`** repeats the work without the agent:
  `bash results/<day>/runall.sh`. Steps that failed stay in it as comments.
- **`notebook.md`** is the story of the day. The agent's entries say what was
  done and seen; everything else is the person's words, quoted.
- **`INDEX.tsv`** says which file came from which tool, with a one-line
  summary, the files it was made from (`inputs`) and its key numbers
  (`facts`: cells kept of how many, hits at which threshold). A tool flags a
  number that looks wrong; `status` lists the flagged ones. Raise them with
  the person before building on that output. Following `inputs` back tells
  whether two results come from the same data, and so are one source
  (section 8.3).
- **`data/README.md`** and **`DATA.tsv`** say where each input came from and
  whether the person judged it good.
- **`HYPOTHESES.tsv`** and **`EVIDENCE.tsv`** are the project's
  triangulation ledger: they outlive the day, so a hypothesis keeps its
  number and its table across days.
- **`FIGURES.tsv`** lists what the person kept for the paper, by number,
  with the claim each supports and its take-home.

Two practical rules:

- The project folder is the one `legume init` marked, found from any
  subfolder the agent starts in; else the server's working folder, or the
  folder named by the environment variable `LEGUME_MCP_PROJECT`.
- Records are only ever added to. Nothing in them is rewritten; a hypothesis
  revised is a new row, the latest one counts.

## 4. The working day

**Start.**

1. **Where we are.** Call `status` and tell the person, in plain words, what
   the project holds and what is open for them. If the project folder looks
   wrong (a folder of source code, with files such as `Cargo.toml` and
   `src/`; a home folder), say so first.
2. **The question.** Ask it as a contrast (fact and foil, section 8.1), and
   what result would change their mind. Record their words as the `plan`;
   you may add the data and the checks, as facts, in `text`.
3. **The data.** For each file whose origin or quality is not recorded: run
   the read-only checks (data-beans `info`, `qc` without `out`,
   `statistics`), explain what each number measures and what is typical,
   name what argues against the data (one sample, already filtered, an
   unknown source), and ask. Record their words as `data`.

**During.**

4. **Before a step, ask what they expect.** The gap between the expectation
   and the result is where they learn.
5. **Run the step.** A long step runs as a job (section 10, rule 5): tell the
   person plainly what is running and about how long.
6. **Observe.** Record kind `observation`: what you did and saw, with
   numbers. Show it to the person; ask what they see.
7. **Their hypotheses.** When the person says what they think a result
   means, record it as a `hypothesis` in their words, with the rivals they
   name and what would nullify it (section 5).
8. **Triangulate.** For a hypothesis worth more work, follow section 8: they
   choose the sources, judge each, and the table sets the verb.
9. **Figures.** When a figure or table is worth publishing, the person keeps
   it as a `figure` (section 6).

**End.**

10. **Their summary.** Ask what changed today, what argues against it, and
    what is next; record their words as the `summary`. Build the slides
    with `report`, and tell them where the records and the slides are.

Examples:

```json
{"tool": "notebook", "args": {"kind": "plan", "words": "Why do the treated donors' monocytes differ from the controls'? I'd drop it if donors 5 and 6 don't show it.", "text": "Data: data/pbmc.zarr.zip, 6 donors (3 control, 3 IFN). Check: held-out donors 5 and 6."}}

{"tool": "notebook", "args": {"kind": "observation", "text": "QC kept 9,812 of 11,204 cells (12% dropped). Donor 4 lost 30%, the others 8-11%."}}
```

## 5. Hypotheses, in the person's words

A hypothesis is the person's: a claim about why something differs, put as a
contrast. The agent never proposes one; it asks, and records what the person
says.

When they state one:

1. Record it: `{"kind": "hypothesis", "words": "<their claim, as they said it>"}`.
   It is numbered H1, H2, ... for the whole project.
2. Ask **what else could explain it**: batch, quality, one donor, chance, a
   cell state rather than a type. Record their answer as `rivals`.
3. Ask **what result would make them drop it**, before looking further.
   Record it as `nullify`.
4. Revise a hypothesis by its number: `{"kind": "hypothesis", "hypothesis":
   "H1", "rivals": [...], "nullify": "..."}`. The latest version counts; the
   earlier ones stay in the records.

Missing rivals or a missing "would nullify it" are not refused: the result
tells you the question to ask, and `status` lists it as open until it is
answered.

Example:

```json
{"tool": "notebook", "args": {
  "kind": "hypothesis",
  "words": "Topic 7 is higher in treated monocytes because of the interferon response",
  "rivals": ["two treated donors carry most of it", "a batch effect"],
  "nullify": "donors 5 and 6 show no rise in ISG15 or MX1"}}
```

A call the tool refuses: `{"kind": "hypothesis", "text": "Topic 7 is clearly
interferon."}` (no `words`: the agent's own version is not the person's).

## 6. Data, figures and the day's summary

The other kinds the person writes:

| What | Call |
|---|---|
| Where a file came from, and their verdict | `{"kind": "data", "path": "data/pbmc.h5", "words": "the core's 10x run of donors 1-6", "verdict": "caveats"}` |
| A figure kept for the paper (first have the tool write it as SVG or PDF: a PNG blurs on a slide) | `{"kind": "figure", "output": "results/2026-10-28/umap.svg", "number": "2B", "hypothesis": "H1", "words": "cluster 4 sits apart from the other monocytes"}` |
| The day's summary | `{"kind": "summary", "words": "<what changed, what argues against it, what is next, as they said it>"}` |
| Anything else they said | `{"kind": "note", "by": "user", "text": "<their words>"}` |

Always the person's own words: do not shorten, fix or summarise them. When
they reply only "ok" or "continue", nothing is recorded: ask again.

## 7. When someone asks the agent to decide

Sometimes the person says "just tell me what it means" or "you decide".
The agent still does not decide. It answers like this:

> What do you see in it? Here are the facts: ... Things that often explain a
> pattern like this are ...; what would tell them apart is ... Which do you
> think holds, and why?

Naming the usual explanations and what would tell them apart is fine; it
helps the person think. Choosing one for them is not, and nothing is
recorded as theirs that they did not say.

Cutoffs and thresholds are the person's choice as well. Show them what
each choice would do: a dry run, with its figure. Where the tool has a
view of its own for the choice, offer it. In data-beans `qc` on a
terminal, the person moves each rule's edge, sees how many cells only it
removes, and presses `p` for the flags that pick the same cells. Then
run the step through the tool's server with those settings, so
`runall.sh` repeats it: a choice made in a view and never run here is
not in the records.

## 8. Explaining by contrast, checking by triangulation

This section is the reasoning behind the loop. Agents: the rules in it are
marked **Rule**.

### 8.1 Every question is a contrast

A result is never explained on its own. We explain why something happens
here *rather than* there: the **fact** and the **foil**.

- Fact: monocytes of IFN-treated donors express topic 7.
- Foil: monocytes of control donors do not.

A hypothesis names a **difference** between the two sides that could cause
the contrast (Lipton's difference condition).

**Rule.** Help the person put each claim as "<fact> rather than <foil>,
because <difference>". For example: "Topic 7 is higher in treated than in
control monocytes because of the interferon response." Ask; do not rephrase
their claim for them.

**Rule.** Choose the foil as close to the fact as you can: the same cell
type, the same batch, ideally one difference. A foil that differs in many
ways (another tissue, another lab) explains little.

A hypothesis's "because" is a claim to test, not a finding. The verb rule
(8.3) is for what is written as found: observations, the triangulation table
and the summary.

### 8.2 Ranking hypotheses: likelier and lovelier

**Rule.** When the person weighs their hypothesis against its rivals, ask
them two things:

- **What does it explain?** Which observations it accounts for.
- **What does it leave open?** What it does not explain.

A hypothesis backed by more evidence is *likelier*. One that explains more
and leaves less open is *lovelier*. They are not the same. Loveliness decides
which to test first; only the test decides which holds. Lovely stories are
easy to believe, and in single-cell data the dull rival (batch, one donor,
quality) is often the likeliest: make sure the person has weighed it.

### 8.3 Sources of evidence

Triangulation compares **sources**: data that differ in where they came
from. Each source has its own main bias. Two sources are more independent
the less they share: cells, donors, lab and platform, study design. None is
fully independent of ours, but held-out donors share no cells, and another
study shares neither cells nor lab. A tool is how a source is read; it is
not a source of its own.

| Source | Shares with our cells | Main bias | Read with |
|---|---|---|---|
| **Our cells**: the data the hypothesis came from | (everything) | post-selection; these donors; this batch | senna, lupin, cocoa, pinto, mung |
| **The same cells, another measurement**: chromatin (ATAC), alleles in the same reads, protein | cells, donors, handling | the same sampling | chickpea (ATAC); faba `snp` read by mung `donors --baf` (alleles) |
| **The same donors, another sample**: a spatial section, another tissue, bulk | donors | the section cut; the tissue | pinto; data-beans `import` |
| **Other donors, same study**: held-out donors (section 12) | lab, platform, protocol | same lab, same platform | the same steps, every choice frozen |
| **Another study**: published data (section 9) | tissue, species | platform, cohort, how its cells were labelled | data-beans `import`, then the same tools; lupin to put its labels and programmes in common terms (8.4) |
| **Genetics**: the donors' genotypes; population studies (eQTL, GWAS) | donors, or nothing | linkage; one variant acting on several genes; few donors give little power | faba `snp`; fine-mapping in chickpea (planned) |
| **Perturbation**: published screens, knockouts, drug treatments | nothing | off-target effects; culture | lupin, with the perturbed genes as a gene set (8.4); section 9 |
| **Curated knowledge**: GO, the Cell Ontology, pathway databases, papers | nothing directly, but built from earlier studies | well-studied genes; circularity (8.4) | lupin |

The first five rows share less and less with our cells. Genetics and
perturbation also differ in design: alleles are assigned at conception and
perturbations by the experimenter, so neither follows the confounders of an
observational study. Curated knowledge is built from earlier studies, so it
counts only as far as those studies differ from ours (8.4).

Each tool reads a different contrast of a source, with a bias of its own.
Most read our cells; chickpea and allele balance read the same cells' other
measurements:

| Analysis | Tool | Contrast | Its own bias |
|---|---|---|---|
| Cell state | senna, lupin | cluster vs rest; type vs type | post-selection: found and tested on the same cells |
| Exposure effect | cocoa | treated vs counterfactual control, across individuals | unmeasured confounding |
| Place | pinto | region vs neighbouring region | the section cut; neighbourhood |
| Order | lupin trajectories | before vs after | pseudotime is inferred, not observed |
| Copy number | mung `donors`, `clones` (command line, no MCP yet: the person runs it, or the agent if it has a shell) | a donor's region vs that donor's neutral regions; a donor-private clone vs mixable cells | a block of co-expressed genes can look like a copy-number change |
| Regulation | chickpea, on the same cells' ATAC | linked vs unlinked peaks; group vs group | correlation; peaks in one module share it |
| Allele balance | faba `snp` (its `{batch}_baf` counts), read by mung `donors --baf` | imbalanced vs balanced heterozygous sites, within a donor | needs heterozygous sites with enough reads; per donor, not per cell. Each call comes back "supported" (agrees), "unsupported" (disagrees), "untested" (too few sites) or "neutral" (no gain or loss called) |

**Rule.** Agreement counts once per source. The analyses of one source make
a claim sharper, and one can contradict another, but when they agree they
are still one source: senna markers, lupin terms and cocoa effects on the
same cells are one. The same cells' other measurements (chromatin, alleles)
count as a source of their own, but name what they share: the cells and the
donors. They do not replicate a claim; only a source with other cells does.

**Rule.** A disagreement is information. Ask the person which source's bias
could explain it, and name that bias as a rival for them to weigh. Two sources can also disagree
because they see different things: allele balance sees a copy-neutral loss
of heterozygosity that expression cannot.

**Rule.** Use the verb the agreeing sources allow:

| What agrees | Verb |
|---|---|
| Our cells only, clusters and markers | "marks", "is characteristic of" |
| Our cells, or other measurements and samples of the same donors: a contrast across donors or conditions (cocoa, chickpea, pinto, trajectories) | "is associated with, in these donors" |
| Two or more sources that share no cells (held-out donors, another study) | "is associated with", replicated |
| As above, and genetics, a perturbation, or (for its own effect) a treatment the experimenter applied | "affects", "raises", "lowers", "drives" |

Never write "causes" or "drives" from one source. When a source disagrees,
keep the verb the agreeing sources allow, and always name the one that
disagrees: "is associated with (our cells, donors 5-6), but not in the
published data".

**Rule.** A treatment the experimenter applied (cells stimulated in the dish,
animals assigned at random) is a perturbation for the treatment's own
effect: once replicated, "IFN raises GZMB in NK cells" is allowed. In our
own data the treatment is part of the design, not a source of its own. It is not
a perturbation for anything downstream: "ISG peaks regulate GZMB" still
needs its own source. A treatment people chose or happened to have
(patients, exposures in a cohort) is an exposure, not a perturbation. If you
do not know how the treatment was given, ask the person; until they say,
treat it as an exposure.

### 8.4 Making sources comparable

Independent sources rarely share cells. Often they do not share features
either: peaks, regions and proteins are not genes. Each lab also names cell
types its own way. To ask the same contrast of each source, write it in
terms every source has. lupin supplies them.

- **Cell types as Cell Ontology terms.** lupin places each label in the Cell
  Ontology, and `lupin data suggest-cl` (command line) gives the nearest
  terms for any label, another study's too. Write the contrast with the
  terms ("CD14-positive monocyte rather than CD16-positive monocyte"), and
  map the other study's labels the same way before comparing.
- **Programmes as gene sets.** A single gene often does not carry across
  platforms, modalities (peaks linked to genes) or species; a gene set does.
  When sources differ gene by gene, compare them term by term. Read a term
  by its effect, not its q: lupin warns that q favours large programmes many
  clusters share.
- **Another source as a gene set.** lupin's term test takes a `gmt` file. A
  signature from another source, written as a GMT, tests our clusters
  against that source: the genes a knockout changes (perturbation), the
  target genes of eQTLs (genetics), another lab's markers (another study).
  Name where the gene set came from in the plan before the test.
- **Frozen panels.** The round's marker panel, applied unchanged to another
  source (`annotate_run` on its run), tests our labels there. A fresh
  annotation of the other source is a new finding, not a test.
- **Curated knowledge is a source too.** GO terms and marker panels come from
  earlier studies. When a panel's markers come from the papers that defined
  the type, agreement with them is circular and counts for nothing: the
  labels were chosen with that panel. It counts only when the panel or
  terms came from studies other than the ones the labels rest on. Say where
  the panel came from.
- **Whose labels.** A contrast is only as good as its labels. Say whether
  the person accepted them or the agent chose them alone (lupin's
  `decided_by`).

A cluster lupin cannot place, or places only at a coarse class, or where its
methods disagree, is where the known types do not fit. Put it on the
"Unexplained" observation (8.5).

### 8.5 The triangulation table

Do not write the table yourself. For each source, record what it says of a
hypothesis as kind `evidence`, with the analyses and numbers in `text`:

| Field | What to write |
|---|---|
| `hypothesis` | H1, H2, ... (section 6) |
| `source` | a name for the data, the same each time: "our cells, donors 1-4", "held-out donors 5-6", a study's accession |
| `class` | the kind of source (8.3): `our_cells`, `same_cells`, `same_donors`, `other_donors`, `other_study`, `genetics`, `perturbation`, `curated` |
| `verdict` | `agrees`, `disagrees`, or `mixed` (its analyses disagree) |
| `text` | what it shows, with numbers and thresholds |

```json
{"tool": "notebook", "args": {"kind": "evidence", "hypothesis": "H1", "source": "our cells, donors 1-4", "class": "our_cells", "verdict": "agrees", "text": "ISG15 and MX1 top markers of topic 7; cocoa: 212 genes at FDR < 0.1, ISGs top"}}

{"tool": "notebook", "args": {"kind": "evidence", "hypothesis": "H1", "source": "held-out donors 5-6", "class": "other_donors", "verdict": "agrees", "text": "topic 7 higher in both treated donors than in both controls, panel frozen"}}
```

The notebook keeps each row in `results/EVIDENCE.tsv` and returns the
hypothesis's table, one row per source (a source's latest word counts, and
counts once):

```text
**H1. Topic 7 is higher in treated than in control monocytes because of the interferon response**

| Source | Kind | Shares with our cells (by kind) | Says | Evidence |
|---|---|---|---|---|
| our cells, donors 1-4 | our cells | everything | agrees | ISG15 and MX1 top markers ... |
| held-out donors 5-6 | other donors, same study | lab, platform | agrees | topic 7 higher in both ... |

Verb: "is associated with". Standing: replicated: sources that share no cells agree.
```

The verb is worked out by the rules of 8.3; use it. "Standing" says how far
the claim got (one group of cells; replicated; replicated with genetics or a
perturbation). The "shares" column comes from the kind you gave. Anything that does not
agree is named after it, to be named with the verb. The result also lists
the kinds of source not yet used, those that would raise the verb first:
name them to the person (8.6). A source given a second time with another
class is refused: one source keeps one class.

Then, if a source disagrees or something is unexplained, show the person and
ask what they make of it. Write anything no hypothesis explains as an
observation starting "Unexplained:". Over a project these add up; point them
out, since several pointing the same way may be worth a hypothesis of the
person's own, including "something not yet named".

### 8.6 Choosing the next source

After each piece of evidence, name what could come next. This is the step
that turns a description into a causal claim, and the person chooses it.

**Rule.** Name sources that share little with the sources that already
agree. Another analysis of the same data adds nothing. For each, say what it
shares with the agreeing sources, what it costs, and whether it could
nullify the hypothesis, not only corroborate it.

**Rule.** Aim for the next verb up:

| The claim now allows | Add | Then it allows |
|---|---|---|
| "marks" (our cells, clusters) | a contrast across donors or conditions in our cells (cocoa), the same cells' ATAC (chickpea), a spatial section (pinto) | "is associated with, in these donors" |
| "is associated with, in these donors" | held-out donors (section 12); another study (section 9) | "is associated with", replicated |
| "is associated with", replicated | genetics (faba `snp`; copy number backed by allele balance, mung `donors --baf`; fine-mapping is planned); a perturbation (published data, or its genes as a gene set for lupin, 8.4) | "affects", "drives" |

**Rule.** Put the sources as numbered choices, in plain words, and always
include stopping:

> H1 now allows "is associated with, in these donors": in our cells (donors
> 1-4), cell state and cocoa agree. That is one source. What could
> corroborate it, or nullify it:
>
> 1. Held-out donors 5 and 6: other donors, same lab (minutes; the split is
>    done). You said no rise in ISG15 or MX1 there would make you drop it.
> 2. The same cells' ATAC: chickpea peak-to-gene links for the ISG genes
>    (shares the cells; about an hour).
> 3. Another study: a published IFN data set (section 9).
> 4. Perturbation: a published IFN-receptor knockout's genes, as a gene set
>    for lupin (8.4).
> 5. Stop here for H1: it stays "associated, one source".

The person chooses; what they choose shows in the next step and its
evidence, so the list itself is not recorded. Stopping is a legitimate
choice: if it is unclear whether they mean the whole day, ask. The person
decides how far to go; the table records how far the claim got.

**Rule.** A source that disagrees is as valuable as one that agrees. Add it
to the table. Never leave it out to keep the claim tidy.

### 8.7 Why the steps are in this order

| Step | From | In this paper |
|---|---|---|
| Generate hypotheses and their rivals | Peirce: abduction | the person's hypotheses H1, H2, ..., with rivals (section 5) |
| Rank them by contrast | Lipton: inference to the best explanation | fact and foil; likelier and lovelier (8.1, 8.2) |
| Choose the next source by what it shares | triangulation | the next source (8.6) |
| Test with predictions fixed first | Popper: falsification | "would nullify it" (`nullify`); the `plan` before outside data (section 9) |
| Combine sources with different biases | Lawlor, Tilling and Davey Smith; Munafò and Davey Smith: triangulation | sources, common terms and the table (8.3 to 8.5) |
| Refine without merely fitting | Lakatos: progressive and degenerating programmes | a refined hypothesis needs a new test (section 9) |
| Keep what does not fit | Kuhn: anomalies | the "Unexplained" observations (8.5) |

## 9. Testing on outside data

A hypothesis that holds only in the data it came from is weak. Data sets
that others have published can confirm it, sharpen it, or show that it is
wrong. Using them costs time, and some need permission to access, so it is
the person's choice. The agent asks.

**When to ask.** As one of the next sources (section 8.6), for a hypothesis
the person wants to take further. When exploring (section 13), this is after
stage 4, the shortlist. If the person passes it over or says "not now", leave
outside data out of later lists for that hypothesis unless they bring it up.

**How to ask.** Put the purposes as numbers (or a multiple-choice question):

> Shall I look for published data sets to test H1, "Topic 7 is higher in
> treated than in control monocytes because of the interferon response"?
> Pick any:
>
> 1. To confirm it: a similar data set where it should hold.
> 2. To refine it: data that can show where it holds (which cell type,
>    dose, time point, tissue).
> 3. To try to disprove it: a setting where it should fail if it is wrong.
> 4. Not now.

"ok" is not a choice: ask which number.

**Finding candidates.** Only after the person chose outside data and its
purpose (confirm, refine, disprove).

- Search with the literature or data tools you have (PubMed, bioRxiv, GEO,
  CELLxGENE and the like; load them first if your client lists them as not
  yet loaded). If you have none, ask the person for candidates.
- **Never invent an accession, a title or a sample count.** Give only what a
  search result showed you. Write "not checked" for anything you did not see.
- Prefer a data set where the hypothesis could fail. A data set picked
  because it will surely agree tests nothing.

Propose two to four candidates, numbered D1, D2, ..., each with:

| Field | What to write |
|---|---|
| Accession and link | As the search result gave it, e.g. a GEO series. |
| What it holds | Species, tissue, assay, number of donors or samples, conditions. |
| Purpose | Confirm, refine or disprove, and which hypothesis. |
| Prediction | What result would support the hypothesis, and what would refute it. Ask the person; use their words. |
| Differences | Platform, protocol, processing: reasons a mismatch might not mean the hypothesis is wrong. |
| Access | Open or controlled; size. |

Write the person's predictions in the notebook as kind `plan` (their words),
with the candidates as facts in `text`, **before anything is downloaded**.
This fixes the predictions before the data are seen. Then ask which to use
("D1", "D2", or "None of these").

```json
{"tool": "notebook", "args": {"kind": "plan", "words": "D1 should show topic 7 higher in treated monocytes in most donors; if not, H1 is in trouble. D2 (LPS) should stay low; if LPS raises it as much, it's general activation, not interferon.", "text": "Outside data for H1.\nD1 <accession, as found>: PBMCs, IFN-beta vs control, 8 donors, 10x 3'. Purpose: confirm. Differences: other chemistry, IFN-beta not IFN-alpha. Access: open, 4 GB.\nD2 <accession, as found>: same cells, untreated, stimulated with LPS. Purpose: disprove. Access: open, 3 GB."}}
```

**Bringing it in.**

1. The person downloads the data, or the agent does when the person says so.
   Controlled-access data (dbGaP, EGA and the like) is the person's to
   request.
2. Put the files in `data/` and import them with data-beans `import`, with
   `source` naming the accession and the link. That writes `data/README.md`.
   A gene set from another study (a perturbation's genes, eQTL targets) goes
   to lupin's term test as a `gmt` file instead (8.4); name its source in the
   plan.
3. Run the same steps with every choice frozen: the same settings, marker
   panel and thresholds. Change only what the data force (gene names, a file
   format), and note each change in an observation. Do not adjust settings
   until the outside data agree: that is fooling yourself (section 12).
4. Write an observation: held, held in part, or did not hold, with numbers,
   against the prediction in the plan.
5. Show the person, and ask what it means and why. When a difference between
   the data sets could explain a mismatch, name it as a rival for them to
   weigh. Record their judgement as `evidence`.

A hypothesis that does not hold is a result. Record it as plainly as one that
does. If refining changes the hypothesis, the changed version is a new
hypothesis: it needs its own test, on data it was not shaped on.

## 10. Rules for agents

**Before working**

1. Call `status` and tell the person where the project stands (section 4).
2. Ask the person's question and record their words as the `plan`.

**Running tools**

3. Look before you change: read-only tools first.
4. Never overwrite. Every tool refuses an existing output. Choose a new name
   in `out`. Each tool's instructions say what leaving `out` empty does; it
   differs between tools (for data-beans `qc`, no `out` means a preview that
   writes nothing).
5. A long step runs in the background and returns a job id at once
   (`job-3`). Wait for it:
   `{"tool": "job_wait", "args": {"id": "job-3", "seconds": 120}}`.
   If it is still running, call `job_wait` again. Use its outputs only after
   its state is `done`.
6. Tell the person plainly what is running and about how long, not the
   job's id. When a job's status has an `attach` command, tell them they
   can watch the step on screen with it.
7. If the person stopped a call ("stopped by the user"), do not run it again
   unless they ask.
8. A "Note from the user" in a tool result is the person speaking to you.
   Take it into account.
9. When a tool refuses a call, read the message. It says what to fix. Change
   the call; never send the same call again unchanged.

**Reading results**

10. Reason from each result's summary and numbers. Open large files only
    when you need a detail.
11. Give every count with its threshold: "87 genes at FDR < 0.1", never just
    "87 genes".
12. Report what was dropped or failed, not only what worked.

**Interpreting: the person's**

13. Never say what a result means, and never write a hypothesis, a verdict,
    a take-home or a summary for the person (sections 5 to 7). Explain what
    numbers measure; ask what they mean; record the person's words.
14. A result found on the same cells that were used to choose it is not
    confirmed (section 12). Say so in the observation.
15. Help the person put each claim as fact rather than foil, because a
    difference, and ask for its rivals and what would nullify it. Use only
    the verb the agreeing sources allow (section 8). Several tools on the
    same data are one source.
16. For a hypothesis, name the sources that could corroborate or nullify it
    (section 8.6); the person chooses and judges each; record it as
    `evidence` with their reason.
17. Name published data sets as one of the next sources (section 9). Record
    the person's predictions in a `plan` before any data is downloaded.
    Never invent an accession.

**Closing**

18. Ask for the person's summary of the day and record it (`summary`).
    Build the slides with `report`. Tell the person: today's folder
    (`results/<day>/`), the slides, what failed or was dropped, and what
    `status` lists as open for them.

## 11. The tools and how they connect

Each tool has its own instructions, sent when it connects. This table is the
map.

| Tool | What it does | Main tools (MCP) |
|---|---|---|
| **faba** | Turns aligned reads (BAM) into per-cell matrices: gene counts, depth, RNA editing, m6A, polyadenylation, SNPs. | `count`, `run`, `qc`, `pileup`, `cluster`, `docs` |
| **data-beans** | Stores single-cell matrices. Imports, inspects, filters (QC), subsets, splits, merges. | `info`, `names`, `statistics`, `values`, `qc` (both with a `figure`), `import`, `subset`, `split`, `merge` |
| **senna** | Embeds cells (topics, layouts, clusters) and shows them on a map. | `run_fit`, `view_open`, `view_map`, `view_markers`, `view_cell`, `view_export` |
| **pinto** | Fits spatial models and shows them on the tissue. | `run_fit`, `view_open`, `view_map`, `view_markers`, `view_export` |
| **lupin** | Labels clusters with cell types (decisions with a rationale), places them in the Cell Ontology, tests GO or pathway terms (also gene sets from other sources, as `gmt`), and orders types into trajectories. | `annotate_run`, `annotate_review`, `annotate_genes`, `annotate_preview`, `annotate_relabel`, `trajectory_run`, `trajectory_read` |
| **chickpea** | Links ATAC peaks to genes in paired RNA + ATAC data. | `p2g_run`, `p2g_links` |
| **legume** | Keeps the project's notebook: the person's plan, data verdicts, hypotheses with their rivals, evidence and its table, kept figures, summaries. Serves this paper, the project's standing and the day's slides. Runs no steps. | `guide`, `status`, `report`, `notebook` |
| **cocoa** | Estimates the effect of an exposure (e.g. treatment) on gene expression across individuals, adjusted for confounders. | `cocoa_commands`, `cocoa_run`, `cocoa_read`, `docs` |
| **mung** | Infers copy number per cell and per donor, and finds donor-private clones. | none yet: command line (`infercnv`, `clones`, `donors`), section 14 |

Every tool's server also has `job_status`, `job_wait` and `job_cancel`; the
notebook is the `legume` server's alone. Start a session with its `status`:
where the data, the outputs, today's notebook, the hypotheses and the kept
figures stand, and what is open for the person.

The usual path:

```text
BAM files ── faba ──▶ matrices ── data-beans (import, qc) ──▶ clean matrix
                                                                  │
                     ┌────────────────────────────────────────────┤
                     ▼                                            ▼
              senna (embed, cluster)                     pinto (spatial)
                     │                                            │
                     └──────────────▶ lupin (cell types, trajectories)
                                          │
                     ┌────────────────────┼─────────────────────┐
                     ▼                    ▼                     ▼
        chickpea (peak→gene links)   cocoa (treatment     senna / pinto views
        per cell type                effects per type)    of the labelled cells
```

lupin's labelling is its own decision loop: `annotate_review` shows the
evidence, `annotate_preview` shows what would change, and `annotate_relabel`
applies the decisions. Show the person the evidence and ask which label they
give; each decision has `decided_by`, and here it is `user` (or
`user_override` when they chose against lupin's top call), with their
rationale in their words.

## 12. Not fooling ourselves

Exploratory single-cell analysis goes wrong in a few known ways. The agent
checks for each one and says so in the notebook.

| Trap | What happens | What to do |
|---|---|---|
| **Double dipping** | Clusters are found on the data, then tested on the same data. The test looks stronger than it is. | Report effect sizes and ranks. Call p-values after clustering descriptive. Confirm on held-out data. |
| **Pseudo-replication** | Cells are treated as independent samples. Thousands of cells from three donors are still three donors. | Compare donors or samples (pseudobulk), not cells. |
| **Donor genome** | Donors differ in copy number (whole chromosome arms gained or lost). This can explain more variance than cell type, so clusters follow donors and hide the types. | Before senna, on the QC'd data, run mung `donors` if it is installed (section 14; if not, skip to the next sentence): `genome_share` (in its `.donors.json`) is the fraction of variance that lies between donors, and its per-donor calls say which regions are gained or lost. Report both in an observation. There is no fixed cutoff for `genome_share`: it depends on the baseline, the bin size and the cells per donor. Before naming types, check whether clusters follow donors, and make "donor copy number" one of the readings. mung `clones` finds donor-private CNV clones; senna and pinto take its table as `--cnv-clones`, so integration keeps them apart. |
| **Batch effects** | A pattern follows the sequencing run, not the biology. | Check every finding against batch. If they line up, say so. |
| **Post-selection** | Labels or marker sets were chosen on these cells. Rescoring on the same cells is not a test. | lupin marks such results `post_selection`. Confirm on held-out cells. |

Example observation:

```json
{"tool": "notebook", "args": {"kind": "observation", "text": "412 genes at FDR < 0.05 between clusters 3 and 5. The clusters were found on these same cells, so the p-values are descriptive. Not confirmed: the held-out donors are not tested yet."}}
```

**How to confirm.** Split off whole donors or batches before you start
(data-beans `split`). Do all the exploring on the rest. At the end, run the
same steps on the held-out part with every choice frozen (the same marker
panel, the same settings), and compare. If there is no held-out data, write
that the result is **not confirmed**. Published data from other labs is a
stronger test still (section 9).

## 13. Exploring without a hypothesis

Sometimes the person has data but no specific question. The agent can still
work in a disciplined way. The question becomes: "What varies in this data,
and which differences are worth testing?"

Work in stages. Stop after each stage, show the person what you found, and
let them steer.

| Stage | Goal | Typical tools | Ends with |
|---|---|---|---|
| 1. Survey | What is in the data: cells, genes, samples, conditions, quality, and how much variance lies between donors' genomes. | data-beans `info`, and `statistics` and `qc` with their `figure`; mung `donors` | An observation with the key numbers. |
| 2. Structure | Main axes of variation, clusters, cell types. | senna `run_fit`, `view_map`; lupin `annotate_*` | The person's labels and what they make of the clusters, in their words. |
| 3. Contrast | Where conditions differ: composition, programs, effects. | cocoa; lupin; chickpea | What the person makes of the differences, in their words. |
| 4. Shortlist | Three to five of the person's hypotheses, each as a contrast, with rivals and what would nullify it. | notebook `hypothesis` | Hypotheses H1, H2, ... in the person's words. |
| 5. Triangulate | Add sources that share little with those that agree, one at a time, as the person chooses (section 8.6): held-out donors (section 12), the same cells' other measurements, published data sets (section 9), genetic and perturbation data. | data-beans `split`, `import`; chickpea; lupin (`gmt`, frozen panels) | A triangulation table per hypothesis. |
| 6. Figures and report | The figures and tables the person keeps for the paper; each hypothesis with its table and the verb it allows. | notebook `figure`, `summary`; `legume` `report` | The day's slides (PDF and Markdown), ready to share. |

The result of exploring is **hypotheses**, not findings. Say so.

## 14. What is available now

| Feature | Status |
|---|---|
| Records: `runall.sh`, `notebook.md`, `INDEX.tsv`, `data/README.md` | Available |
| Background jobs (`job_status`, `job_wait`, `job_cancel`), sessions with `attach` | Available |
| `legume init`: a project connected to the agents (Claude Code, Codex, Cursor, VS Code, Gemini CLI, Antigravity, opencode), their instructions in `AGENTS.md`, and the project marked (`.legume/`) | New in legume-mcp 0.5.0 |
| The `legume` server (`legume mcp`): `guide`, `status` (with a hypothesis's table, the data on disk and what is open), `report`, `notebook` | New in legume-mcp 0.5.0 |
| One notebook, the `legume` server's: `plan`, `data`, `observation`, `hypothesis`, `evidence`, `figure`, `summary`, `note`; the person's kinds take their words only | New in legume-mcp 0.5.0 |
| Hypotheses H1, H2, ... with rivals and what would nullify each (`HYPOTHESES.tsv`); evidence with the person's reason (`EVIDENCE.tsv`); the table, its verb and the sources not yet used (section 8.5) | New in legume-mcp 0.5.0 |
| Data verdicts (`DATA.tsv`) and figures kept for the paper (`FIGURES.tsv`) | New in legume-mcp 0.5.0 |
| Key numbers (`facts`, flagged ones in `status`) and `inputs` for every step, in `INDEX.tsv` | New in legume-mcp 0.5.0; each tool adds its own numbers |
| The day's slides: `report.pdf` (typeset by legume when built with `--features pdf`, nothing else to install) and `report.md`, figures in place, with the trail graph | New in legume-mcp 0.5.0 |
| Recipes as MCP prompts on the `legume` server: `explore`, `triangulate` | New in legume-mcp 0.5.0 |
| `next` hints in results ("Next, if the person agrees"); tool annotations (read-only, writes, destructive) | New in legume-mcp 0.5.0; the job tools give them, each tool adds its own |
| Testing on outside data (section 9): `plan` and data-beans `import` | Available with 0.5.0 |
| lupin `decided_by` on labelling decisions | Available |
| lupin: Cell Ontology placement, `suggest-cl` (command line), term tests on GO, GAF or GMT gene sets (section 8.4) | Available |
| `legume paper`: a manuscript draft from the kept figures and the person's text | Planned |
| Fine-mapping (SuSiE-like) in chickpea, for genetics as a source | Planned |
| mung `infercnv` and `clones` (command line), read by senna and pinto `--cnv-clones` | Available |
| mung `donors`: per-donor CNV calls, the share of variance between donors, allele support from faba `snp` | In development (not released) |
| mung per-cell allele evidence; MCP server | Planned |
| `genome_share` against a null with donor labels shuffled, and against the same share for cluster labels (donor vs type) | Planned |

**Tools older than 0.5.0** still have a notebook of their own, which takes
`plan`, `observation`, `note` and the old `conclusion`. Use the `legume`
server's notebook instead whenever it is connected (`legume init` connects
it); if it is not, ask the person to run `legume init`.

## 15. A short worked example

```text
Person:  Explore the IFN-treated PBMCs. I have no specific hypothesis.

Agent:   [status]  The project holds data/pbmc.zarr.zip (6 donors); nobody
         has recorded where it came from or whether it is good.
         Where did it come from?

Person:  The core's 10x run, donors 1-6, three treated with IFN.

Agent:   [notebook data, words "The core's 10x run, ..."]
         [data-beans qc, no out: a preview]  Median 1,904 genes per cell. A
         15% mitochondrial cutoff would drop 12% of cells; donor 4 would lose
         30%. High mitochondrial reads often mean damaged cells. What do you
         make of donor 4?

Person:  Donor 4 was a bad prep. Keep it but use 20%.

Agent:   [notebook data, verdict "caveats", words "Donor 4 was a bad prep ..."]
         [data-beans qc, 20% cutoff, out results/.../qc20.zarr.zip]
         [notebook observation]  Kept 10,412 of 11,204 cells; donor 4 lost 14%.
         Before I cluster: what do you expect to separate the treated cells?

Person:  Monocytes, an interferon program.

Agent:   [senna fit; view_map]  Topic 7 is high in treated monocytes:
         ISG15 and MX1 lead it. What do you think it is?

Person:  The interferon response, I'm fairly sure.

Agent:   [notebook hypothesis, words "Topic 7 is the interferon response in
          treated monocytes"]  Recorded as H1. What else could explain it?
         And what result would make you drop it?

Person:  Could be two donors carrying it. I'd drop it if donors 5 and 6
         don't show it.

Agent:   [notebook hypothesis H1, rivals ["two donors carry it"],
          nullify "donors 5 and 6 don't show it"]
         What could corroborate or nullify it:
           1 held-out donors 5-6 (other donors, same lab; you named this)
           2 the same cells' ATAC (shares the cells)
           3 a published IFN data set      4 stop here

Person:  1

Agent:   [the same steps on donors 5 and 6, every setting frozen]
         Topic 7 is higher in both treated donors, about 3x. Does that agree
         with H1, and why?

Person:  Agrees: both held-out treated donors show it.

Agent:   [notebook evidence, H1, held-out donors 5-6, other_donors, agrees,
          words "both held-out treated donors show it"]
           Verb: "is associated with". Standing: replicated.
```

Every bracketed action is a tool call. Every interpreting word in the
notebook is the person's.

## 16. Glossary

- **Agent**: the AI model that calls the tools.
- **Contrast**: a fact and its foil: why this rather than that (section 8).
- **Evidence** (notebook kind): what one source says of one hypothesis, and
  why the person judged it so; the notebook builds the table from it
  (section 8.5).
- **Fact / foil**: the case to explain, and the closest case where it does
  not happen.
- **Figure** (notebook kind): an output the person keeps for the paper, with
  its number and their take-home.
- **Hypothesis**: the person's claim, as a contrast, with its rivals and what
  would nullify it; numbered H1, H2, ... for the whole project.
- **Job**: a long step running in the background, with an id like `job-3`.
- **Notebook**: `results/<day>/notebook.md`, the day's account.
- **Observation**: what was done and seen, with numbers. Facts only.
- **Outside data set**: data published by others, used to confirm, refine or
  try to disprove a working hypothesis (section 9).
- **Post-selection**: a result computed on the same cells used to choose the
  labels or settings. Not a confirmation.
- **Prediction**: what result would support a hypothesis and what would
  refute it, in the person's words, before the data are seen.
- **Project folder**: where all records are written.
- **Pseudobulk**: summing cells per donor or sample, so that donors, not
  cells, are compared.
- **Rivals**: what else could explain a hypothesis, as the person named it.
- **Session**: a tool's own screen running in the background; the person
  opens it with the `attach` command a job's status gives.
- **Source** (of evidence): data that differ in where they came from (other
  cells, donors, a study, a kind of study), with a main bias of their own.
  Several tools on the same data are one source (section 8.3).
- **Triangulation** (causal): combining independent sources of evidence
  whose biases differ; agreement between them is stronger than any one
  alone. The aim of the work (section 1).
- **Summary** (notebook kind): the person's account of the day: what changed,
  what argues against it, what is next.

## References

1. W. S. Noble, "A quick guide to organizing computational biology projects",
   PLoS Comput Biol 5(7): e1000424 (2009). doi:10.1371/journal.pcbi.1000424
2. W. S. Noble, "Ten simple rules for defining a computational biology
   project", PLoS Comput Biol 19(1): e1010786 (2023).
   doi:10.1371/journal.pcbi.1010786
3. P. Lipton, *Inference to the Best Explanation*, 2nd ed. Routledge (2004).
4. D. A. Lawlor, K. Tilling and G. Davey Smith, "Triangulation in aetiological
   epidemiology", Int J Epidemiol 45(6): 1866-1886 (2016).
5. M. R. Munafò and G. Davey Smith, "Robust research needs many lines of
   evidence", Nature 553: 399-401 (2018).
6. C. S. Peirce, *Collected Papers*, vols. 1-8, Harvard University Press
   (1931-1958).
7. K. Popper, *The Logic of Scientific Discovery*. Hutchinson (1959).
8. I. Lakatos, "Falsification and the methodology of scientific research
   programmes", in *Criticism and the Growth of Knowledge*, Cambridge
   University Press (1970).
9. T. S. Kuhn, *The Structure of Scientific Revolutions*. University of
   Chicago Press (1962).
