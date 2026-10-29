# Working together: people, agents and the legume tools

A white paper on how a person and an AI agent analyse data together with the
legume tools (faba, data-beans, senna, pinto, lupin, chickpea, cocoa), and how
the work is kept so anyone can check it and run it again. The tools run the
steps; the `legume` server (`legume mcp`, from legume-mcp) keeps the reasoning
they share, and serves this paper a section at a time (its `guide` tool, or
`legume guide` on the command line).

It is written for two readers:

- **People** who run analyses with an agent and want to know what to expect.
- **Agents**, including small models. The quick card below and sections 4 to
  11 are the rules an agent follows. They are plain steps on purpose. If you
  are an agent, read them fully before you call a tool.

Some features are new in legume-mcp 0.5.0 or still planned. Section 14 lists
what is available now. For how to call a tool's own functions (its
arguments, what `out` does), follow that tool's instructions. For the
notebook, follow this paper: some tools' instructions still say to close with
kind `conclusion`, which is the old name (section 14).

**Two notebooks, one record.** Every tool's server has a `notebook` for the
`plan`, `observation` and `note`. The `legume` server's `notebook` takes
those too, and the reasoning: `interpretation`, `answer`, `evidence`,
`offer` and `choice`. All write the same `results/<day>/notebook.md`.

---

## Quick card for agents

```text
 1. Find the project folder in the tools' instructions ("under the project ...").
    If it looks wrong (a folder of source code, e.g. with Cargo.toml and src/;
    your home folder), tell the person. Read data/README.md if it is there.
 2. First call: notebook kind "plan": the question as a contrast (fact rather
    than foil), the data, and two or more sources of evidence that share as
    little as possible (section 8.3). Several tools on the same data are
    one source.
 3. Look before you change: read-only tools first.
 4. Never overwrite. If a tool refuses, read its message and change the call.
 5. A job id ("job-3") means: call job_wait {"id": "job-3"} until it is done.
    Use its outputs only after that.
 6. After a step: notebook kind "observation", with numbers and thresholds.
 7. To say what something means: on the legume server, notebook kind
    "interpretation" with 2+ readings (it also takes answer, evidence, offer
    and choice; a tool's own notebook refuses them),
    each claim as "<fact> rather than <foil>, because <difference>".
    Then ask the person which hold.
 8. Their reply: notebook kind "answer" with "chose" (numbers) and "words" (exact).
    "ok" or "continue" is not an answer: ask again. The readings chosen
    become hypotheses H1, H2, ... (the result says which).
 9. For each source of evidence on a hypothesis: notebook kind "evidence"
    with "hypothesis", "source", "class", "verdict" and the numbers in "text"
    (section 8.5). The result is its table, the verb it allows and the
    sources not yet used. Offer the next sources, ones that share little
    with the sources that agree (section 8.6): notebook kind "offer" with
    numbered "options", then ask. Their reply: kind "choice" with
    "chose" and "words", never "answer". Published data sets are one such
    source: search only after they choose it, and never invent an accession.
10. Use the verb the agreeing sources allow. "Drives" only with genetic,
    perturbation or applied-treatment evidence.
11. Never decide alone what a result means, even if asked to. A result that
    ends "Next, if the person agrees" lists calls to propose, not to run.
12. At the end: the `legume` server's `report`, with your story of the day
    (the rest of the slides is the records); then where the records and the
    slides are, and for each hypothesis its table and the source still
    missing.
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
facts from a source. The agent plans, runs the tools, proposes what the
results might mean and, after each answer, proposes the next source that
could confirm or break the claim. The person decides what the
results mean and how far to go. Everything is written down in the project
folder, so the work can be checked and run again without the agent. The
records follow two guides by W. S. Noble: how to organise a computational
biology project (2009) and how to define one (2023).

## 2. Who does what

| | Does | Never does |
|---|---|---|
| **Tool** | Computes and states facts: counts, fits, scores, tables, figures. Records every step it runs. Refuses bad input with a message that says what to fix. | Decides what a result means. Overwrites an earlier result. |
| **Agent** | Plans, runs tools, reads results, writes the notebook, proposes several possible readings, asks the person, records the reply, proposes the next source of evidence. | Decides alone what a result means. Rewrites the person's words. Hides a failed or doubtful step. |
| **Person** | Sets the question. Answers the agent's proposals. Decides what the results mean. Stops or redirects the work. | Has to remember what was done: the records do that. |

The short version: **tools state facts, the agent proposes, the person
decides.**

## 3. The project folder

All work happens in one project folder. Every tool writes its records there.

```text
<project>/
  data/                   inputs
  data/README.md          where each input came from, and when
  results/
    INDEX.tsv             one row per output: date, time, server, tool, file, summary,
                          the files it was made from, its key numbers
    HYPOTHESES.tsv        the readings the person chose, numbered H1, H2, ...
    EVIDENCE.tsv          what each source of evidence says of a hypothesis
    <YYYY-MM-DD>/         one folder per day
      runall.sh           every step of the day, in order; running it repeats the work
      notebook.md         the day's account: plan, observations, readings, answers
      chat.md             the day's chats with a local model (legume chat), and the model
      ...                 outputs the agent did not name
```

What each record is for:

- **`runall.sh`** repeats the work without the agent:
  `bash results/<day>/runall.sh`. Steps that failed stay in it as comments.
- **`notebook.md`** is the story of the day: what was asked, done and seen,
  and what it means. The tools' outputs hold the facts; the notebook holds
  the reasoning.
- **`INDEX.tsv`** says which file came from which tool, with a one-line
  summary, the files it was made from (`inputs`) and its key numbers
  (`facts`: cells kept of how many, hits at which threshold). A tool flags a
  number that looks wrong; `legume` `status` lists the flagged ones. Raise
  them with the person before building on that output. Following `inputs`
  back tells whether two results come from the same data, and so are one
  source (section 8.3).
- **`data/README.md`** says where each input came from.
- **`HYPOTHESES.tsv`** and **`EVIDENCE.tsv`** are the project's
  triangulation ledger: they outlive the day, so a hypothesis keeps its
  number and its table across days.

Two practical rules:

- The project folder is the server's working folder, or the folder named by
  the environment variable `LEGUME_MCP_PROJECT`. Each tool's instructions
  say which, in a sentence like "Records ... under the project
  /path/to/project". Set `LEGUME_MCP_PROJECT` when you register the tools,
  or the records land wherever the client was started.
- Records are only ever added to. Nothing in them is rewritten.

## 4. The working loop

Every piece of work follows the same loop, in this order.

1. **Check the folder.** Read the project folder from the tools'
   instructions (no call needed). If it looks wrong (a folder of source code,
   with files such as `Cargo.toml` and `src/`; your home folder), tell the
   person before anything else. Read `data/README.md` if it is there: the
   plan names the data.
2. **Plan.** Notebook kind `plan`: the question as a contrast (fact and
   foil, section 8.1), the data, the sources of evidence you expect to use
   (two or more that share as little as possible, section 8.3), and how the
   result will be checked.
3. **Look before you change.** Inspect the data with read-only tools (for
   example data-beans `info`, `names`, `statistics`).
4. **Run a step.** Call the tool. A long step runs in the background: see
   section 10, rule 5.
5. **Observe.** Notebook kind `observation`: what you did and what you saw,
   with numbers.
6. **Propose.** When a result needs interpreting: notebook kind
   `interpretation` with two or more readings (section 5). Then ask the
   person.
7. **Record the reply.** Notebook kind `answer` (section 6).
8. **Triangulate.** When the person has chosen a reading worth more work (a
   working hypothesis): record what each source says of it as kind
   `evidence` (section 8.5), which returns its table and the verb it allows
   now, and offer the next source of evidence, one that
   shares little with the sources that already agree (section 8.6).
   Published data sets are one such source (section 9). The person chooses.
9. **Next step.** Agree the next step with the person, and go back to 3.
10. **Close.** Tell the person where the records are and, for each working
    hypothesis, its table, the verb it allows, and the source that would
    strengthen it most.

Examples (a plan and an observation go to any server's notebook):

```json
{"tool": "notebook", "args": {"kind": "plan", "text": "Question: why do the cells of IFN-treated donors differ from those of control donors, cell type by cell type? (fact: treated; foil: control, same cell type)\nData: data/pbmc.zarr.zip, 6 donors (3 control, 3 IFN).\nSources: our cells, donors 1-4 (senna, lupin, cocoa); held-out donors 5 and 6; a published IFN data set if the person chooses it.\nCheck: a finding must hold in donors 5 and 6."}}

{"tool": "notebook", "args": {"kind": "observation", "text": "QC kept 9,812 of 11,204 cells (12% dropped). Donor 4 lost 30%, the others 8-11%."}}
```

## 5. Proposing readings

An agent never decides alone what a result means. It proposes **readings**:
possible explanations, each one testable.

Rules:

1. Give **two or more** readings. The tool refuses fewer.
2. Each reading has:
   - `claim` (required): one sentence, the possible explanation.
   - `rationale` (required): why it is plausible.
   - `check` (required): what would tell this reading apart from the others.
   - `evidence` (optional): the facts it rests on, as a list of
     `{"kind": ..., "term": ...}`. Use one of these kinds:
     `marker` (a gene), `term` (a GO or pathway term), `statistic` (a
     number, e.g. "median mito 22%"), `figure` (a file), `step` (an output
     of an earlier step), `note`.
   - `alternatives` (optional): what else it could be, and why not, as a list
     of `{"label": ..., "why_not": ...}`.
3. When a boring explanation is possible (batch, quality, one donor, chance),
   make it one of the readings.
4. The notebook numbers the readings R1, R2, ... in the order you give them.

Example:

```json
{"tool": "notebook", "args": {
  "kind": "interpretation",
  "readings": [
    {"claim": "Topic 7 is higher in treated than in control monocytes because of the interferon response",
     "evidence": [{"kind": "marker", "term": "ISG15"}, {"kind": "marker", "term": "MX1"}],
     "alternatives": [{"label": "batch effect", "why_not": "topic 7 is present in both sequencing runs"}],
     "rationale": "3x higher in treated donors, led by interferon-stimulated genes. Explains: the ISG markers and the monocyte bias. Leaves open: why donor 2 is highest",
     "check": "pseudobulk per donor: all 3 treated donors higher than all 3 controls"},
    {"claim": "Topic 7 is higher in treated than in control monocytes because two treated donors carry most of it",
     "evidence": [{"kind": "statistic", "term": "donors 2 and 3 hold 70% of topic 7 cells"}],
     "rationale": "most of its cells come from two donors. Explains: the uneven donor counts. Leaves open: why those two",
     "check": "leave-one-donor-out: the effect stays without donor 2 or 3"}
  ]}}
```

A call the tool refuses:

```json
{"tool": "notebook", "args": {"kind": "interpretation", "text": "Topic 7 is clearly interferon."}}
```

It is one claim, with no evidence, no alternative and no check.

After the readings, **ask the person** which hold. Make answering easy:

> Which of these hold? Reply with numbers (for example "1" or "1 and 3"),
> or a short line of your own.

Use a multiple-choice question if your client can show one.

## 6. Recording the person's reply

Record the reply with notebook kind `answer`. It has two fields:

- `chose`: the numbers of the readings the person chose. Leave it out when
  they chose none.
- `words`: the person's reply, **copied exactly**. Do not shorten, fix or
  summarise it.

The notebook does the rest: it links the answer to today's latest proposal,
checks the numbers, and writes who decided.

| The person replies | Call |
|---|---|
| "2" | `{"kind": "answer", "chose": [2], "words": "2"}` |
| "1 and 3, but check donor 4" | `{"kind": "answer", "chose": [1, 3], "words": "1 and 3, but check donor 4"}` |
| "1 I think, not sure" (numbers with a hedge) | `{"kind": "answer", "chose": [1], "words": "1 I think, not sure"}`. The numbers count; the hedge stays in `words`. |
| "None of these. Cluster 4 is doublets." | `{"kind": "answer", "words": "None of these. Cluster 4 is doublets."}` |
| "ok" / "continue" / "sounds good" / "whatever you think" / "yes" | **No call.** This does not say which readings hold. Ask again: "Which of R1 to R3 hold? A number is enough." |
| A reply to a list that is **not** readings: next sources (section 8.6), outside data (section 9) | Kind `choice`, never `answer`: `{"kind": "choice", "chose": [2], "words": "2"}`. The list itself was recorded as kind `offer` when you asked (section 8.6). `answer` takes only reading numbers, so "2" there would mean R2; the notebook refuses an `answer` when the latest question was an offer, and a `choice` when it was a proposal. A bare "yes" to a numbered list is not a choice either: ask which number. |
| A reply to an **earlier** question, e.g. "Actually R2 holds too" after you made an offer | `{"kind": "note", "by": "user", "text": "Actually R2 holds too"}`, then answer the open question first. If it changes which readings hold, propose them again (a new `interpretation`) so the person can answer it. |
| "Note: donor 4 was sequenced late." (not about a proposal) | `{"kind": "note", "by": "user", "text": "Note: donor 4 was sequenced late."}` |

The notebook then shows:

```text
## 14:32:10 · senna · interpretation (proposed)
**R1. Topic 7 is higher in treated than in control monocytes because of the interferon response**
...
## 14:35:02 · senna · interpretation (accepted by the user)
Answers the proposal of 14:32:10 (senna): chose R1. Hypotheses: R1 = H1.

> 1, but check donor 4
```

Anyone reading it later sees what was proposed, what the person decided, and
whose words are whose. Each reading chosen becomes a **hypothesis** of the
project, H1, H2, ..., in `results/HYPOTHESES.tsv`; the same claim chosen
again keeps its number. Refer to it by that number from then on, on any
day. An answer that chose readings is recorded as
"accepted by the user"; one that chose none, as "user override". lupin
records its labelling decisions the same way, under the names
`agent_proposed_user_accepted` and `user_override` (field `decided_by`).

If the tool refuses an answer because "there is no proposal today", record
the person's words as a `note` with `by` `user` instead.

The person is responsible for the interpretation. The tools make that easy
and visible; they cannot force it. A report built from a notebook with only
proposed readings shows "not yet interpreted".

## 7. When someone asks the agent to decide

Sometimes the person says "just tell me what it means" or "you decide".
The agent still does not decide alone. It answers like this:

> Here are the readings I think are most likely, with what would tell them
> apart: R1 ..., R2 ... I lean towards R1 because ... Which do you want to go
> with? Reply with a number.

Giving a preference is fine. Recording it as the person's decision is not.
Only the person's reply, recorded as an `answer`, decides.

## 8. Explaining by contrast, checking by triangulation

This section is the reasoning behind the loop. Agents: the rules in it are
marked **Rule**.

### 8.1 Every question is a contrast

A result is never explained on its own. We explain why something happens
here *rather than* there: the **fact** and the **foil**.

- Fact: monocytes of IFN-treated donors express topic 7.
- Foil: monocytes of control donors do not.

A reading names a **difference** between the two sides that could cause the
contrast (Lipton's difference condition).

**Rule.** Write each claim as "<fact> rather than <foil>, because
<difference>". For example: "Topic 7 is higher in treated than in control
monocytes because of the interferon response."

**Rule.** Choose the foil as close to the fact as you can: the same cell
type, the same batch, ideally one difference. A foil that differs in many
ways (another tissue, another lab) explains little.

A reading's "because" is a proposal to test, not a finding. The verb rule
(8.3) is for what you write as found: observations, the triangulation table
and the close.

### 8.2 Ranking readings: likelier and lovelier

**Rule.** In each reading's `rationale`, add two lines:

- **Explains:** which observations it accounts for.
- **Leaves open:** what it does not explain.

A reading backed by more evidence is *likelier*. A reading that explains more
and leaves less open is *lovelier*. They are not the same. Loveliness decides
which reading to test first; only the check decides which holds. Language
models are good at lovely stories, and in single-cell data the dull reading
(batch, one donor, quality) is often the likeliest. When you say which you
lean towards (section 7), say whether it is likelier or lovelier.

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

**Rule.** A disagreement is information. Ask which source's bias could
explain it, and make that one of the readings. Two sources can also disagree
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
offer from them (8.6). A source given a second time with another class is
refused: one source keeps one class.

Then, if a source disagrees or something is unexplained, propose readings on
it and ask, as usual. If the agreeing sources keep the hypothesis, offer the
next sources (8.6). Write anything no reading explains as an observation
starting "Unexplained:". Over a project these add up. Several pointing the
same way are worth a reading of their own, including "something not yet
named".

### 8.6 Choosing the next source

After each answer that keeps a working hypothesis, offer what to use next.
This is the step that turns a description into a causal claim.

**Rule.** Offer sources that share little with the sources that already
agree. Another analysis of the same data adds nothing. The order of the
list is not a ranking: say which one you lean towards and why (what it
shares with the agreeing sources, what it costs).

**Rule.** Aim for the next verb up:

| The claim now allows | Add | Then it allows |
|---|---|---|
| "marks" (our cells, clusters) | a contrast across donors or conditions in our cells (cocoa), the same cells' ATAC (chickpea), a spatial section (pinto) | "is associated with, in these donors" |
| "is associated with, in these donors" | held-out donors (section 12); another study (section 9) | "is associated with", replicated |
| "is associated with", replicated | genetics (faba `snp`; copy number backed by allele balance, mung `donors --baf`; fine-mapping is planned); a perturbation (published data, or its genes as a gene set for lupin, 8.4) | "affects", "drives" |

**Rule.** Offer the sources as numbered choices, with what each would add,
what it shares with the agreeing sources and what it costs, and always
include stopping:

> R1 now allows "is associated with, in these donors": in our cells (donors
> 1-4), cell state and cocoa agree. That is one source. Next sources, pick
> any:
>
> 1. Held-out donors 5 and 6: other donors, same lab (minutes; the split is
>    done).
> 2. The same cells' ATAC: chickpea peak-to-gene links for the ISG genes
>    (shares the cells; about an hour).
> 3. Another study: a published IFN data set (section 9).
> 4. Perturbation: a published IFN-receptor knockout's genes, as a gene set
>    for lupin (8.4).
> 5. Stop here for R1: it stays "associated, one source".

Record the list as kind `offer` before you ask, and the reply as kind
`choice` (section 6), never as an `answer`:

```json
{"tool": "notebook", "args": {"kind": "offer", "text": "Next sources for R1 (now: is associated with, in these donors; one source: our cells, donors 1-4).", "options": ["Held-out donors 5 and 6", "The same cells' ATAC: chickpea links for the ISG genes", "Another study: a published IFN data set", "Perturbation: a published IFN-receptor knockout's genes as a gene set", "Stop here for R1"]}}

{"tool": "notebook", "args": {"kind": "choice", "chose": [1, 3], "words": "1, then 3"}}
```

Stopping is a legitimate choice. "Stop here" ends work on this hypothesis;
if it is unclear whether the person means the whole day, ask. The person
decides how far to go; the table records how far the claim got.

**Rule.** A source that disagrees is as valuable as one that agrees. Add it
to the table. Never leave it out to keep the claim tidy.

### 8.7 Why the steps are in this order

| Step | From | In this paper |
|---|---|---|
| Generate readings | Peirce: abduction | readings R1, R2, ... (section 5) |
| Rank them by contrast | Lipton: inference to the best explanation | fact and foil; likelier and lovelier (8.1, 8.2) |
| Choose the next source by what it shares | triangulation | the next source (8.6) |
| Test with predictions fixed first | Popper: falsification | `check`; the `plan` before outside data (section 9) |
| Combine sources with different biases | Lawlor, Tilling and Davey Smith; Munafò and Davey Smith: triangulation | sources, common terms and the table (8.3 to 8.5) |
| Refine without merely fitting | Lakatos: progressive and degenerating programmes | a refined hypothesis needs a new test (section 9) |
| Keep what does not fit | Kuhn: anomalies | the "Unexplained" observations (8.5) |

## 9. Testing on outside data

A hypothesis that holds only in the data it came from is weak. Data sets
that others have published can confirm it, sharpen it, or show that it is
wrong. Using them costs time, and some need permission to access, so it is
the person's choice. The agent asks.

**When to ask.** As one of the next sources (section 8.6), once a reading has
become a **working hypothesis**: the person chose it in an `answer`, and it
is worth more work. When exploring (section 13), this is after stage 4, the
shortlist. If the person passes it over or says "not now", leave outside
data out of later lists for that hypothesis unless they bring it up.

**How to ask.** Offer the choices as numbers (or a multiple-choice question):

> R1 is now our working hypothesis: "Topic 7 is higher in treated than in
> control monocytes because of the interferon response". Shall I look for published data sets to test it? Pick any:
>
> 1. To confirm it: a similar data set where it should hold.
> 2. To refine it: data that can show where it holds (which cell type,
>    dose, time point, tissue).
> 3. To try to disprove it: a setting where it should fail if it is wrong.
> 4. Not now.

Record the question as an `offer` and the reply as a `choice`, exactly as in
section 8.6 ("ok" is not a choice: ask again):

```json
{"tool": "notebook", "args": {"kind": "offer", "text": "Outside data for R1.", "options": ["To confirm it", "To refine it", "To try to disprove it", "Not now"]}}

{"tool": "notebook", "args": {"kind": "choice", "chose": [1, 3], "words": "1 and 3"}}
```

**Finding candidates.** Only after the person chose outside data, as a
`choice` (a bare "yes" to a numbered list is not one: ask which number). If
they chose it from the next-sources offer (8.6), ask only the purpose
(confirm, refine, disprove) before searching.

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
| Purpose | Confirm, refine or disprove, and which reading. |
| Prediction | What result would support the reading, and what would refute it. |
| Differences | Platform, protocol, processing: reasons a mismatch might not mean the reading is wrong. |
| Access | Open or controlled; size. |

Write the candidates in the notebook as kind `plan` **before anything is
downloaded**. This fixes the predictions before the data are seen. Then ask
which to use: an `offer` with one option per candidate ("D1 ...", "D2 ...",
and "None of these"), and the reply as a `choice`, as above.

```json
{"tool": "notebook", "args": {"kind": "plan", "text": "Outside data for R1 (topic 7 = interferon response in monocytes).\nD1 <accession, as found>: PBMCs, IFN-beta vs control, 8 donors, 10x 3'. Purpose: confirm. Prediction: topic 7 genes higher in treated monocytes in at least 6 of 8 donors; refuted if no higher than in controls. Differences: other chemistry, IFN-beta not IFN-alpha. Access: open, 4 GB.\nD2 <accession, as found>: same cells, untreated, stimulated with LPS. Purpose: disprove. Prediction: topic 7 stays low; refuted (topic 7 is a general activation program, not interferon) if LPS raises it as much. Access: open, 3 GB."}}
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
5. Propose readings on what it means, as usual. When a difference between
   the data sets could explain a mismatch, make that one of the readings.
   The person answers.

A hypothesis that does not hold is a result. Record it as plainly as one that
does. If refining changes the hypothesis, the changed version is a new
hypothesis: it needs its own test, on data it was not shaped on.

## 10. Rules for agents

**Before working**

1. Check the project folder (section 4, step 1).
2. Write a `plan` first.

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
6. When a job's status has an `attach` command, tell the person that
   command: they can watch the step on screen.
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

**Interpreting**

13. Never decide alone what a result means (sections 5 to 7).
14. A result found on the same cells that were used to choose it is not
    confirmed (section 12). Say so in the observation.
15. Write each claim as fact rather than foil, because a difference; use the
    verb the agreeing sources allow (section 8). Several tools on the same
    data are one source.
16. Record what each source says of a hypothesis as kind `evidence`, and
    use the verb its result allows. Then offer the next sources, ones that share little
    with those that agree, with stopping as a choice: an `offer`, and the
    reply as a `choice` (section 8.6).
17. Offer published data sets as one of the next sources (section 9).
    Write the predictions in a `plan` before any data is downloaded. Never
    invent an accession.

**Closing**

18. Tell the person: today's folder (`results/<day>/`), `notebook.md`,
    `runall.sh`, what failed or was dropped, which readings are still
    waiting for their answer, `HYPOTHESES.tsv` and `EVIDENCE.tsv`, and for
    each hypothesis worked on its table, the verb it allows and the source
    still missing, as the last `evidence` result gave them. Do not close
    with kind `conclusion` (section 14).

## 11. The tools and how they connect

Each tool has its own instructions, sent when it connects. This table is the
map.

| Tool | What it does | Main tools (MCP) |
|---|---|---|
| **faba** | Turns aligned reads (BAM) into per-cell matrices: gene counts, depth, RNA editing, m6A, polyadenylation, SNPs. | `count`, `run`, `qc`, `pileup`, `cluster`, `docs` |
| **data-beans** | Stores single-cell matrices. Imports, inspects, filters (QC), subsets, splits, merges. | `info`, `names`, `statistics`, `values`, `qc`, `plot`, `import`, `subset`, `split`, `merge` |
| **senna** | Embeds cells (topics, layouts, clusters) and shows them on a map. | `run_fit`, `view_open`, `view_map`, `view_markers`, `view_cell`, `view_export` |
| **pinto** | Fits spatial models and shows them on the tissue. | `run_fit`, `view_open`, `view_map`, `view_markers`, `view_export` |
| **lupin** | Labels clusters with cell types (decisions with a rationale), places them in the Cell Ontology, tests GO or pathway terms (also gene sets from other sources, as `gmt`), and orders types into trajectories. | `annotate_run`, `annotate_review`, `annotate_genes`, `annotate_preview`, `annotate_relabel`, `trajectory_run`, `trajectory_read` |
| **chickpea** | Links ATAC peaks to genes in paired RNA + ATAC data. | `p2g_run`, `p2g_links` |
| **legume** | Keeps the reasoning the tools share: readings, the person's answers, hypotheses and their evidence, offers and choices. Serves this paper and the project's standing. Runs no steps. | `guide`, `status`, `hypotheses`, `notebook` (every kind) |
| **cocoa** | Estimates the effect of an exposure (e.g. treatment) on gene expression across individuals, adjusted for confounders. | `cocoa_commands`, `cocoa_run`, `cocoa_read`, `docs` |
| **mung** | Infers copy number per cell and per donor, and finds donor-private clones. | none yet: command line (`infercnv`, `clones`, `donors`), section 14 |

Every tool's server also has `notebook` (plan, observation, note),
`job_status`, `job_wait` and `job_cancel`. Start a session with the `legume`
server's `status`: where the data, the outputs, today's notebook and the
hypotheses stand, and any question still waiting for the person's reply.

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
evidence, the agent proposes labels with a rationale, `annotate_preview`
shows what would change, and `annotate_relabel` applies the decisions. Each
decision has `decided_by`: `agent` for the agent's own,
`agent_proposed_user_accepted` only when the person accepted it,
`user_override` when the person chose differently, `user` for the person's
own.

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
| 1. Survey | What is in the data: cells, genes, samples, conditions, quality, and how much variance lies between donors' genomes. | data-beans `info`, `statistics`, `qc`, `plot`; mung `donors` | An observation with the key numbers. |
| 2. Structure | Main axes of variation, clusters, cell types. | senna `run_fit`, `view_map`; lupin `annotate_*` | Readings on what the clusters are, and an answer. |
| 3. Contrast | Where conditions differ: composition, programs, effects. | cocoa; lupin; chickpea | Readings on the differences, and an answer. |
| 4. Shortlist | Three to five candidate hypotheses, each as a contrast, with evidence, a check and the sources that could test it. | notebook | Readings the person answers. |
| 5. Triangulate | Add sources that share little with those that agree, one at a time, as the person chooses (section 8.6): held-out donors (section 12), the same cells' other measurements, published data sets (section 9), genetic and perturbation data. | data-beans `split`, `import`; chickpea; lupin (`gmt`, frozen panels) | A triangulation table per hypothesis. |
| 6. Report | Each hypothesis with its table, the verb it allows, and the source still missing. | `legume` `report` (the story is yours; the rest is the records) | The day's slides, ready to share. |

The result of exploring is **hypotheses**, not findings. Say so.

## 14. What is available now

| Feature | Status |
|---|---|
| Records: `runall.sh`, `notebook.md`, `INDEX.tsv`, `data/README.md` | Available |
| Background jobs (`job_status`, `job_wait`, `job_cancel`), sessions with `attach` | Available |
| Notebook kinds `plan`, `observation`, `note` | Available |
| The `legume` server (`legume mcp`): `guide`, `status`, `hypotheses`, and the notebook's reasoning kinds | New in legume-mcp 0.5.0 |
| Notebook kinds `interpretation` (with `readings`) and `answer` | New in legume-mcp 0.5.0, on the `legume` server |
| lupin `decided_by` on labelling decisions | Available |
| Key numbers (`facts`, flagged ones in `legume` `status`) and `inputs` for every step, in `INDEX.tsv` | New in legume-mcp 0.5.0; each tool adds its own numbers |
| Recipes as MCP prompts on the `legume` server: `explore`, `triangulate` | New in legume-mcp 0.5.0 |
| `next` hints in results ("Next, if the person agrees"); tool annotations (read-only, writes, destructive) | New in legume-mcp 0.5.0; the job tools give them, each tool adds its own |
| Notebook kinds `offer` and `choice` (section 8.6) | New in legume-mcp 0.5.0 |
| Testing on outside data (section 9): `plan`, `offer`, `choice` and data-beans `import` | Available with 0.5.0 |
| Contrast and triangulation (section 8), written in `claim`, `rationale` and observations | Available |
| lupin: Cell Ontology placement, `suggest-cl` (command line), term tests on GO, GAF or GMT gene sets (section 8.4) | Available |
| Notebook kind `evidence`; hypotheses H1, H2, ... (`HYPOTHESES.tsv`, `EVIDENCE.tsv`); the table, its verb and the sources not yet used, built by the notebook (section 8.5) | New in legume-mcp 0.5.0 |
| `contrast` (fact and foil) as a notebook field | Planned |
| Fine-mapping (SuSiE-like) in chickpea, for genetics as a source | Planned |
| mung `infercnv` and `clones` (command line), read by senna and pinto `--cnv-clones` | Available |
| mung `donors`: per-donor CNV calls, the share of variance between donors, allele support from faba `snp` | In development (not released) |
| mung per-cell allele evidence; MCP server | Planned |
| `genome_share` against a null with donor labels shuffled, and against the same share for cluster labels (donor vs type) | Planned |
| The triangulation table, its verb and the sources not yet used, in the chat's screen and in `legume report` | New in legume-mcp 0.5.0 |
| `legume chat` (the tools with a local model: llama-server, LM Studio, Ollama, MLX, vLLM), with a small chat for small models | New in legume-mcp 0.5.0 (feature `chat`) |
| The chat's screen: the conversation beside the steps and their numbers, the notebook and the hypotheses with their sources, in a session to leave and come back to (`legume attach`) | New in legume-mcp 0.5.0 (feature `screen`) |
| `legume init` (a project's chat settings, `.legume/chat.json`) and the chat's record (`chat.md`, with the model file's SHA-256) | New in legume-mcp 0.5.0 (feature `chat`) |
| `legume report` (a day's slides built from the records, Typst; the story the agent's only) and `legume graph` (the provenance graph from `INDEX.tsv`) | New in legume-mcp 0.5.0; the `legume` server's `report` tool |

**Without the `legume` server.** If a tool's notebook says a kind "goes to
the `legume` server's notebook" and no `legume` server is connected, ask the
person to add it (`legume mcp`, registered like any tool). Until then, or if
the notebook refuses `readings` or the kinds `answer`, `evidence`, `offer`
or `choice` because the tool is older than 0.5.0:

- write the readings as a numbered list in the text of kind `conclusion`
  (the old name), marked "proposed, not decided";
- write the person's reply as
  `{"kind": "note", "by": "user", "text": "Answer to the proposal of <time>: chose R1. The user said: \"<exact words>\""}`;
- write an offer as a numbered list in a `note` (yours: no `by`), and the reply to it as
  `{"kind": "note", "by": "user", "text": "Choice on the offer of <time>: chose 2. The user said: \"<exact words>\""}`.
- number the chosen readings H1, H2, ... yourself, and write each table as an
  `observation`: the hypothesis, then one row per source (source, kind,
  what it says, the numbers), and the verb section 8.3 allows.

On a 0.5.0 server, kind `conclusion` still works, but it is recorded as a
proposal with no numbered readings, and an `answer` cannot refer to it. Use
`interpretation` with `readings` instead, even if a tool's instructions say
`conclusion`.

## 15. A short worked example

```text
Person:  Explore the IFN-treated PBMCs. I have no specific hypothesis.

Agent:   (project folder from the instructions: /data/pbmc-ifn, looks right)
         [notebook plan]  Question: what differs between IFN and control?
           Data: data/pbmc.zarr.zip, 6 donors. Check: donors 5 and 6 held out.
         [data-beans split by donor: explore set (1-4), held-out set (5-6)]
         [data-beans qc, no out: a preview]  Median 1,904 genes per cell.
           A 15% mito cutoff would drop 12% of cells; donor 4 would lose 30%.
         [notebook observation]  (the numbers above)
         [notebook interpretation, 3 readings]
           R1 donor 4 is low quality          check: rerun without donor 4
           R2 stress in treated cells         check: heat-shock genes by condition
           R3 the mito cutoff is too strict   check: compare 15% and 20%
         Which of these hold? Reply with numbers, or a short line.

Person:  ok

Agent:   That doesn't tell me which hold. Which of R1 to R3? A number is enough.

Person:  3, try 20%

Agent:   [notebook answer, chose [3], words "3, try 20%"]
         [data-beans qc with a 20% cutoff, out results/.../qc20.zarr.zip]  ...

         (later: the person chose R1, "Topic 7 is higher in treated than in
          control monocytes because of the interferon response")
         [notebook evidence, H1, our cells, donors 1-4, our_cells, agrees]
           Verb: "marks" or "is associated with, in these donors".
           Not yet used: other donors, another study, ...
         [notebook offer, 5 options]
         R1 now allows "is associated with, in these donors". Next
         sources, pick any:
           1 held-out donors 5-6         2 the same cells' ATAC (chickpea)
           3 a published IFN data set    4 a knockout's genes as a gene set
           5 stop here

Person:  1, then 3

Agent:   [notebook choice, chose [1, 3], words "1, then 3"]
         [the same steps on donors 5 and 6, every setting frozen]  ...
         [notebook evidence, H1, held-out donors 5-6, other_donors, agrees]
           Verb: "is associated with". Standing: replicated: sources that share no cells agree.
```

Every bracketed action is a tool call. Every decision is in the notebook,
with who made it.

## 16. Glossary

- **Agent**: the AI model that calls the tools.
- **Answer**: the person's reply to a proposal, recorded exactly, with the
  readings they chose.
- **Choice**: the person's reply to an offer, recorded exactly, with the
  options they chose.
- **Contrast**: a fact and its foil: why this rather than that (section 8).
- **Evidence** (notebook kind): what one source says of one hypothesis;
  the notebook builds the table from it (section 8.5).
- **Fact / foil**: the case to explain, and the closest case where it does
  not happen.
- **Hypothesis**: a reading the person chose, numbered H1, H2, ... for the
  whole project.
- **Interpretation**: what the results mean. Proposed by the agent as
  readings; decided by the person's answer.
- **Job**: a long step running in the background, with an id like `job-3`.
- **Notebook**: `results/<day>/notebook.md`, the day's account.
- **Observation**: what was done and seen, with numbers. Facts only.
- **Offer**: numbered options put to the person that are not readings,
  such as which source of evidence to use next.
- **Outside data set**: data published by others, used to confirm, refine or
  try to disprove a working hypothesis (section 9).
- **Post-selection**: a result computed on the same cells used to choose the
  labels or settings. Not a confirmation.
- **Prediction**: what result would support a reading and what would refute
  it, written in a `plan` before the data are seen.
- **Project folder**: where all records are written.
- **Pseudobulk**: summing cells per donor or sample, so that donors, not
  cells, are compared.
- **Reading**: one possible interpretation, with a claim, a rationale, a
  check, and optionally evidence and alternatives.
- **Session**: a tool's own screen running in the background; the person
  opens it with the `attach` command a job's status gives.
- **Source** (of evidence): data that differ in where they came from (other
  cells, donors, a study, a kind of study), with a main bias of their own.
  Several tools on the same data are one source (section 8.3).
- **Triangulation** (causal): combining independent sources of evidence
  whose biases differ; agreement between them is stronger than any one
  alone. The aim of the work (section 1).
- **Working hypothesis**: a reading the person chose that is worth more work.

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
