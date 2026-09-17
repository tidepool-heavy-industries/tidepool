# DSL review: the mode-interpreted record sketch and its authored uses

Recorded 2026-09-16. Sol owns abstraction and boundary design for this DSL.
This is a mechanical soundness review of
[`Sketch.hs`](../../jev-integration/haskell/Sketch.hs), its four compile-fail
fixtures, and the authored examples in
[`jev-notebook-microprograms.md`](../jev-notebook-microprograms.md),
[`jev-bash-lsp-examples.md`](../jev-bash-lsp-examples.md),
[`jev-shoal-examples.md`](../jev-shoal-examples.md), and
[`jev-core-shoal-uses.md`](../jev-core-shoal-uses.md), with recommendations
motivated by today's observations. Names are illustrative.

## What the sketch establishes

- A closed type family `Field mode endpoint` interprets one authored record
  under `Questions`, `Answers`, `Descriptions`, `Probabilities`, and
  `Handlers result`. This mirrors `Tidepool.Agent.Contract`'s `:-` and is the
  right idiom for this codebase.
- `Option payload description` keeps a local payload (an `Edge`, an
  `Evidence`, a command value, an actor handle) out of the wire entirely. Only
  the description goes to Jev. This is the single most important property:
  selection returns the original typed value, never a label to parse.
- `Handlers result` gives heterogeneous elimination: `follow :: Edge -> r`
  and `finish :: Evidence -> r` in one record, consumed by `matchChoice`.
- `Selected scope options` and `Distribution scope options` carry a phantom
  scope with nominal role annotations, so candidates from two results cannot
  be mixed and `coerce` cannot bypass it. `withChoice` exposes the scope only
  when needed; `matchChoice` does not require it.
- `Group` and `Each` nest records and runtime-sized keyed collections, and
  their `Answers` interpretation retains the same shape.
- The fixtures prove mixed scopes, wrong payloads, cross-scope coercion, and
  missing handlers are rejected, the last only under
  `-Werror=missing-fields`.

The authored examples read naturally. `pick policy answer.target` returning a
typed payload is the ergonomic core, and the `Locate`, `Assess`, `NextSteps`,
`Traverse`, `Attention`, and `Completion` records show that the vocabulary
scales from one question to a dozen without changing shape.

## Recommendations

### 1. A candidate pool position

Today's state-referenced experiment shows candidates can be described once in
`state` and referenced from several questions by key with null descriptions,
with answers matching the inline form on one four-edge case. That is enough
to justify the position and a proper comparison, not a claim that quality
holds in general. The traversal coalgebra wants exactly this: one
`edges` pool, one Choice over it, per-edge Nouls over it, and a witness Choice
over it. With descriptions inline in each question the pool is repeated three
times.

Add a `Pool` endpoint. Under `Questions` it serializes its keyed descriptions
into state under its field name. `Choice (From pool)` and `Each (Over pool)`
reference the pool's keys; their criteria carry null descriptions plus any
extra alternatives such as no-match. Under `Answers` they retain candidate
identity exactly as `Choice` does today. The pool record is the same
mode-interpreted record used for descriptions, probabilities, and handlers.

This also makes candidate coverage explicit: a question over a pool cannot
name a key the pool lacks, and a response key outside the pool plus extras
fails structural validation.

### 2. Keys are semantic; derive them from selector names

Keys bias inference. For static alternatives, derive the wire key from the
snake-cased record selector, as the agent contract does with `toSnakeCase`.
Authors then get semantic keys by naming fields well, and `follow_publication_
gate` versus `stop_with_current_witness` is the natural outcome.

For dynamic candidates, the checked constructor should reject empty keys,
duplicate keys, and keys containing the `Each` path separator. Without the
last, `a.b` under `c` and `a` under `b.c` produce the same flat question id.
Rejection is cheaper than escaping and the constraint is easy to state.

Provide a pure `coalesce` combinator that merges alternatives with equal
descriptions before request construction, returning a mapping from merged key
to the original payloads. The handoff established that equivalent alternatives
under different keys do not split evenly.

### 3. Add Score

The sketch has no Score mode. Levels are ordered, so a level record's Generic
field order is the level order and `Probabilities` mode works unchanged. The
`Answers` interpretation should carry the expectation, the structured legend
exactly as returned, the per-level distribution, and confidence. Do not reduce
the legend to text.

Score is the right primitive for ordered action ladders: "background
information, next checkpoint, blocks next action, invalidates work" is an
escalation order, and expectation plus confidence gives policy a threshold to
move. The entity-alignment cookbook's insight that three levels can be three
actions applies directly to wake policy.

### 4. `pick` returns the distribution too

The examples' `pick policy answer` returns a payload or a typed stop. It
should return the selected payload with its probability, the runner-up with
its probability, and the confidence, so policy can detect near-ties. Today's
per-commit experiment split 0.37/0.31 across two runs; a `pick` that hides
that would make a coin flip look like a decision.

### 5. Make missing handlers a compile error by configuration

Exhaustive `Handlers result` records need `-Werror=missing-fields`. The eval
compiler options for session modules are ours to set in the GHC pipeline.
Setting that warning as an error for authored modules is a total fix that
needs no builder DSL. Note it in the model-facing library documentation so an
author understands the error.

### 6. Validation splits into structure and arithmetic

The Rust handler should validate structure and return a typed error: answer
keys equal question keys, kinds match, Choice selections and probability keys
equal the submitted candidate set plus extras, Score legend and probability
indices equal the submitted levels, values are finite and in range. It should
not gate on a distribution summing to one; the handoff saw correct answers
with rounding drift past 0.01. Arithmetic checks are warnings attached to the
response, and policy may renormalize.

`interpret.rs` in the research crate already computes these checks. It is
the seed of the production validator, minus the sum gate.

### 7. The `:-` operator collides

`Tidepool.Agent.Contract` defines `:-` as a closed family with a `TypeError`
fallthrough. A Jev module cannot add instances. Options: a qualified Jev
operator that authors import from one module, or one shared open family with
per-mode `TypeError` instances that restore the custom messages. Records that
mix agent endpoints and Jev endpoints are unlikely, so the qualified operator
is the smaller change. This is Sol's call.

### 8. Request and response records

The request should be one record with `model`, `state`, and `questions`; the
response one record with the resolved model, `usage`, and `answers`. Keep the
resolved model, which may differ from the requested alias. Retain the raw
response behind the typed one for journaling.

## The authored examples, evaluated

**`Locate`** (one Choice over a checked candidate set) is the right smallest
unit and covers the evidence lens.

**`Assess`** (two Nouls plus a heterogeneous Choice) is the right shape for a
fold: sufficiency and contradiction are independent, and the disposition
retains different payload types per branch.

**`Traverse`** and the `expansionRequest` coalgebra want the pool position
above; otherwise they are right.

**`Attention` under `Each`** matches today's per-commit result exactly:
per-item Nouls plus a Score, flattened to `recipients.<id>.<field>`.

**`Completion`** (eight Nouls and a pure guard policy) is the model for stop
conditions. The policy reads probabilities as booleans at 0.5; a production
policy should read margins. Otherwise it is the right idea: stopping is a
Haskell policy over independent judgments, not a Jev verdict.

**The seven `jev-core-shoal-uses` packets** all parse and all have between
four and eight questions, which is the cadence that worked live. Their Choice
keys are semantic. Two of them repeat candidate descriptions across questions
and would benefit from the pool position.

## What the DSL does not need

- A batching layer, request coalescer, or speculative execution framework.
  One request is already a map; dependent calls are ordinary sequencing.
- A universal `ToJSON a => a` entry point. Positions differ in what they
  admit; position-typed wrappers over the existing `Tidepool.Aeson` codecs are
  enough.
- A BoundingBox constructor.
- A retry policy in Haskell. Transport, credentials, deadlines, and budgets
  stay in the Rust handler like every other effect.
