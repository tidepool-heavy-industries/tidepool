# Fork subsumes split

**Status: steps 1–4 landed (2026-08-22/23, step 4 in `bdfc334a`).**

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

## Step 3 design note (2026-08-22)

Scope: give `drive_fork_child_window` (driver.rs) the same operator-GUI/tree
lifecycle `service_outer_branch` gives a branch child — a fork child today
runs on the full window pump but is invisible on the operator page and the
d3 tree.

**Label/path scheme.** A fork child has no wire-carried label (unlike
`runLLMTurnBranchLabeled`) and no `ContextRef` (it forks from the LIVE
parent, not a frozen snapshot), so both must be derived. Base path: the
parent's companion `NodePath` (`branch_node_paths.get(parent)`) when the
parent is itself a companion coalgebra node, else the parent's own
registered GUI label (`node_labels.get(parent)`) when the parent is itself a
labeled branch/fork child, else the fixed root id `"root"` (mirroring
`tidepool_web::DEFAULT_NODE_ID` as a literal — this crate cannot depend on
`tidepool-web`, same coupling shape as `parse_companion_node_path`'s
prompt-convention parsing). Child segment: `f<idx>-<ascii-slug-of-brief>`,
mirroring a structurally-labeled branch's own `root/1-child` convention so a
fork child's tree position reads the same way. `idx` is a per-PARENT
monotonic counter (`fork_child_seq: Mutex<HashMap<NodeId, u32>>`), assigned
INSIDE `drive_fork_child_window` rather than threaded in from a caller —
load-bearing, because both of `drive_fork_child_window`'s call sites
(`drain_answerer_fork`, and `service_thread_ready`'s async fork arm — the
latter is root's concurrent territory this change must not touch) already
pass a fixed argument list, and the boundary forbids editing
`service_thread_ready`. A per-parent counter also gives uniqueness across
repeated forks from the same parent over its lifetime, not just within one
`forkAll` batch.

Also mirrored from `service_outer_branch`: `engine::parse_companion_node_path`
runs on the fork's own BRIEF (not just branch prompts), populating
`branch_node_paths` for a fork child whose brief happens to be a companion
coalgebra prompt — same discipline, so a fork-shaped delegation attributes
correctly too.

**Guard choice: widen `BranchWindow`, not a sibling struct.** Checked reuse
first, per the mechanism-index rule. `BranchWindow`'s `validated_ref` field
is read only by `Drop`'s trace line, never for logic — widening it to
`Option<ContextRef>` costs nothing and lets a fork child (which has no
`ContextRef`) construct a guard with `None`. The success path differs
genuinely: a fork answer has no `(T, ContextRef)` pair to freeze (branch's
`finalize_data` calls `freeze_snapshot`), and a successful fork child's
durable ending must be `NodeDone` before retirement (seam map §7.10),
whereas `finalize_data` retires straight into `NodeCancelled`-via-
`terminate_node`. So `BranchWindow` gains one new consuming method,
`finalize_fork_data`, alongside the existing `finalize_data`/`fold_exit` —
`fold_exit` and the `Drop` impl are reused completely unchanged. No new
struct.

## Step 4 design note (locked with the operator, 2026-08-23)

The operator's corrections, verbatim where quoted, supersede the older step-4
sketch above:

1. **Every session's finalize type is exactly the `@Type` at its invocation
   site — "a Haskell value, as requested".** No harness-imposed answer
   schema (the proposed `Answer { answerText, tensions }` is rejected).
   Interior types are model-designed per `fork @T`; the ROOT's type is
   whatever the authored loop requests at its own `runLLMTurn` call site —
   start plain, author-evolvable: the invoker picks, same rule at every
   level.
2. **`Harness.loop` collapses to: seed question → ONE top-level typed
   request → render.** The tree emerges from the session's own `async (fork
   @T brief)` calls, serviced recursively by the driver — no authored
   recursion, no layer walk. `fork` remains available to authored harness
   code too (future use), not only to model turns.
3. **Companion `Config` caps retire.** `maxDepth`/`maxNodes`/`maxFanOut`/
   `gatePolicy`/`gateMaxRounds` are gone — the driver's spawn-time budgets
   (depth 8 / 32 descendants, landed in step 2) are the one containment
   mechanism. `State` keeps `question`/`turnCount`/`lastRun`.
4. **The receipt renders the root's typed value as-is** — no re-shaping into
   tensions/tree/accounting the loop no longer owns.
5. **`delegate` is unchanged.** Verified (not assumed): after the collapse,
   `takeDelegatedBranches`' fold-time read-back has NO Haskell caller left in
   the tree — the fold that consumed it is gone and delegate results already
   return inline (`delegateSummary`). Flagged as a vestige candidate for the
   identifier-sweep lane; that sweep landed (2026-08-23, architecture-review
   F3) — the whole cluster (`parse_companion_node_path`, `branch_node_paths`/
   `delegated_branches`, the `DelegateBranches` effect/decl, and the
   `fork_child_label` first-choice read) is deleted. `delegate` itself is
   unaffected: results still return inline via `delegateSummary`.
6. **Protocol text sheds all `ProposeSplit` teaching.** What survives in the
   per-turn framing: session persistence/ancestry-scoped declarations, fork
   as THE decomposition mechanism, delegate, operator asks, the multi-round
   batch rhythm.
7. **Sequencing: collapse FIRST, redeploy on it, run rounds against the new
   architecture.** The pre-collapse companion binary remains the wedge
   fallback.

What this deletes (the surveyed map): `Harness.hs` `walkGroup`/`walkNode`/
`foldAt`/`bulkDiscoverOutcomes`/`bulkLayerWindow`/`foldWindow`/
`discoverWith`/`discoverGroup`/`applyGate`/`gateRounds`/`childSeed`/
`childEdge`/`childAllowance`/`layerFromProposal`/`splitLayer`/`mergeFold`/
`mergeChildInto`/`resolveConflict`/`ownDelegatedBranch`/`NodeSeed` and the
prompt builders; `HarnessTypes.hs` `LayerProposal`/`ProposedBranch`/
`Posture`/`BranchRoleWire`/`FoldDecision`/`GatePolicy`/`GateVerdict`/
`LayerApproval`/`gateApplies`/`Config`/`NodePath` machinery/`NodeAnswer`/
the layer plumbing — roughly two thirds of the 2191 companion lines.
`SeedQuestion`/`OperatorSteering`/`State`/`RunSummary` (shrunk)/`render`
survive.

## Foundation already landed (2026-08-22)

The answerer-plane green scheduler (`service_answerer_green` — `async (fork
@T)` composes, dispatcher unifies askUser/fork/green servicing), the
per-window fork budget with loud refusal, `answer_fork_hole_raw`,
`drive_one_fork_child`, fork children resolving parent-declared types, and
the reachability split (`Tidepool.Async.Types`; `waitEvent` in
`Tidepool.Event`). Acceptance: `tidepool-harness/tests/answerer_async_fork.rs`.
