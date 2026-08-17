# C3 vertical slice — the recursive companion harness

**Status:** scaffold (2026-08-17). Companion to [PRD 21](21-recursive-companion-prd.md)
lane C3; consumes [C0](21-recursive-companion-prd.md)'s `Tidepool.Thought`,
[the C1 mount seam](21-c1-mount-seam.md), and
[C2's scope trees and snapshots](21-c2-scope-trees.md). Decisions and mechanism
only — written BEFORE the harness, so the harness has a contract rather than an
archaeology.

The deliverable is `harness-dogfooding/recursive-companion/`: one root turn in
which a coalgebra window finalizes a `ThoughtF` layer (or a local `Finish`),
each branch descends recursively from its parent's FROZEN post-coalgebra
context (§4), the algebra window
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
      coalg = journaled                       -- records what the DRIVER did (§10.1)
                (gatedLayer cfg
                  (fanOutCapped seedDepth cfg.maxFanOut
                    (depthCapped seedDepth cfg.maxDepth
                      (allowanceCapped (discover cfg)))))
  rootRef <- freezeContext          -- the root's own ref; see §4
  f <- thoughtHylo (foldNode cfg) coalg (rootSeed st rootRef)
  answer <- f (NodePath [])
  pure (recordAnswer answer st)
```

**The gate is the OUTERMOST wrapper, and the order is load-bearing.**
`fanOutCapped` decides AFTER its inner coalgebra has run — fan-out is a
property of the produced layer, not of the seed — so a gate nested beneath it
would present the operator a layer the fan-out cap then discards, which is
exactly what §6 forbids. Read outside-in and that is also the order the
policies fire: gate last, fan-out cap on the produced layer, depth and
allowance caps before the window runs at all.

`thoughtHylo` is used **verbatim** from `Tidepool.Thought` — no second
recursion engine, no forked copy. Budget middleware is `Tidepool.Thought`'s own
`depthCapped`/`fanOutCapped`; the node-count cap is `nodeCapped`, which needs
`MonadState Int` and therefore rides on the seed instead (§6).

### 1.1 The seed

```haskell
data NodeSeed = NodeSeed
  { seedPath    :: NodePath          -- root-relative branch slugs; the node id
  , seedBrief   :: ForkBrief         -- title/role/instruction (Tidepool.Thought)
  , seedDepth     :: Int
  , seedAllowance :: Int             -- node budget REMAINING for this subtree, incl. this node
  , seedRef       :: ContextRef      -- the frozen context this node's window branches off (§4)
  }
```

It lives in `Harness.hs`, not `HarnessTypes.hs`, and that placement is forced
by `seedRef` — see §4.

`seedAllowance` holds what is LEFT, not what was spent: a node reserves one
unit for itself and divides the remainder among its children (§7), and a
spent-counter cannot be divided. Naming it for the remainder is what keeps the
next reader out of an off-by-a-whole-subtree hazard.

`seedPath` is the node's identity everywhere — the journal key, the GUI node id,
the render's tree line, the scripted provider's needle. One name, one derivation
(§3), never re-spelled per consumer.

### 1.2 The two seams

| Seam | Type | What enters |
|---|---|---|
| coalgebra | `discover :: Config -> NodeSeed -> Companion (ThoughtF NodeSeed)` | ONE window: `runLLMTurnBranch @LayerProposal` off the parent's ref (§4) |
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
      , splitStrategy    :: ProposedStrategy  -- WantSequential | WantConcurrent | WantPooled {pooledWidth}
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

**Every payload-carrying sum arm takes a NAMED field.** `WantPooled
{pooledWidth :: Int}`, not `WantPooled Int`. The vendored generic JSON has no
key to put a positional field under and rejects it with a compile-time
`TypeError` (`GAllFieldsNamed`) — so a positional arm is not a style question,
it does not compile. Same for `GatePolicy`'s `GateWiderThan {gateWidth}` in §6.
Constructor names, and therefore wire tags, are unaffected.

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

## 4. Inherited context — freeze, then branch

Locked decision 2 is what this slice implements: **every child forks the frozen
post-coalgebra context of its parent's own window.** Gap 1 closed on trunk
(§8), so this section is the mechanism rather than a description of what v1
sends instead:

```haskell
data NodeSeed = NodeSeed
  { seedPath      :: NodePath
  , seedBrief     :: ForkBrief
  , seedDepth     :: Int
  , seedAllowance :: Int
  , seedRef       :: ContextRef   -- the frozen context THIS node's window branches off
  }
```

Two verbs and one rule:

```haskell
freezeContext    :: M ContextRef   -- freezes the CALLING window's prefix; not a window, never an Either
runLLMTurnBranch :: ContextRef -> Text -> M (Either InvocationExit (a, ContextRef))
```

The `Either` wraps the WHOLE pair, and §8 gap 3 has the reason: a window that
never finalized minted no context of its own to hand on, so a shape that could
represent "failed, but here is a ref" would be a lie waiting to be believed.

`loop` mints the root's ref with `freezeContext` before `thoughtHylo` — that
ref is the companion loop's own accumulated context, which is exactly what the
root's coalgebra should fork from — so `discover` is UNIFORM: every seed that
reaches it holds a ref, and there is no root special case to get wrong.
`discover` opens that one window, records what it said under `proposed`
(§10.1), and on a `Right` re-stamps the seed with `myRef` before
`layerFromProposal` builds the layer — so `childSeed`'s `parent.seedRef` is the
parent's POST-coalgebra ref, the decision it just made included. On a `Left` the
node becomes a leaf whose `FinishOrigin` is `InvocationFailed`, and no ref is
wanted because a `Finish` has no children to seed.

Consequences, all of them real:

- the shared prefix is the ancestor's ACTUAL transcript, byte-identical
  through the freeze point, not a summary of it composed by the driver — so a
  grandchild's window genuinely contains its grandparent's exchange rather
  than a rendered line about it;
- the receipts are the RUNTIME's and are re-derived by it, never minted by the
  harness: `Event::SnapshotFrozen{digest,prefix_bytes}` at each freeze, and
  `Event::BranchInvocation{snapshot,shared_prefix_bytes,branch_suffix_bytes}`
  at a branched window's first turn — written ONLY for a node minted through
  `fork_from_snapshot`, and only after the harness's own re-digest-and-compare
  check passes. The harness's own journal carries no ref and no byte count
  beside them (§10.1);
- declaration inheritance rides along: a branched child's decl scope is minted
  as a real CHILD of the origin window's scope, so locked decision 4 holds
  through the branch path rather than trivially at `ScopeId::ROOT`;
- the "enormous shared prefix, tiny divergent suffix" SHAPE is now what this
  slice actually produces. It is still **not** a cache-win claim: no provider
  impl in this tree emits `cache_control` breakpoints, and `cached_input_tokens`
  is `None` unless a provider volunteers it.

**The ref is a CAPABILITY, threaded as a value only.** It never enters `State`
(checkpointed JSON), is never interpolated into a prompt, and is never
reconstructed from text — possession is permission, and a ref only ever comes
from `freezeContext` or from a `runLLMTurnBranch`'s own return. `ContextRef`
has `Show`/`Eq` so `NodeSeed` still derives them; nothing renders a seed into a
prompt, a payload, or a receipt, and nothing should start.

**The seed had to move, and that is forced.** `ContextRef` is declared by
`RunLLMTurn`'s own decl, so it exists only in a row containing that effect —
and `HarnessTypes.hs` must stay compilable in the ANSWERER's row (`[AskUser,
Fork, ReadState, Finalize T]`, no `RunLLMTurn`) or the window types it defines
become unnameable by the windows asked to finalize them (§2). So `NodeSeed`,
`childSeed`, `layerFromProposal` and `applyGate` live beside `loop` in
`Harness.hs`, still pure and still exported — exactly as dev-tree keeps its own
`NodeSeed` (which carries a `WorktreeHandle`) beside its `loop`, and exactly as
dev-tree's `resumePlanFor`/`childAllowance` are pure decisions living there.

---

## 5. Window rows and the invocation table

Both windows are OUTER-loop invocations serviced by the self-harness driver
(`service_outer_fanout` → `drive_fanout_child` for the fork form,
`service_outer_branch` for the branch form). Each gets its own freshly-minted
answerer realm on the shared outer machine and its own scope (C2), and is
retired at finalize.

| # | Invocation | Window's opening context | Row it compiles against | Answer contract | Per node |
|---|---|---|---|---|---|
| 1 | coalgebra `runLLMTurnBranch @LayerProposal seed.seedRef` | the parent's FROZEN post-coalgebra prefix (§4) | `[AskUser, Fork, ReadState, Finalize LayerProposal]` | `Either InvocationExit (LayerProposal, ContextRef)` | exactly 1 |
| 2 | algebra `runLLMTurnFork @FoldProposal` | an empty root, plus a RENDERED view of the realized layer in the prompt | `[AskUser, Fork, ReadState, Finalize FoldProposal]` | `Either InvocationExit FoldProposal` | exactly 1 |
| 3 | gate `askUser @LayerApproval` | — (the OUTER row, no window) | the OUTER row | `LayerApproval` | 0..N per split (§6) |

Never plain `runLLMTurn`: it lands on the driver's ONE reused per-loop answerer
node, which accumulates every hole's exchange into a single flat context. That
would put every sibling's output into every later node's window — precisely what
locked decision 2 forbids. Both forms above mint a fresh node per window, which
is the isolation the design needs.

**The algebra does not branch, and could not.** Two reasons, both recorded at
`foldWindow` in the code. (1) POLICY: PRD 21 gives the algebra's model window a
RENDERED view of the realized layer by default; mounting the live value is the
escalation PRD open question 3 gates, explicitly out of v1. (2) MECHANISM: a
branch needs a `ContextRef`, and the only ref a node ever mints is the one
`discover` gets back from its OWN coalgebra window — `ThoughtF` has no task slot
(`Swarm`'s `PlanF` does), so nothing carries a per-node value from a node's
coalgebra to its own algebra, which only ever sees `ThoughtF Folded`. This is
the same wall the gate-count receipt hits (§10.2), and the only relay (stamp it
into every child's seed, read it back off a child's answer) launders a
capability through the fold and still fails for a leaf, whose layer has no
children to read it off.

**The gate cannot live inside a window.** `drive_fanout_child_inner` supports
`finalize` only; a nested `askUser`/`note`/`fork` from a fanout child ends that
window with a typed `ExitNotFinalized` naming the gap (§8 gap 3), not service.
So the layer-approval gate is raised by
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
  = GateOff                          -- unattended: every layer auto-approved
  | GateWiderThan {gateWidth :: Int} -- ask only past N branches (named field — see §2)
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

It covers the FOLD descent. The DISCOVERY descent is `thoughtHylo`'s own
`traverse`, inside `Tidepool.Thought`, which this lane consumes verbatim — so
making discovery concurrent is that module's edit, not this harness's. Worth
knowing before C6 reads a receipt: the stamped transformation stands in for
concurrency that is reachable at one of the two descents, not both.

---

## 8. Substrate gaps — escalated, then ruled on

Three, found by building against the real surface, escalated before any runtime
change, and ruled on: **gap 1 is CLOSED on trunk** (the context-ref lane
landed; this slice consumes it), **gap 2's v1 mitigation is accepted** and
Haskell-nameable node registration is queued for C5's typed-UI scope, and
**gap 3 is closed inside this lane** at the verb level. Each is stated below as
found, with its ruling.

### Gap 1 — the frozen-snapshot seam has no authored-surface reach (CLOSED)

As escalated: `Harness::freeze_snapshot`/`fork_from_snapshot` (C2 §4) had **no
production caller** — `grep` found only `tests/companion_snapshots.rs`. The
authored loop's window primitives (`runLLMTurnFork`/`runLLMTurnFanout`) mint
every child through `SelfHarnessDriver::drive_fanout_child`, which calls
`Harness::create_root_framed(title, "", answerer_framing)` — a fresh root with
an EMPTY opening. Two things were missing, and the second was the deeper one:

1. the driver never branched a window from a frozen prefix, and
2. a window had no Haskell-visible IDENTITY at all — the loop received a typed
   value, never a handle — so the authored driver could not even name the
   window it wanted to branch from.

**Ruling: CLOSED on trunk, and this slice consumes it.** Commit `5080be91`
built the effect-surface primitive the escalation asked for, in the shape the
sketch named:

```haskell
freezeContext    :: M ContextRef   -- freezes the CALLING window's prefix; not a window, never an Either
runLLMTurnBranch :: ContextRef -> Text -> M (Either InvocationExit (a, ContextRef))
```

The `Either` wraps the WHOLE pair, and §8 gap 3 has the reason: a window that
never finalized minted no context of its own to hand on, so a shape that could
represent "failed, but here is a ref" would be a lie waiting to be believed.

serviced by `SelfHarnessDriver::service_outer_branch` over the existing
`freeze_snapshot`/`fork_from_snapshot` pair — so a branched child is a REAL
`fork_from_snapshot` child (which is why it writes an `Event::BranchInvocation`
receipt at all; an empty-root child cannot) rather than
`create_root_framed(…, "", …)`. It also mints each branched child's decl scope
as a real CHILD of the origin's scope, so locked decision 4's declaration
inheritance now holds through the BRANCH path rather than trivially at
`ScopeId::ROOT`. `tidepool-harness/tests/companion_context_ref.rs` is its
acceptance.

§4 is therefore the mechanism, not a description of an interim: locked decision
2 is demonstrated. The one thing that did NOT arrive with it is a live-value
mount into a window — the algebra still folds a rendered layer, per §5 and PRD
open question 3.

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

### Gap 3 — CLOSED (2026-08-17): the window verbs answer an `Either`

Was: `drive_fanout_child_inner` returned `Err(DriverError::Session(..))` on
round exhaustion / non-finalization and `service_outer_fanout` propagated it
with `?`, so one branch's window failure failed the whole outer turn and
erased every sibling result.

**Ruling: closed inside this lane** (LANDED 2026-08-17), at the verb level rather than as a
driver-side policy knob — the caller folding `NodeFailed` at the branch position
IS the design, so the type hands it to them. Specified in
[21-c3-exit-verb.md](21-c3-exit-verb.md); the shape:

- EVERY verb that opens a window at a branch position answers an `Either`:
  `runLLMTurnFork @T :: Text -> M (Either InvocationExit T)`,
  `runLLMTurnFanout @T :: [Text] -> M [Either InvocationExit T]`, and
  `runLLMTurnBranch @T :: ContextRef -> Text -> M (Either InvocationExit (T,
  ContextRef))` — the `Either` around the WHOLE pair for the last, since a
  window that never finalized minted no context of its own. Changed IN PLACE
  rather than grown a `try`-prefixed sibling — one spelling, matching the
  codebase's own typed-failure idiom (`run`, `llm`, #335). The authored `.hs`
  callers were updated, plus the inline-Haskell fixtures in the harness test
  suite (`golden_path`, `acceptance_fanout`, `turn_splice`,
  `acceptance_value_bind`) — the general Agent stack reaches the fork/fanout
  verbs through `Harness::answer_fork`/`answer_fanout`, which now wrap in
  `Right` for a `runLLMTurn`-sourced hole (`engine::ForkSource`).
- `runLLMTurn @T` and `freezeContext` keep their signatures: the first is
  answered in context (its failure is the outer turn's failure, and it has no
  siblings to erase), the second is not a window at all.
- The line that decides what becomes a typed exit: a failure attributable to ONE
  CHILD'S WINDOW (round exhaustion, non-finalization, that child's own
  compile/provider failure) is typed; a failure of the MECHANISM (fanout
  cardinality, sum/list assembly against the table, session bookkeeping, the
  per-loop inference-call runaway cap, a stale or unknown `ContextRef`) still
  hard-fails. Laundering a broken mechanism into "the model failed" would be a
  false receipt.

Two classes then fold as data, and both are exercised (§9): a window that
finalizes a structurally unusable layer (§2 — an empty split, a blank branch),
and a window that exits abnormally.

**The harness folds the two exits differently, and the difference is the
point.** A COALGEBRA exit means the node decided no layer, so `discover` makes
it a leaf whose `FinishOrigin` is `InvocationFailed`; its own algebra then folds
it like any other childless layer. An ALGEBRA exit means the layer was fine and
the FOLD failed — so `foldAt` replaces only what that node itself owed (its
synthesis and tensions) and rolls its children's answers, tree lines, and
accounting up **untouched**. Discarding them would erase completed sibling work
one level up, which is the same erasure decision 6 forbids at a branch position,
just reached from the algebra side. Neither exit aborts and neither is silent:
both journal under kind `failed`, tagged with which window produced them.

The harness funnels both invocations through `layerWindow`/`foldWindow`, two
adjacent one-line functions under a single comment block, so the rewire was one
edit each: both now pass the `Either` through unwrapped and their two callers
fold it, per the paragraph above. TWO rather than one polymorphic `runWindow :: Text -> Companion a`,
for a mechanical reason worth recording: extract's typed-yield site pass
rejects a `runLLMTurnFork @a` call at a bare type VARIABLE ("polymorphic
runLLMTurn site"), which is the same constraint that makes `Tidepool.Harness`
re-export `runLLMTurn` rather than wrap it.

One constraint that came with it, and it binds any harness: `InvocationExit`
lives in the per-fragment generated `Tidepool.Effects`, so an
`Either InvocationExit T` cannot be a cross-turn session VALUE BIND (the
cross-row bind guard refuses it, as it already did for `Schema`). Irrelevant
to this lane's authored loop — the `Either` is consumed inside one fragment —
but do not bind one by name across turns; project at the bind instead.

---

## 9. Acceptance — the scripted tier

**Built: `tidepool-harness/tests/companion_recursive_slice.rs`.** It drives the
SHIPPED `harness-dogfooding/recursive-companion/` harness through
`SelfHarnessDriver::run_one_cycle` — not a fixture copy, which would keep
passing while the deliverable rotted. Additionally:
`harness-dogfooding/recursive-companion/` joins `dogfood_harness_typecheck.rs`
as its third probe (the outer row, no extra imports/decls beyond the universal
contract).

Two knobs script a scenario, and between them they cover the matrix below:

- **The provider table.** Runs on `KeyedProvider` (the needle-matched provider
  `tests/outer_fanout.rs` already defines for exactly this reason:
  concurrent/ordered windows cannot be served by `ReplayProvider`'s strict
  FIFO), adapted in two ways the shipped harness forces. An entry carries a
  needle SET rather than one needle, so a scenario names a branch by POSITION
  and phase (`["NODE root/2-", "— DISCOVER"]`) instead of hardcoding a slug the
  harness derives from a model-produced title (§3, row 11). And a request is
  matched against the last message carrying a prompt HEADER rather than simply
  the last message — a branch child's request opens with its parent's frozen
  transcript (§4), and a starved window is re-prompted with the driver's
  round-cap ultimatum, which carries no header at all.
- **A seeded checkpoint.** Scenario config (`maxDepth`/`maxNodes`/`maxFanOut`/
  `gatePolicy`) varies per scenario and `initialState` is fixed, so each
  scenario writes a durable `persistence::Checkpoint` carrying its own `State`
  JSON and boots through `SelfHarnessDriver::restore` — the production restart
  path, not a test-only argument.

**What one compile actually costs, and how the file is bundled.** The driver
splices the restored `State` JSON into the fused `render`+`loop` compile, so ONE
CONFIG IS ONE COMPILE SHAPE: runs sharing a config share it, and runs differing
only in their provider table or gate script cost nothing extra. So the rows
below are grouped into FOUR configs — the depth cap, the node cap and the
fan-out cap are three different `Config` values and no run can hold two without
confounding which cap fired, while everything a `GateOff`, generously-budgeted
config can carry rides in one tree, and rows 8/8b share one `GateEveryLayer`
config between two runs. Answerer-side compiles are keyed by the reply block's
source, so one shared `ProposeFinish`, one shared `FoldProposal`, one shared
two-branch and one shared three-branch split serve the whole file.

Every row below is BUILT, and the table names the check that carries it. `A`
is the tree scenario (`GateOff`, `maxDepth 3`/`maxNodes 40`/`maxFanOut 5`:
`companion_tree_recurses_folds_and_contains_its_node_ids`), `B`/`C`/`D` the
three cap scenarios, `E` the shared gate config's two runs.

| # | PRD 21 C3 acceptance line | Scenario | Assertion |
|---|---|---|---|
| 1 | the root is never asked for descendant shape | A: root splits 5; one child splits again | the root's window was served exactly ONE reply, and `LayerProposal` has no recursive arm — asserted off the hole card's rendered shape DOCUMENT (`synopsis::type_document` over the turn's real compiled `DataConTable`, expanding through field types): `LayerProposal` occurs exactly ONCE in it, as its own `data` head, never as a field type of itself or of anything it reaches |
| 2 | grandchild recursion from inherited context | A: depth-3 tree | a grandchild's window is genuinely FORKED from its parent's frozen prefix, never an empty root: one `Event::BranchInvocation` per coalgebra window and NONE for the algebra's (empty-root) fork windows — they exist only for `fork_from_snapshot` children, see `companion_context_ref.rs` — the depth-2 branch naming the digest its PARENT's window froze, with `shared_prefix_bytes` equal to that frozen prefix's own byte count and a non-zero `branch_suffix_bytes`. Sibling branches at one node name ONE shared digest. Receipts are attributed to nodes by the driver's own `RunLLMTurnHole{prompt}`→`TurnStart{node}` emission, never by event order, and nothing here reads prompt TEXT |
| 3 | branch-order delivery, never completion order | A: 5 siblings | the algebra's rendered layer lists branches in DECLARED order (read off the root's fold prompt). No artificial per-sibling delay: the descent is sequential by construction today (`traverseLayer` is an order-preserving traversal that ignores its `Strategy`), so a delay would prove nothing — what this pins is the property that must survive when green threads make the descent concurrent (§7) |
| 4 | failure accumulates as data — unusable layer | A: branch 2 finalizes an empty `ProposeSplit` | branch 2 folds as `InvocationFailed`, and reaches its parent's realized layer as ordinary data with that origin; its siblings' own answers still arrive in that same layer |
| 4b | failure accumulates as data — abnormal exit | A: branch 5's window never finalizes (starved: prose, no block, round caps lowered to 1/2 so it costs four instant provider calls and no compiles); AND one interior node's ALGEBRA window starved the same way | the turn COMPLETES. The coalgebra exit makes branch 5 a leaf whose `FinishOrigin` is `InvocationFailed` carrying the rendered `InvocationExit` (`round exhaustion: …`), at its own branch position. The algebra exit replaces only that node's own synthesis (its line says `fold failed`) while its children's answers, tree lines and accounting roll up UNTOUCHED — the half that matters, since discarding them would erase completed sibling work one level up. Both journal under `failed`, tagged `coalgebra`/`algebra` |
| 5 | budget-forced finish | B: `maxDepth 2` against a tree that wants 3 | the depth-2 nodes carry `BudgetForced ForcedDepth`, stamped in the render; no coalgebra window ran for any of them (the cap is never itself the reason a window is spent), and a budget-refused node journals no `split`/`finish` of its own — only the `fold` every node gets |
| 6 | node-count cap | C: `maxNodes 4` against a tree that wants 6 | the overflow branches carry `BudgetForced ForcedNodeCount`, and the number of nodes that RUN a coalgebra window is exactly the cap. Not "the total window count": the cap is carried STRUCTURALLY on the seed (`seedAllowance`/`childAllowance`, §7) because `nodeCapped` is `MonadState Int` and the outer row is not, so what it bounds is the nodes that run — a refused node still exists, still folds, and still costs its own algebra window |
| 7 | fan-out cap | D: a layer proposing more branches than `maxFanOut` | that node finishes with `BudgetForced ForcedFanOut` and NO child window runs — but it spent BOTH its own windows, because fan-out is a property of the produced layer and the coalgebra had already run. The journal still records the `split` the coalgebra genuinely produced beside the render's forced finish: two different facts, kept apart rather than one retroactively rewriting the other |
| 8 | the gate is exercised through the form API | E: `GateEveryLayer` + a scripted gate answering `Prune`, then `Approve` | the pruned branch's window never runs; both survivors' do — and the survivor that was branch 3 is now `root/2-…`, since every accepted verdict re-derives the surviving branches' paths from their NEW positions. Two presentations and two submissions crossed the real form API, journaled under `gate` at the node they gated |
| 8b | an amended branch is WORKED as amended | E: `Amend` on branch 1, then `Approve` | branch 1's own coalgebra prompt carries the amended instruction and NOT the one it replaced — not just the rendered tree. A branch holds its `ForkBrief` twice (on the `Branch`, and inside the seed the child's window is prompted from); writing one and not the other renders right and works wrong, so this asserts the prompt, not the render |
| 9 | gate policy auto-approves unattended | A: `GateOff` | no form is presented at all, and nothing is journaled as a gate that never happened |
| 10 | the turn is journaled per node event | A | the exact multiset of `(kind, key)` the tree implies — `turn`/`split`/`finish`/`fold`/`failed`, every key a `renderPath` of the node its entry is about |
| 11 | node ids are containment-safe | A: a branch titled with punctuation, markup and non-ASCII | every id the run EMITTED (the journal's keys) is root-relative and every segment matches `<index>-<slug>` with the slug drawn from `[a-z0-9-]` and at most 32 characters; none of the title's punctuation/markup/unicode appears in any of them |

Also asserted off scenario A, though not one of the rows above: the explicit
`Strategy` transformation (§7, locked decision 9) — a root proposing
`WantConcurrent` carries `strategy: proposed Concurrent, executed Sequential`
on its own line, shown transformed rather than silently downgraded.

Pure-function tests (no model, no driver) for `layerFromProposal`, `applyGate`,
`slug`/`renderPath`, and the child allowance live beside the harness as ordinary
Haskell exercised through the typecheck probe's exported surface, mirroring
dev-tree's `resumePlanFor`/`childAllowance` precedent.

Verification commands for the lane are the task's own list; the harness-side
one is `scripts/battery.sh -p tidepool-harness -E 'binary(dogfood_harness_typecheck)
or binary(companion_recursive_slice)'`. Budget the wall time: the slice binary's
six scenarios run ~7 minutes with a WARM compile memo (the residual is
per-window JIT compilation, which nothing memoizes), and meaningfully longer
cold.

---

## 10. Journal, render, and the GUI

### 10.1 Journal kinds and keys

`record :: Text -> Text -> Value -> M ()` (kind, key, payload). **Key is always
`renderPath` of the node the entry is about.** Kinds:

**What the WINDOW said and what the DRIVER did are two kinds, not one.**
`discover` runs innermost, so a layer journaled there can still be discarded by
the fan-out cap or reshaped by the gate. Recording the proposal as `split` made
that kind name children that never ran, and miss children an operator added —
while `split`'s whole job below is to be the durable record of the tree's shape.
So `discover` writes `proposed`, and the outermost `journaled` middleware writes
`split`/`finish` after every policy has had its say. A capped node honestly
carries both: a `proposed` naming seven branches and a `finish` stamped
`BudgetForced ForcedFanOut`.

| kind | when | payload |
|---|---|---|
| `turn` | once, at the start and once at the end of the root turn | `{root, config}` / `{answer, nodes, windows}` |
| `proposed` | a coalgebra WINDOW returned (or exited) — written by `discover`, before any policy | `{posture, focus, strategy, branches:[title]}` / `{finish}` / `{exit}`. The friction log's raw material: a model's refused seven-branch layer is exactly what C6 wants to see, and no cap may erase it |
| `split` | the driver DESCENDED through a layer with branches | `{posture, focus, strategyProposed, strategyExecuted, branches:[{path,title,role}]}` — no ref, no digest, no byte count: the branch receipts are the RUNTIME's (`SnapshotFrozen`/`BranchInvocation`, §4), re-derived by it, and a second weaker claim minted here beside them would be worse than none |
| `finish` | the driver ended the node locally — model-chosen, budget-forced, or a failed window | `{origin, draft}` — `origin` is the rendered `FinishOrigin`, so all three are distinguishable without a second kind |
| `gate` | the gate was presented | `{verdict, target, note, rounds}` |
| `fold` | an algebra folded a node | `{synthesis, tensions, children:[path], depth}` |
| `failed` | a window exited | `{reason, window}` — `window` is `coalgebra` or `algebra`, because the two fold differently (§8 gap 3) and a node can carry both |

`record` is write-only here; resume is not built (PRD 20 S1-L5 is a different
lane, and PRD 21's persistence section explicitly defers durable branch resume).
The kinds above are chosen so a future resume fold has what it needs — a `split`
entry names its children's paths, which is the only durable record of the tree's
shape, and it is trustworthy for that precisely because a refused or reshaped
layer never produces one.

### 10.2 Render — the folded answer is primary

`render :: State -> Text` (pure, `HarnessTypes.hs`) emits, in order:

1. **the folded answer** — the root's synthesis, as prose, unadorned;
2. the tensions the root fold surfaced;
3. `--- tree ---` and then one indented line per node:
   `<path>  <posture/finish>  <title>  [origin/strategy badges]`;
4. a one-line receipt: nodes, windows, forced finishes, failures.

**Gate interventions are journaled, not counted in that line, and the reason is
structural.** A gate happens in a node's COALGEBRA; `ThoughtF` has no task slot,
so that node's algebra never sees its own seed, and with `thoughtHylo` verbatim
and no state slot in the outer row there is no channel carrying a count from a
node's coalgebra to its own fold. (One relay does exist — stamp the count into
each child's seed, return it on each child's answer, read it off any child —
and it is deliberately not taken: it works only because gates happen solely on
splits, which is a coupling that would break silently the day the gate moves,
in exchange for one number that kind `gate` already records per node with
verdict, target, note, and rounds.) A counted receipt wants a state slot in the
outer row or a wider fold reader; both are edits outside this lane.

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
