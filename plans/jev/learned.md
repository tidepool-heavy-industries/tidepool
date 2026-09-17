# What we learned about Jev

Recorded 2026-09-16 from the vendored TypeSafe skill, the live documentation,
the crate's 316 captured exchanges summarized in
[`FRONTIER-EXPERIMENTS.md`](../../jev-integration/FRONTIER-EXPERIMENTS.md) and
[`CONTRACT.md`](../../jev-integration/CONTRACT.md), and four fresh live calls
made during this session over real repository state. Those four exchanges are
not in the evidence directory; their shapes and results are recorded here.

## What the model is

Jev is TypeSafe's first "System One" model. It takes one `state` (string,
object, or array), a map of questions, and a model name, and returns one
answer per question. Three question kinds:

| Kind | Answer | Meaning |
|---|---|---|
| Choice | selected key, probability per key, confidence | pick one of a supplied set; the distribution ranks the supplied alternatives |
| Score | expected position, legend, probability per level, confidence | position on an ordered rubric; the number is an expectation, not a generated value |
| Noul | probability of yes | whether a condition holds; no separate confidence |

Questions in one request are evaluated independently over the same state and
cannot see each other's answers. That is the whole contract: one request is a
wide snapshot of judgments over one world-state, never a chain. Dependent
judgments need a second request built from new evidence.

Instructions, Choice descriptions, Score levels, and Noul criteria all accept
structured JSON, and the model is trained on structure. Backticked paths into
state (`edges.publication_gate`) are the documented way to point a question at
a value. Question identifiers are code association keys and the model never
sees them; Choice keys and descriptions are model-facing.

Publisher claims that we did not independently measure: about 100 ms typical,
$0.042 per million input tokens, free output tokens, calibrated probabilities
from a decision-trained objective, self-consistent across repeats.

## Contract facts we confirmed

From the crate's probe matrix, all against `jev-latest` resolving to
`jev-1.13.0`:

- Request spine: `model`, non-null `state`, nonempty `questions` object, all
  required. One invalid question rejects the whole request.
- State: string, object, or array; empty forms accepted; bare number, bare
  boolean, and null rejected.
- Instructions: omitted, null, string, object, or array; bare scalars rejected.
- Choice: 1 to 255 alternatives; 256 rejected. Descriptions may be null,
  empty, or deeply structured. An empty-string key is accepted.
- Score: 1 to 10 levels; null level rejected. Legend returns the structured
  levels as submitted.
- Noul criteria: omitted, null, empty object, true-only, false-only, null
  descriptions all accepted. An unknown `maybe` member is accepted but its
  semantics are not established.
- Questions: 640 parallel questions succeeded; 1,024 hit a token limit.
  Single-question state with 768 irrelevant records passed; 896 and above hit
  the same limit.
- Usage returns `input_tokens` and `output_tokens`.
- An undocumented `bounding_box` discriminator exists and is disabled for the
  account. Out of scope.
- The account lists two models: `jev-latest` and `jev-preview`. During this
  session `jev-preview` also resolved to `jev-1.13.0` and answered
  identically, so there is nothing to learn from it yet.

## Behavioral properties that matter for design

**Semantic width is strong; exact depth is not.** Six-field conjunctive
selection among 255 near-neighbors, seven-edge semantic path validation,
temporal accepted/superseded/draft/off-branch reasoning, and six
mutually-checking Nouls all worked. Exact shuffled pointer chasing failed at
depth eight and above. Jev is not a graph interpreter. Haskell does exact
traversal, joins, counters, and ancestry; Jev judges the frontier Haskell
prepared.

**Option keys are model-facing and bias inference.** Two alternatives with
identical descriptions and different keys split 0.88/0.12 for
`route_a`/`route_b`, 0.90/0.10 for `first`/`second`, and 0.06/0.94 for
`option_z`/`option_a`. Consequences: coalesce equivalent continuations before
the call, give retained alternatives intentional semantic names, and never
read a duplicate-option split as calibration.

**Distributions can be rounded past a naive sum check.** Wide requests
occasionally returned probabilities summing to more than 0.01 away from one
while the winning key was right. Structural validity and semantic selection
are separate gates; do not reject a response on arithmetic.

**Dependent questions must state their premise.** Since siblings cannot see
each other, "if the mechanism is X, which check" must say so in the
instruction. This is the fan-out pattern the publisher documents, and it is
how one packet pre-decides several branches.

**Confidence is a shape statistic.** It summarizes distribution concentration
and is not correctness, permission, or workflow success. A Noul near 0.5 means
uncertainty, not medium intensity. Several acceptable alternatives legitimately
spread mass.

## Four fresh live calls over real repository state

All four used `jev-latest`, resolved to `jev-1.13.0`, and were sent with
plain HTTP from this session. Latencies are single samples.

### Mechanism-index routing

State: the root `CLAUDE.md` mechanism table, the repository one-implementation
rule, and a task ("append durable JSONL records of every Jev exchange and
issue a per-process identifier"). Six questions.

| Question | Result |
|---|---|
| Choice: which mechanism should the log extend | `reuse_repr_jsonl_primitive` at 1.0 |
| Choice: which mechanism issues the identifier | `reuse_repr_issuer` at 1.0 |
| Noul: would a private writer violate the rule | 0.71 |
| Noul: does the task need a new protocol effect | 0.30 |
| Noul: does the task touch supervision | 0.21 |
| Score: unstated design ambiguity, four levels | 2.56, split 0.43 moderate / 0.57 major |

172 ms; 994 input and 222 output tokens. Both routes are correct. The
ambiguity score leaning "major" is fair: log ownership and location were not
stated.

### `Each` over real git history

State: the last twelve commit subjects on `main` and an inquiry about stale
notebook display pages. Two Nouls per commit (touches notebook, is a behavior
change) plus one Choice over all twelve with a no-match alternative and one
Score for how many are in scope. 26 questions.

Per-commit relevance lined up with the subjects: the three notebook
implementation commits scored 0.77 to 0.92 on both Nouls; docs, fixture
refresh, and merge bookkeeping scored 0.07 to 0.26. The origin Choice spread
over the three notebook commits at confidence 0.31, and a second run swapped
the top two at nearly equal mass. That is the honest answer when only subjects
are visible, and it shows the distribution is the signal rather than the
argmax.

232 ms and 126 ms on the two runs; 1,853 input and 954 output tokens.

### Candidates described once in state, referenced by key

State: an inquiry, a current node, and an `edges` pool of four edges with
relation, destination, nearby source, and visited flag. Two Choices whose
criteria were the edge keys with null descriptions plus one no-match key, and
four per-edge relevance Nouls.

| Question | Result |
|---|---|
| Choice: next edge | `publication_gate` at 1.0 |
| Choice: witness edge | `publication_gate` at 0.83, `no_witness` 0.16 |
| Noul: publication_gate relevant | 0.96 |
| Noul: telemetry relevant | 0.14 |
| Noul: constructor relevant | 0.21 |
| Noul: retry_scheduler relevant | 0.16 |

355 ms and 189 ms on two runs; 797 input and 207 output tokens. This is new
relative to the handoff: candidate descriptions can live once in state and be
referenced from several questions by key with null descriptions, and on this
one four-edge case the answers matched the inline form. One success supports
further testing; it does not establish that quality holds across pool sizes
or harder frontiers. If it does hold, it halves tokens when several questions
range over one pool and keeps one source of truth for descriptions.

### `jev-preview`

The state-referenced and per-commit requests were rerun with
`model: jev-preview`. Both resolved to `jev-1.13.0` and answered within
rounding of `jev-latest`.

## What this means in one list

- One packet is a snapshot, not a conversation. Ask everything the current
  evidence can answer, then act, then ask again only at the next boundary.
- Put candidates in state once when several questions range over them.
- Keys are labels the model reads. Make them mean something.
- Always supply a real no-match alternative when none may fit, and a separate
  presence Noul when coverage is independently useful.
- Keep the distribution; policy decides, not argmax. Concentration is not
  accuracy: a confident answer is a confident answer, and only a checked
  outcome says whether it was right.
- Keep competing explanations alive when the distribution splits. Gather one
  discriminating observation per live hypothesis, then ask again over the
  enriched state. Ambiguity is a reason to collect evidence, not to return.
- Haskell owns everything exact. Jev owns relevance, sufficiency,
  contradiction, urgency, and selection among prepared alternatives.
- The observed cost of a normal packet is one to two thousand input tokens and
  a few hundred milliseconds. At published prices that is well under a tenth
  of a cent, which is what makes pervasive use plausible.
