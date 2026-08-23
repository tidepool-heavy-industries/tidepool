# Fork subsumes split

**Status: direction locked (operator, 2026-08-22); step 1 not started.**

The companion's tree should EMERGE from model-authored forks — `async (fork
@T "you're in branch X")` — not from models proposing splits that authored
machinery executes. The `ProposeSplit → applyGate → harness-spawns-branches
→ separate fold window` pipeline collapses into ordinary Haskell in the
model's block: research, fork, wait, synthesize, finalize. The fold is the
code after the waits.

## Locked decisions

1. **Fork inherits everything split has.** Fork children become first-class
   companion tree nodes: tree path/label from the brief, `node_seeded` /
   timeline / `node_finalized` / `node_failed` on the operator page, event
   tracking, budget accounting. One child-spawning mechanism — fork servicing
   routes through the same lifecycle spine `service_outer_branch` uses today.
2. **Gates are dropped, not ported.** No driver gate interception at fork
   time. Operator-approval policy is written by the model (or the authored
   harness, when it must be mandatory) as ordinary Haskell over the ask
   machinery: `plan <- askUser @ForkApproval …` before spawning. This
   matches the Mechanism Index ("operator interaction … never a second
   channel") and the gate-unify-ask trajectory. Named trade, accepted: the
   per-split consultation guarantee becomes a convention unless the authored
   loop hard-wires an ask; budgets bound a model that never asks.
3. **Budgets replace structural containment.** The fork-free child row and
   depth-one bottoming-out give way to recursive forking bounded by driver
   budgets enforced at spawn (per-window fan-out — landed 2026-08-22 — plus
   depth and total-node caps).

## Order

1. **Children on the window pump** — fork children become full windows
   (multi-round, `askUser`, recursive fork) instead of the one-shot `resume`
   path; closes the ChildSuspended gap for real.
2. **Recursive rows + budgets at spawn** — drop the `Green`/`Fork` strip in
   `fork_child_decls`; add depth/total-node budgets beside the fan-out one.
3. **Lifecycle unification** — fork servicing through the branch spine
   (tree identity, GUI node lifecycle). No gate interception (decision 2).
4. **Companion collapse** — Harness.hs sheds `ProposeSplit`/`LayerProposal`/
   `FoldDecision`, the layer walk, `applyGate`/`gatePolicy`/`gateMaxRounds`
   and the Amend/Add wire types + their branch-prompt tests. Also dissolves
   the reason `renderBrief` had to stay in per-node prompts (gate-amended
   briefs living outside model turns). DESIGNED WITH THE OPERATOR before any
   code — it rewrites the companion's recursion and prompt architecture.

## Foundation already landed (2026-08-22)

The answerer-plane green scheduler (`service_answerer_green` — `async (fork
@T)` composes, dispatcher unifies askUser/fork/green servicing), the
per-window fork budget with loud refusal, `answer_fork_hole_raw`,
`drive_one_fork_child`, fork children resolving parent-declared types, and
the reachability split (`Tidepool.Async.Types`; `waitEvent` in
`Tidepool.Event`). Acceptance: `tidepool-harness/tests/answerer_async_fork.rs`.
