# recursive-companion

An investigation companion collapsed around `fork` (fork-subsumes-split
step 4, superseding the PRD 21 C3 layer-walk slice): one turn is **one
top-level typed request**. The session it opens decomposes the operator's
question by forking typed sub-answerers of its own — `async (fork @T
"brief")`, recursively, each a full multi-round session — and the **driver**
services that tree: spawn-time depth/descendant budgets, operator-page node
lifecycle, journal receipts. The fold is ordinary Haskell in the session's
own block: the code after the `wait`s.

Direction and locked decisions: [`plans/fork-subsumes-split.md`](../../plans/fork-subsumes-split.md).

## The shape

- **`loop`** (`Harness.hs`): seed question (`askUser @SeedQuestion`, once,
  operator-provided) → `runLLMTurn @Text (rootPrompt st)` → store the answer
  verbatim in `State.lastAnswer`. Nothing recursive lives in the authored loop.
- **Every session's answer type is the `@Type` at its invocation site.**
  The root's is `Text` because `loop`'s own call site starts plain
  (author-evolvable); interior types are model-designed per `fork @T` — a
  session declares the record it wants back, and the driver pins the child's
  `finalize` to it.
- **Budgets are the driver's** (8 deep / 32 descendants per tree, refused
  loudly at spawn). The companion carries no caps and no gate: operator
  policy, where a run wants one, is authored Haskell over the ask machinery
  (`askUser` before spawning), not driver interception.
- **`render`** (`HarnessTypes.hs`): the last answer verbatim, the question,
  and the companion protocol — the teaching every session in the turn's tree
  inherits through its framing (delegate contract, operator-steering ask,
  ancestry-scoped declaration rules). The multi-round rhythm, fork/async
  batching, and finalize contract are taught by the driver's own answerer
  framing, not re-taught here.

## What the operator sees

Fork children are first-class nodes on the operator page (fork-subsumes-split
step 3): birth, seed brief, timeline, final typed value or failure — plus the
loop-level turn-complete note carrying the root's answer between turns.

## Running it

**Scripted / unattended:** the CI tier —
`tidepool-harness/tests/companion_collapsed_slice.rs` (seed gate + a forking
turn, scripted record-replay, no live model) and
`tidepool-harness/tests/delegate_positive_path.rs` (the delegating row).
`dogfood_harness_typecheck.rs` pins the module against the driver's outer
row.

**Live (attended):**

```bash
./harness-dogfooding/run.sh harness-dogfooding/recursive-companion/Harness.hs
```
