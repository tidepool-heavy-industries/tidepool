# recursive-companion

The C3 vertical slice of [PRD 21](../../plans/self-iterating-harness/21-recursive-companion-prd.md):
one root turn in which a **coalgebra** window finalizes a single `ThoughtF`
layer — a split into branches, or a local `Finish` — and, for a split, each
branch **descends recursively** from its parent's frozen post-coalgebra
context, all the way down. As each node's own subtree finishes, an
**algebra** window folds its children's typed results back up, at every node
the recursion visited (leaves included). The operator gets one folded
answer, with the whole tree inspectable underneath it — subordinate to the
answer, not beside it.

Full design and locked decisions:
[`21-c3-recursive-companion-slice.md`](../../plans/self-iterating-harness/21-c3-recursive-companion-slice.md).

## The shape

- **Coalgebra** (`discoverWith`/`discoverGroup`, one `runLLMTurnBranchFanout
  @LayerProposal` call per SIBLING GROUP — every child of one parent, forked
  off that parent's shared `ContextRef` and driven concurrently): proposes
  either `ProposeFinish` (this node is a leaf; done) or `ProposeSplit` (a
  posture — Explore, Compare, or Challenge — plus a list of child branches,
  each with a title, role, and instruction). The root is structurally
  incapable of describing anything deeper than its own layer: `LayerProposal`
  has no recursive arm, so this is a type-level guarantee, not a prompt
  convention.
- **Descent**: each proposed branch becomes a fresh child window BRANCHED off
  the frozen context of its parent's own window — the ancestor's actual
  transcript as its shared prefix, not a summary rendered into the prompt —
  and recurses through the same coalgebra/algebra pair. The root is no special
  case: `loop` freezes its own context first, so the root's coalgebra branches
  exactly like every descendant's. `walkGroup` drives one sibling group
  (bulk-discover, then recurse into each node); `walkNode` recurses into a
  node's own children as a new sibling group and then folds — the discovery
  and fold steps are the same single recursive walk, not two separate passes.
- **Algebra** (`foldAt`/`foldWindow`, one `runLLMTurnBranch @FoldDecision`
  call per node): runs at *every* node, leaves included, right after that
  node's own subtree (if any) has finished, and folds a synthesis plus any
  open tensions from that node's children (or, for a leaf, from its own
  finish). This is the one place a fold becomes durable. A childless
  non-root node folds mechanically (no window call at all — its finish text
  IS its synthesis); the root always gets a real fold window, even when
  childless.
- **Gate**: between a coalgebra's proposed layer and its descent, the
  authored loop can pause for operator approval (approve / prune a branch /
  amend an instruction / add a branch), governed by `gatePolicy`.

## Config knobs

| Knob | Meaning |
|---|---|
| `maxDepth` | hard cap on recursion depth; a node at the cap is force-finished (`BudgetForced ForcedDepth`) instead of splitting further |
| `maxNodes` | total node budget for the whole run, carried structurally on the seed and divided among children as the tree grows; overflow branches force-finish (`BudgetForced ForcedNodeCount`) |
| `maxFanOut` | cap on branches per layer; a layer proposing more finishes instead (`BudgetForced ForcedFanOut`) with no child window run for the excess |
| `gatePolicy` | `GateOff` (unattended, every layer auto-approved), `GateWiderThan Int` (ask only past N branches), or `GateEveryLayer` (ask every time) |

## What the operator sees

The harness's `render` puts the **folded root answer first**, as prose, then
the tensions the root fold surfaced, then a `--- tree ---` section with one
indented line per node (path, posture/finish, title, and any forced-budget or
strategy badges), then a one-line receipt (node count, window count, forced
finishes, failures, gate interventions). The tree is there to inspect, not to
read as the primary answer.

Every recursion node is independently addressable in the harness's own
tree/session model: each has its own `ContextRef`, its own coalgebra and
algebra windows, and its own path (`NodeSeed`/`NodeAnswer`/`NodePath`). The
operator GUI does not yet expose that per-node structure as separate tabs,
though — `tidepool-selfharness.rs` registers only the single default (`root`)
node, the same as every other harness, and the whole recursion narrates into
that one node's timeline. A speculative second `register_node("root")` for
this harness's own tree was tried and then removed (dogfood finding,
2026-08-19): until node ids can cross from Haskell into the GUI, nothing
routes an update to any node but the default, so registering one produced
only a permanently-empty second tab. Carrying node ids across that boundary
is still open work — see gap 2 in the design doc.

## Running it

**Scripted / unattended (`GateOff`):** this is the tier that runs in CI —
see `tidepool-harness/tests/companion_recursive_slice.rs` and
[`scripts/battery.sh -p tidepool-harness -E 'binary(dogfood_harness_typecheck) + binary(companion_recursive_slice)'`](../../plans/self-iterating-harness/21-c3-recursive-companion-slice.md).
No live model is called by this slice's own test suite.

**Live (PRD 21's first dogfood scenario — a real design decision, attended,
gate on):** the exact launch line, using `harness-dogfooding/run.sh`'s real
interface (a path to a `Harness.hs`, no other flags — `run.sh` only skips the
operator gate when `--yes`/`--auto`/`--replay` is passed to the underlying
binary, which this invocation does not do):

```bash
./harness-dogfooding/run.sh harness-dogfooding/recursive-companion/Harness.hs
```

**This line is prepared and has not been run.** Standing this slice up is
scripted-tier work with no live model involved; running the live scenario is
the operator's call to make, on their own machine, against their own model
credentials.

## What this slice does NOT demonstrate

- **A cache WIN.** Children now genuinely fork their parent's frozen prefix
  (locked decision 2, design doc §4), so the shared-prefix shape is real — but
  no provider implementation in this tree emits `cache_control` breakpoints,
  and `cached_input_tokens` stays `None` unless a provider volunteers it.
  **This slice makes no cache-win claim**; what it can show is the digest and
  the exact shared/suffix byte counts the runtime's own `SnapshotFrozen`/
  `BranchInvocation` receipts carry.
- **A live layer value in the algebra's window.** The fold window is still a
  `runLLMTurnBranch` over a RENDERED view of the realized layer. Mounting the
  live value is the escalation PRD 21 open question 3 gates, and `ThoughtF`
  has no task slot to carry a node's own ref from its coalgebra to its algebra
  anyway — design doc §5.
- **A model-visible scheduling knob.** Sibling branch windows are ALWAYS
  driven concurrently — one bulk `runLLMTurnBranchFanout` call per sibling
  group (`discoverGroup`), never one at a time — and there is no
  `splitStrategy` field left on `LayerProposal` for a model to request
  otherwise: scheduling is an implementation detail, not a choice the model
  makes. Folding, by contrast, is sequential: `walkNode` calls `foldAt` only
  after its own children have already been walked and folded, one node at a
  time up the tree it just descended.
