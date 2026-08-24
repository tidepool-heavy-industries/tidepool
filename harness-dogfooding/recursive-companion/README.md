# recursive-companion

An investigation companion collapsed around `fork` (superseding the earlier
PRD 21 C3 layer-walk slice, which proposed splits for authored machinery to
execute through a driver gate): one turn is **one
top-level typed request**. The session it opens decomposes the operator's
question by forking typed sub-answerers of its own — `async (fork @T
"brief")`, recursively, each a full multi-round session — and the **driver**
services that tree: spawn-time depth/descendant budgets, operator-page node
lifecycle, journal receipts. The fold is ordinary Haskell in the session's
own block: the code after the `wait`s.

**Locked decisions.** (1) Fork inherits everything the old split mechanism
had: fork children are first-class companion tree nodes (tree path/label
from the brief, node-seeded/timeline/node-finalized/node-failed on the
operator page, event tracking, budget accounting), through the same
lifecycle spine branch children use — one child-spawning mechanism. (2)
Gates are dropped, not ported: there is no driver gate interception at fork
time; operator-approval policy is ordinary authored Haskell over the ask
machinery (`plan <- askUser @ForkApproval …` before spawning) when a run
wants one, per the Mechanism Index's "operator interaction … never a second
channel" rule — the per-split consultation guarantee becomes a convention
unless the authored loop hard-wires an ask, and budgets bound a model that
never asks. (3) Budgets replace structural containment: recursive forking is
bounded by driver budgets enforced at spawn (per-window fan-out, depth, and
total-node caps) rather than by a fork-free child row or fixed depth.

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

Fork children are first-class nodes on the operator page: birth, seed brief, timeline, final typed value or failure — plus the
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
