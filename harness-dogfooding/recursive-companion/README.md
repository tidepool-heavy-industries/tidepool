# recursive-companion

The C3 vertical slice of [PRD 21](../../plans/self-iterating-harness/21-recursive-companion-prd.md):
one root turn in which a **coalgebra** window finalizes a single `ThoughtF`
layer — a split into branches, or a local `Finish` — and, for a split, each
branch **descends recursively** from the context it inherited, all the way
down. As branches finish, an **algebra** window folds their typed results
back up, in branch order, at every node the recursion visited (leaves
included). The operator gets one folded answer, with the whole tree
inspectable underneath it — subordinate to the answer, not beside it.

Full design and locked decisions:
[`21-c3-recursive-companion-slice.md`](../../plans/self-iterating-harness/21-c3-recursive-companion-slice.md).

## The shape

- **Coalgebra** (`discover`, one `runLLMTurnFork @LayerProposal` per node):
  proposes either `ProposeFinish` (this node is a leaf; done) or
  `ProposeSplit` (a posture — Explore, Compare, or Challenge — plus a list of
  child branches, each with a title, role, and instruction). The root is
  structurally incapable of describing anything deeper than its own layer:
  `LayerProposal` has no recursive arm, so this is a type-level guarantee, not
  a prompt convention.
- **Descent**: each proposed branch becomes a fresh child node, seeded with
  context inherited from its ancestors (see "What this slice does NOT
  demonstrate" below), and recurses through the same coalgebra/algebra pair.
- **Algebra** (`fold`, one `runLLMTurnFork @FoldProposal` per node): runs at
  *every* node, leaves included, and folds a synthesis plus any open tensions
  from that node's children (or, for a leaf, from its own finish). This is
  the one place a fold becomes durable.
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

This slice registers the recursion's root node (`"root"`) through
`tidepool-web`'s existing multi-node operator GUI
(`tidepool-selfharness.rs` now calls `spawn_operator_server_multi` instead of
`spawn_operator_server`) so the surface the driver already renders is what
the operator opens in a browser when the gate is on. The recursion tree's
*individual* branch nodes are not yet independently addressable tabs — see
gap 2 in the design doc.

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

- **Inherited context is a rendered prompt, not a frozen snapshot.** Locked
  decision 2 wants every child to fork the exact post-coalgebra context. The
  authored surface has no way to do that today (`Harness::freeze_snapshot`/
  `fork_from_snapshot` have no production caller — see design doc §8 gap 1),
  so a child's prompt is instead built from a rendered summary of its
  ancestry plus the parent's rendered decision. This holds sibling isolation
  (each child is still a fresh node) but it means there is no cache-relevant
  shared verbatim prefix here — **this slice makes no cache-win claim**; no
  provider implementation in this tree parses or reports a cache metric.
- **Siblings execute sequentially**, always, regardless of what a layer's
  `ProposedStrategy` requests. `thoughtHylo` descends through `traverse`,
  which is sequential; a model that proposes `WantConcurrent` gets
  `Sequential` execution instead, and that transformation is stamped in the
  node's receipt and rendered in the tree (`strategy: proposed Concurrent,
  executed Sequential`) rather than silently downgraded.
