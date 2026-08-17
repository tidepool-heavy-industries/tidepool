# C3 vertical slice — the recursive companion harness

**Status:** scaffold (2026-08-17). Companion to [PRD 21](21-recursive-companion-prd.md)
lane C3; consumes [C0](21-recursive-companion-prd.md)'s `Tidepool.Thought`,
[the C1 mount seam](21-c1-mount-seam.md), and
[C2's scope trees and snapshots](21-c2-scope-trees.md). Decisions and mechanism
only — written BEFORE the harness, so the harness has a contract rather than an
archaeology.

The deliverable is `harness-dogfooding/recursive-companion/`: one root turn in
which a coalgebra window finalizes a `ThoughtF` layer (or a local `Finish`),
each branch descends recursively from inherited context, the algebra window
folds typed results in branch order, and the operator gets a folded answer with
the tree inspectable but not primary.

---

## 1. The driver's shape over `Tidepool.Thought`

The recursion is **ordinary Haskell in the AUTHORED OUTER LOOP**, exactly as
`harness-dogfooding/dev-tree/` runs `Swarm.hyloM` there. That placement is
forced, not chosen, and the reason is worth stating once so nobody re-derives
it: a nested answerer window CANNOT host the recursion, because
`Harness::new` builds its `child_cfg` as `fork_child_decls(&cfg.decls)` — this
node's row minus the fork-spawning effects — so a fork/fanout child compiles
against a row with no `Fork` in it and structurally cannot produce
grandchildren. Depth stops at two. Recursion therefore lives where `Fork` is
never removed: the authored loop.

```haskell
loop :: State -> Companion State
loop st = do
  let cfg  = st.config
      seed = rootSeed st
      coalg = fanOutCapped seedDepth cfg.maxFanOut
                (depthCapped  seedDepth cfg.maxDepth
                  (gatedLayer cfg (discover cfg)))
      alg   = fold cfg
  answer <- thoughtHylo alg coalg seed
  pure (recordAnswer answer st)
```

`thoughtHylo` is used **verbatim** from `Tidepool.Thought` — no second
recursion engine, no forked copy. Budget middleware is `Tidepool.Thought`'s own
`depthCapped`/`fanOutCapped`; the node-count cap is `nodeCapped`, which needs
`MonadState Int` and therefore rides on the seed instead (§6).

### 1.1 The seed

```haskell
data NodeSeed = NodeSeed
  { seedPath    :: NodePath          -- root-relative branch slugs; the node id
  , seedBrief   :: ForkBrief         -- title/role/instruction (Tidepool.Thought)
  , seedDepth   :: Int
  , seedSpent   :: Int               -- nodes already spent in THIS subtree's allowance
  , seedContext :: InheritedContext  -- what this node knows from above (§4)
  }
```

`seedPath` is the node's identity everywhere — the journal key, the GUI node id,
the render's tree line, the scripted provider's needle. One name, one derivation
(§3), never re-spelled per consumer.

### 1.2 The two seams

| Seam | Type | What enters |
|---|---|---|
| coalgebra | `discover :: Config -> NodeSeed -> Companion (ThoughtF NodeSeed)` | ONE window: `runLLMTurnFork @LayerProposal` |
| algebra | `fold :: Config -> ThoughtF NodeAnswer -> Companion NodeAnswer` | ONE window: `runLLMTurnFork @FoldProposal` |

Everything else in the file is compiled coordination and costs zero tokens.
The algebra runs at EVERY node (locked decision 8) — a leaf's algebra sees a
realized layer with no children, and that is the uniform place a fold becomes
durable.

**The root is never asked for descendant shape** (locked decision 1) and it is
the TYPE that guarantees it, not the prompt: `LayerProposal` (§2) has no
recursive arm. A coalgebra invocation is structurally incapable of describing
more than its own layer, so a model that tries produces a decode error, not a
deeper tree.

---

## 2. The answer types — what a window may finalize

Both live in `HarnessTypes.hs`, per the two-file convention AND per the
answerer-imports rule: `HarnessSource::answerer_imports` derives a window's
in-scope modules structurally from the sibling modules the harness file
imports, and the harness module itself is never among them. A type inlined
beside `loop` cannot be named by the window that is asked to finalize it.

```haskell
-- The COALGEBRA's answer. One layer. No recursion, by construction.
data LayerProposal
  = ProposeFinish { finishDraft :: Text }
  | ProposeSplit
      { splitPosture     :: Posture           -- Explore | Compare | Challenge
      , splitFocus       :: Text              -- focus / decision / claim, per posture
      , splitStrategy    :: ProposedStrategy  -- WantSequential | WantConcurrent | WantPooled Int
      , splitBranches    :: [ProposedBranch]
      }
  deriving (Generic, FromJSON, JsonSchema)

data ProposedBranch = ProposedBranch
  { branchTitle       :: Text
  , branchRole        :: BranchRoleWire       -- Primary | Alternative | Critic
  , branchInstruction :: Text
  } deriving (Generic, FromJSON, JsonSchema)

-- The ALGEBRA's answer. v1 is deliberately thin — artifact selection and
-- composition are C4's, and PRD open question 2 says the type shrinks freely
-- until persistence depends on it.
data FoldProposal = FoldProposal
  { foldSynthesis :: Text
  , foldTensions  :: [Text]
  } deriving (Generic, FromJSON, JsonSchema)
```

`ProposeSplit` carries a plain `[ProposedBranch]`, not a `NonEmpty`: the wire
shape a model writes must be an ordinary JSON array, and emptiness is a
CONDITION the driver detects rather than a shape the model is trusted to
respect. `layerFromProposal` is the total, pure conversion:

```haskell
layerFromProposal :: NodeSeed -> LayerProposal -> ThoughtF NodeSeed
```

- `ProposeFinish` → `Finish (Draft text ModelFinished depth)`.
- `ProposeSplit` with a non-empty, well-formed branch list → the matching
  `Explore`/`Compare`/`Challenge` with `NE.fromList` over seeds built by
  `childSeed` (§4).
- `ProposeSplit` with an EMPTY branch list, or any branch with a blank title or
  blank instruction → `Finish (Draft why (InvocationFailed (NodeFailure why)) depth)`.
  A split that declares no branches is not a finish the model chose; it is a
  window that failed to produce a usable layer, and `FinishOrigin` is where that
  distinction already lives (C0 built the constructor for exactly this).

Pure and total, so the whole shape-validation story is one function a test
calls directly with no model anywhere.

`ProposedStrategy` is recorded and rendered but **not scheduled** in v1 —
see §7.

---

## 3. Node identity — `NodePath`, and why it is slugified

```haskell
newtype NodePath = NodePath [Text]        -- root-relative, outermost first

renderPath :: NodePath -> Text            -- "root", "root/1-cheaper-index", ...
```

The root is `NodePath []`, rendered `"root"`. A child at zero-based branch index
`i` with proposed title `T` extends its parent by one segment:

```
<i+1> "-" slug T          e.g. "2-risk-of-drift"
```

`slug` lowercases, replaces every run of non-`[a-z0-9]` with `-`, trims leading
and trailing `-`, and truncates to 32 characters; a title that slugs to the
empty string becomes `branch`. The leading index makes siblings unique even
when two titles slug identically, and makes branch ORDER readable in every
receipt.

**The slug is a containment requirement, not cosmetics.** `tidepool-web`'s
loopback trust model rests on "`node_id` is always a substrate identifier a
caller passed to `register_node` (convention: a branch name), never
model-produced text" — that is the crate's whole reason for having no
injection-surface story. A branch title IS model-produced text. Slugging to
`[a-z0-9-]{1,32}` with an integer prefix is what keeps the invariant true when
the tree's node ids are derived from a model's own branch labels, and it is
asserted directly (§9) rather than left to the renderer's escaping.

---

## 4. Inherited context — what v1 actually sends down

Locked decision 2 wants every child to fork the frozen post-coalgebra context.
**That is not reachable from the authored surface today; see §8, gap 1.** What
v1 does instead is stated plainly here so no receipt overclaims it:

```haskell
data InheritedContext = InheritedContext
  { inheritedAncestry :: [Text]   -- one rendered line per ancestor: posture, focus, this branch's brief
  , inheritedDecision :: Text     -- the PARENT's coalgebra decision, rendered
  }
```

A child's window prompt is `renderInherited ctx <> renderBrief seedBrief <>
<the coalgebra instruction>`. This is prompt-rendered inheritance: the shared
prefix is text the driver composes, not a frozen transcript prefix the harness
branches from. Consequences, all of them real:

- there is no `SnapshotDigest` to put in a receipt, so the journal records
  `inheritedBytes` (the exact byte length of the rendered inheritance) and
  nothing that resembles a digest or a cache claim;
- sibling isolation holds anyway — each child window is a fresh node
  (`drive_fanout_child` → `create_root_framed(…, "", …)`), so no sibling's
  output can reach another by construction;
- ancestor context is a summary, not the ancestor's verbatim exchange, so the
  "enormous shared prefix, tiny divergent suffix" cache shape PRD 21 is built
  around is NOT demonstrated by this slice.

The seam is named at exactly one place in the harness — `childSeed` — so the
swap to `fork_from_snapshot` when gap 1 closes touches one function.

---

## 5. Window rows and the invocation table

Both windows are OUTER-loop `runLLMTurnFork @T`, serviced by
`SelfHarnessDriver::service_outer_fanout` → `drive_fanout_child`. Each gets its
own freshly-minted answerer realm on the shared outer machine and its own scope
(C2), and is retired at finalize.

| # | Invocation | Row it compiles against | Answer contract | Per node |
|---|---|---|---|---|
| 1 | coalgebra `runLLMTurnFork @LayerProposal` | `[AskUser, Fork, ReadState, Finalize LayerProposal]` | `LayerProposal` | exactly 1 |
| 2 | algebra `runLLMTurnFork @FoldProposal` | `[AskUser, Fork, ReadState, Finalize FoldProposal]` | `FoldProposal` | exactly 1 |
| 3 | gate `askUser @LayerApproval` | the OUTER row | `LayerApproval` | 0..N per split (§6) |

`runLLMTurnFork`, not `runLLMTurn`: the plain form lands on the driver's ONE
reused per-loop answerer node, which accumulates every hole's exchange into a
single flat context. That would put every sibling's output into every later
node's window — precisely what locked decision 2 forbids. The fork form mints a
fresh node per window, which is the isolation the design needs.

**The gate cannot live inside a window.** `drive_fanout_child_inner` supports
`finalize` only; a nested `askUser`/`note`/`fork` from a fanout child gets a
clear error naming the gap, not service. So the layer-approval gate is raised by
the AUTHORED LOOP between the coalgebra and the descent
(`service_outer_askuser_hole`), which is also the right place on the merits: the
gate is the operator's authority over the driver, not a capability the model
holds.

The outer row is unchanged — `[RunLLMTurn, AskUser, Console, Worktree,
RepoEvent, Exec, Subagent, Journal]` (`selfharness::driver::outer_decls`). This
harness uses `RunLLMTurn`, `AskUser`, `Console`, and `Journal`; it declares no
new effect and asks for no row widening (that is C5's, reserved to the
operator).

---

## 6. The gate, and its policy

```haskell
data GatePolicy
  = GateOff              -- unattended: every layer auto-approved
  | GateWiderThan Int    -- ask only when a layer proposes more than N branches
  | GateEveryLayer
  deriving (Generic, FromJSON, JsonSchema)
```

`GateOff` is what the scripted acceptance tier runs under, and it is a real
configuration rather than a test hook — an unattended companion turn is a
legitimate mode.

The form is ONE record, so it renders as one form rather than a variant chooser:

```haskell
data LayerApproval = LayerApproval
  { gateVerdict  :: GateVerdict     -- Approve | Prune | Amend | Add
  , gateTarget   :: Text            -- branch title the verdict applies to ("" for Approve/Add)
  , gateTitle    :: Text            -- Add: the new branch's title
  , gateRole     :: BranchRoleWire  -- Add: the new branch's role
  , gateText     :: Text            -- Amend/Add: the instruction
  , gateNote     :: Text
  } deriving (Generic, FromJSON, JsonSchema)
```

`applyGate :: LayerApproval -> ThoughtF NodeSeed -> Either Text (ThoughtF NodeSeed)`
is **pure** — a test drives every verdict with no operator and no model:

- `Approve` — identity.
- `Prune` — drop the branch whose title matches `gateTarget`. **Pruning the LAST
  branch is refused** (`Left "pruning the last branch would leave the layer
  empty"`) and the gate is re-presented. Every `FinishOrigin` C0 defines names
  either the model's choice or a budget or an invocation failure; an operator
  emptying a layer is none of those, and the harness does not invent a fourth
  or borrow a wrong one. The operator who wants the subtree gone prunes to one
  branch, or ends the turn.
- `Amend` — replace the matching branch's `instruction`.
- `Add` — append a branch built from `gateTitle`/`gateRole`/`gateText`. Its
  path index is its new position, so ids stay dense and ordered.

A verdict naming a title no branch has is `Left`, re-presented. The gate loop
is bounded by `gateMaxRounds` (default 8, matching `ASKUSER_MAX_REPROMPTS`'s
spirit); at the bound the layer proceeds as last amended, and the fact that the
bound was hit is journaled.

The gate runs AFTER the fan-out cap and depth cap (so the operator is never
shown a layer the budget already refused) and BEFORE any descent.

---

## 7. Budgets, forced finish, and the Strategy transformation

Three caps, all from `Tidepool.Thought`'s middleware where the type fits:

| Cap | Mechanism | Stamped as |
|---|---|---|
| depth | `depthCapped seedDepth cfg.maxDepth` | `BudgetForced ForcedDepth` |
| fan-out | `fanOutCapped seedDepth cfg.maxFanOut` | `BudgetForced ForcedFanOut` |
| node count | seed-carried allowance (`seedSpent`, divided among children like dev-tree's `childAllowance`) | `BudgetForced ForcedNodeCount` |

`nodeCapped` is `MonadState Int m`, and the outer row is not a `MonadState`
stack, so the node budget is carried STRUCTURALLY on the seed instead: a node
reserves one unit for itself and divides the remainder among its children, so
the run's total is bounded with no mutable counter and completion order cannot
reach it. Same shape, same determinism guarantee, and `Tidepool.Thought`'s own
`nodeCapped` stays the pure-fixture spelling.

**The Strategy transformation is explicit, per locked decision 9.** `thoughtHylo`
descends through `traverse`, which is sequential. A model that proposes
`WantConcurrent` therefore gets `Sequential` execution, and that transformation
is stamped in the node's receipt and rendered in the tree
(`strategy: proposed Concurrent, executed Sequential`). This is not a silent
downgrade and not a scope cut — decision 9 provides for exactly this ("a
model-proposed `Strategy` is transformed EXPLICITLY (shown transformed in the
UI) and the transformation stamped in the receipt").

Why sequential, mechanically: the one concurrent primitive available to the
authored loop is `runLLMTurnFanout`, which fans out one WINDOW per prompt and
retires each at finalize. A window cannot host a recursive subtree (§1), so
`runLLMTurnFanout` can parallelize a layer of coalgebra windows but not a layer
of SUBTREES.

**The upgrade path, written down so nobody re-derives it.** When the
green-threads lane folds, the authored loop gains `async`/`mapConcurrently` over
the outer row, and subtree-level concurrency drops in by INTERPRETING `Strategy`
at the descent — without touching the fanout machinery at all. So the descent is
not an inline `traverse`; it is one named function:

```haskell
traverseLayer :: Strategy -> (Branch a -> Companion b) -> ThoughtF a -> Companion (ThoughtF b)
```

implemented today as an ordinary order-preserving sequential traversal that
ignores its `Strategy` argument except to record the transformation. That is the
single function green threads replaces, and branch order is preserved by
construction either way.

---

## 8. Substrate gaps — escalated, then ruled on

Three, found by building against the real surface, escalated before any runtime
change, and ruled on: **gap 1 is a parallel lane** (context-ref: Haskell-visible
window identity plus branch-from-frozen-prefix), **gap 2's v1 mitigation is
accepted** and Haskell-nameable node registration is queued for C5's typed-UI
scope, and **gap 3 is closed inside this lane** at the verb level. Each is stated
below as found, with its ruling.

### Gap 1 — the frozen-snapshot seam has no authored-surface reach

`Harness::freeze_snapshot`/`fork_from_snapshot` (C2 §4) have **no production
caller** — `grep` finds only `tests/companion_snapshots.rs`. The authored loop's
window primitives (`runLLMTurnFork`/`runLLMTurnFanout`) mint every child through
`SelfHarnessDriver::drive_fanout_child`, which calls
`Harness::create_root_framed(title, "", answerer_framing)` — a fresh root with
an EMPTY opening. Two things are missing, and the second is the deeper one:

1. the driver never branches a window from a frozen prefix, and
2. a window has no Haskell-visible IDENTITY at all — the loop receives a typed
   value, never a handle — so the authored driver cannot even name the window it
   wants to branch from.

Consequence for this lane: locked decision 2 is not demonstrated. §4 states what
v1 sends instead and refuses to call it a snapshot.

**Ruling: a parallel lane owns this.** The context-ref lane builds the
effect-surface primitive — Haskell-visible window identity plus
branch-from-a-frozen-prefix, starting from the sketch of a window's own digest
returned alongside its answer and accepted as an optional branch-of parameter
(`runLLMTurnBranch @T :: ContextRef -> Text -> M (T, ContextRef)`). C3 does not
wait on it: the slice proves the driver, gate, journal, and GUI mechanics either
way, and §4's single named swap point is what makes adopting it a fast
follow-up.

### Gap 2 — the multi-node GUI registry is not reachable from an authored harness

`AppState::register_node` is Rust-only, and `SelfHarnessDriver` holds ONE
`gate: Arc<dyn OperatorGate>` bound to one node. Nothing carries an authored
node id from Haskell to the web crate: driver `Event`s carry `NodeId` (an
opaque `u64` for a driver-minted answerer node), never a `NodePath`. So the
recursion tree cannot be registered as tabs without either a new Haskell-visible
verb or a driver-side announcement channel.

**Ruling: the v1 mitigation is accepted.** §10.3's plan — register the root
through `spawn_operator_server_multi`, render the tree through the harness's own
`render` with the folded answer primary — stands, and Haskell-nameable node
registration is queued for C5, where it belongs beside the typed-UI work.
`register_node` being idempotent is why it can wait.

### Gap 3 — a window's `InvocationExit` aborts its siblings

Locked decision 6 requires a child window's abnormal exit to fold as `NodeFailed`
at its branch position. Today `drive_fanout_child_inner` returns
`Err(DriverError::Session(..))` on round exhaustion / non-finalization, and
`service_outer_fanout` propagates it with `?` — so one branch's failure fails the
whole outer turn, erasing every sibling result. There is no `Either`-shaped
window verb: `runLLMTurnFork @T` resumes with `T` or the loop dies.

**Ruling: closed inside this lane**, at the verb level rather than as a
driver-side policy knob — the caller folding `NodeFailed` at the branch position
IS the design, so the type hands it to them. Specified in
[21-c3-exit-verb.md](21-c3-exit-verb.md); the shape:

- `runLLMTurnFork @T :: Text -> M (Either InvocationExit T)` and
  `runLLMTurnFanout @T :: [Text] -> M [Either InvocationExit T]`, changed IN
  PLACE rather than grown a `try`-prefixed sibling — one spelling, matching the
  codebase's own typed-failure idiom (`run`, `llm`, #335) — with exactly one
  Haskell caller outside the extractor internals to update.
- `runLLMTurn @T` keeps its signature: its failure is the outer turn's failure,
  not a branch position, and it has no siblings to erase.
- The line that decides what becomes a typed exit: a failure attributable to ONE
  CHILD'S WINDOW (round exhaustion, non-finalization, that child's own
  compile/provider failure) is typed; a failure of the MECHANISM (fanout
  cardinality, sum/list assembly against the table, session bookkeeping, the
  per-loop inference-call runaway cap) still hard-fails. Laundering a broken
  mechanism into "the model failed" would be a false receipt.

Two classes then fold as data, and both are exercised (§9): a window that
finalizes a structurally unusable layer (§2 — an empty split, a blank branch),
and a window that exits abnormally. The harness reaches both through ONE
function (`runWindow`), which is also what makes the rewire onto the new verb a
single edit.

---

## 9. Acceptance — the scripted tier

Runs on `KeyedProvider` (the needle-matched provider `tests/outer_fanout.rs`
already defines for exactly this reason: concurrent/ordered windows cannot be
served by `ReplayProvider`'s strict FIFO). Each window's prompt embeds its
`NodePath`, so a scenario is a table of `(path-needle, finalize reply)` and the
whole tree is deterministic and unattended.

New file `tidepool-harness/tests/companion_recursive_slice.rs`, one fixture
harness, family-bundle discipline (one compile shape, many assertions).
Additionally: `harness-dogfooding/recursive-companion/` joins
`dogfood_harness_typecheck.rs` as its third probe (the outer row, no extra
imports/decls beyond the universal contract).

| # | PRD 21 C3 acceptance line | Scenario | Assertion |
|---|---|---|---|
| 1 | the root is never asked for descendant shape | root splits 2; each child splits again | the root's window served exactly ONE reply, and `LayerProposal` has no recursive arm (a compile-level fact, asserted by the type's own shape test) |
| 2 | grandchild recursion from inherited context | depth-3 tree | the depth-2 window's prompt contains its parent's rendered decision AND its grandparent's ancestry line |
| 3 | branch-order delivery, never completion order | 3 siblings, the FIRST delayed longest | the algebra's rendered layer lists branches in declared order |
| 4 | failure accumulates as data — unusable layer | branch 2 finalizes an empty `ProposeSplit` | branch 2 folds as `InvocationFailed`; branches 1 and 3 still fold their real answers |
| 4b | failure accumulates as data — abnormal exit | branch 2's window never finalizes (round exhaustion) | branch 2 folds as `InvocationFailed` carrying the rendered `InvocationExit`; siblings' answers all arrive |
| 5 | budget-forced finish | `maxDepth 2` against a tree that wants 4 | the depth-2 nodes carry `BudgetForced ForcedDepth`, stamped in the receipt and the render |
| 6 | node-count cap | `maxNodes` smaller than the proposed tree | the overflow branches carry `BudgetForced ForcedNodeCount`; the total window count is exactly the cap |
| 7 | fan-out cap | a layer proposing more branches than `maxFanOut` | that node finishes with `BudgetForced ForcedFanOut` and NO child window runs |
| 8 | the gate is exercised through the form API | `GateEveryLayer` + a scripted gate answering `Prune`, then `Approve` | the pruned branch's window never runs; the survivor's does |
| 9 | gate policy auto-approves unattended | `GateOff` | no `askUser` suspension is raised at all |
| 10 | the turn is journaled per node event | any scenario | one journal entry per `split`/`fold`/`forced`/`failed`, keyed by `NodePath` (§10) |
| 11 | node ids are containment-safe | a branch titled with punctuation/markup | every emitted node id matches `^[0-9]+-[a-z0-9-]{1,32}$` per segment |

Pure-function tests (no model, no driver) for `layerFromProposal`, `applyGate`,
`slug`/`renderPath`, and the child allowance live beside the harness as ordinary
Haskell exercised through the typecheck probe's exported surface, mirroring
dev-tree's `resumePlanFor`/`childAllowance` precedent.

Verification commands for the lane are the task's own list; the harness-side
one is `scripts/battery.sh -p tidepool-harness -E 'binary(dogfood_harness_typecheck)
+ binary(companion_recursive_slice)'`.

---

## 10. Journal, render, and the GUI

### 10.1 Journal kinds and keys

`record :: Text -> Text -> Value -> M ()` (kind, key, payload). **Key is always
`renderPath` of the node the entry is about.** Kinds:

| kind | when | payload |
|---|---|---|
| `turn` | once, at the start and once at the end of the root turn | `{root, config}` / `{answer, nodes, windows}` |
| `split` | a coalgebra produced a layer with branches | `{posture, focus, strategyProposed, strategyExecuted, branches:[{path,title,role}], inheritedBytes}` |
| `finish` | a coalgebra produced `Finish` | `{origin, draft}` — `origin` is the rendered `FinishOrigin`, so budget-forced and model-chosen are distinguishable without a second kind |
| `gate` | the gate was presented | `{verdict, target, note, rounds}` |
| `fold` | an algebra folded a node | `{synthesis, tensions, children:[path], depth}` |
| `failed` | a node folded as a failure | `{reason}` |

`record` is write-only here; resume is not built (PRD 20 S1-L5 is a different
lane, and PRD 21's persistence section explicitly defers durable branch resume).
The kinds above are chosen so a future resume fold has what it needs — a `split`
entry names its children's paths, which is the only durable record of the tree's
shape — without this lane reading anything back.

### 10.2 Render — the folded answer is primary

`render :: State -> Text` (pure, `HarnessTypes.hs`) emits, in order:

1. **the folded answer** — the root's synthesis, as prose, unadorned;
2. the tensions the root fold surfaced;
3. `--- tree ---` and then one indented line per node:
   `<path>  <posture/finish>  <title>  [origin/strategy badges]`;
4. a one-line receipt: nodes, windows, forced finishes, failures, gate
   interventions.

The tree is inspectable and subordinate, which is the C3 lane's own wording
("tree inspectable in the GUI without being the primary answer surface"). It is
BELOW the answer, not beside it, and it is one line per node rather than a
nested transcript.

### 10.3 GUI

Given gap 2, the plan is what is reachable:

- the folded answer and the tree are the harness's rendered `State`, which the
  operator page already shows for the driver's node — so the primary surface is
  correct with no web change;
- `tidepool-web/src/bin/tidepool-selfharness.rs` switches from
  `spawn_operator_server` to `spawn_operator_server_multi` and registers the
  ROOT node id (`"root"`) alongside the default, which is the honest extent of
  "register the recursion tree via the existing multi-node surface" until a node
  id can cross from Haskell. Registration is idempotent, so this composes with
  whatever closes gap 2.

No `render.rs`/`shell.rs` change; no redesign of `tidepool-web`; nothing asserts
on its markup.

---

## 11. The live scenario — prepared, not run

`harness-dogfooding/recursive-companion/README.md` carries the exact launch line
for PRD 21's first dogfood scenario (a real design decision, attended, gate on).
It is one documented command; the operator runs it. The scripted tier above is
what runs unattended in CI, and nothing in this lane calls a live model.
