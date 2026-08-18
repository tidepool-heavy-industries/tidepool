# PRD — Recursive Companion

**Status:** proposed (2026-08-17)
**Owner:** self-iterating harness / companion track
**Input:** an external co-design session (ChatGPT, no repo access) with Inanna
produced the initial spec and a revision discussion; this PRD is that design
reconciled against the real substrate. Sibling, not successor, of
[PRD 20](20-exomonad-v3-prd.md): the companion track exercises contextual
recursion and typed value delivery; the swarm track exercises coordination at
scale. They share `Tidepool.Swarm` and the harness runtime.

## Summary

A persistent companion whose reasoning is a recursively discovered, typed
program. At each node one model invocation acts as the coalgebra — it either
finishes locally or defines only the next layer of branches — and, after the
children run, a second invocation in the parent context acts as the algebra,
folding their results. Children fork from the exact frozen post-coalgebra
context, so the tree shape is discovered locally, large context is inherited
by prefix, and branch prompts are tiny.

**The dataflow principle (locked):** prompt text for MEANING; Haskell values
for BEHAVIOR, IDENTITY, AUTHORITY, and COMPOSITION.

- **Down** (parent → child): the inherited context snapshot plus a short
  rendered brief. No typed seed delivery — a child's assignment is language,
  and this is exactly the cache-friendly shape (enormous shared prefix, tiny
  divergent suffix).
- **Up** (child → parent): typed `NodeResult` values. Closures, lenses, and
  explicitly escape-safe handles/capabilities ride up as live heap values
  held by the RUNTIME, never re-expressed as prose (agent, node, and
  worktree handles keep their own lexical lifetime rules and do not escape
  by default).
- **Into the algebra's model window**: a rendered view of the realized layer
  by default (summaries, tensions, artifact ids + intents, caller-computed
  edit previews); the ACTUAL live value mounted as an invocation-local
  binding when higher-order access buys something — calling a child-supplied
  function, composing two children's lenses, exhaustively matching an
  ancestor-declared rich sum. (A genuinely child-LOCAL type cannot be
  matched by the parent — its constructors are not in the parent's scope;
  child-local detail escapes only behind an ancestor-known type or
  eliminator. Type vocabulary flows down; values flow up.)

## Product boundary

The companion is: an open-ended brainstorming/design/decision partner; the
dogfood for contextual recursion, persistent declaration scopes, closure
delivery upward, and typed UI; able to delegate coding and repository
investigation to Codex subagents; a source of real frictions for the
substrate.

It is not: the Exomonad v3 coding factory; a workflow DSL; a requirement that
every question branch; a durable distributed system; a system that asks the
root to generate the whole reasoning program; a fresh-vs-forked context
experiment (v1 descendants always fork).

## Locked v1 decisions

1. **No up-front thinking program.** The harness owns one fixed recursive
   driver. A coalgebra invocation decides only the current layer.
2. **Every child forks the frozen post-coalgebra context**: everything before
   the node, the coalgebra exchange, its compiled blocks and declarations —
   no branch suffix, no sibling output. The snapshot is immutable; once it
   has children its prefix is never rewritten (compaction before a split
   creates a new cache root; existing child snapshots stay stable).
3. **Prompt-down, typed-up** (the dataflow principle above). A child's brief
   is `ForkBrief { title, role, instruction }`, rendered; a child's result is
   a live typed value.
4. **Declarations are a persistent lexical environment, not copy-on-write.**
   Everything is immutable; "write" is "create a descendant scope". Forking
   shares the transcript tip and declaration-environment tip; lookup walks
   local → parent; siblings shadow freely and never collide; the parent
   never gains child declarations by name. The runtime problem is LIFETIME,
   not mutation: ancestor realms and JIT code stay alive while descendant
   contexts, parked continuations, or escaped closures reference them, and
   retire when unreachable.
5. **The fan-out is applicative.** Siblings are independent given the frozen
   snapshot and may run concurrently; the algebra receives results in
   declared branch order, never completion order.
6. **Both invocations have required typed finalization contracts** (the
   existing `Finalize T` row-pinning and GHC correction loop). Round
   exhaustion, runtime failure, cancellation, and non-finalization are a
   typed `InvocationExit`, folded as data at the branch position — never an
   exception that erases sibling results.
7. **Edits are artifacts, applied by the caller.** A child returns
   `ProposedEdit` closures with intent metadata; the RUNTIME applies them to
   a known snapshot, checks invariants, renders before/after, and stamps
   `EditReceipt`s. The algebra's model window selects and orders artifacts
   by id (`FoldDecision`); Haskell resolves and composes the actual
   closures. The model never attests to its own execution.
8. **The algebra runs at every node** (uniform fold; one place for checked
   edits and effects). A leaf's algebra sees a realized layer with no
   children.
9. **Budgets clamp deterministically.** Depth, node count, fan-out, rounds,
   deadline, cost. At a hard cap the driver runs a forced local finish; a
   model-proposed `Strategy` is transformed EXPLICITLY (shown transformed in
   the UI) and the transformation stamped in the receipt.
10. **Codex is an effect, not the architecture.** A branch CAN delegate
    repository evidence and implementation to a coding subagent; recursive
    reasoning stays in the companion. **Settled (2026-08-18, operator):**
    delegation is a DIRECT answer-window effect — node windows spawn coding
    subagents themselves; narrowing the effect-set for child nodes is
    deferred until real runs motivate it. Node windows get NO raw
    `Worktree` verbs: repository access happens only through a spawned
    subagent's own exclusive `BindingTable` worktree binding, and changes
    still ride up as artifacts per decision 7.

## Core types (starting point — expected to be edited through dogfood)

```haskell
data ThoughtF a
  = Finish    { draft :: Draft }
  | Explore   { focus :: Text,    branches :: NonEmpty (Branch a), strategy :: Strategy }
  | Compare   { decision :: Text, options  :: NonEmpty (Branch a), strategy :: Strategy }
  | Challenge { claim :: Text,    attacks  :: NonEmpty (Branch a), strategy :: Strategy }
  deriving (Functor, Foldable, Traversable)

data Branch a = Branch { brief :: ForkBrief, value :: a }
  deriving (Functor, Foldable, Traversable)

data ForkBrief = ForkBrief { title :: Text, role :: BranchRole, instruction :: Text }

-- What the MODEL finalizes: no ids, no receipts — it cannot attest to
-- runtime facts. (finalize goes through the JIT-typed row, so this is not
-- subject to the subagent-schema single-constructor rule.)
data ModelContribution s = ModelContribution
  { view     :: NodeView
  , proposed :: [ProposedArtifact s]
  }

-- What the RUNTIME constructs around it: ids assigned, previews computed,
-- receipt stamped — and failure representable, per locked decision 6.
data NodeResult s
  = NodeSucceeded
      { contribution :: ModelContribution s
      , artifacts    :: [Artifact s]      -- id-stamped, held live by the runtime
      , receipt      :: NodeReceipt
      }
  | NodeFailed
      { failure :: NodeFailure
      , receipt :: NodeReceipt
      }

data Artifact s
  = EditArtifact     ArtifactId EditIntent (s -> Either EditFailure s)
  | EvidenceArtifact ArtifactId Evidence

-- The algebra selects and composes child artifacts by id AND may author new
-- ones of its own; the runtime resolves, applies, and stamps as ever.
data FoldDecision s = FoldDecision
  { synthesis   :: ContributionText
  , selected    :: [ArtifactId]
  , composition :: CompositionOrder
  , proposed    :: [ProposedArtifact s]
  }
```

`ThoughtF` describes one layer only. `Strategy = Sequential | Concurrent |
Pooled Int` controls scheduling, never context inheritance — even sequential
children fork the same snapshot.

## Substrate mapping — what exists, what is new

**Live already** (do not rebuild): the hylo core and derived-`Traversable`
extraction (`Tidepool.Swarm`, pinned on the JIT); typed finalize with the
compile-correction loop; closures as finalized answers delivered by handle;
concurrent windows in declaration order (realms-per-window, cap 8);
green-thread scheduling over parked continuations; depth-one `fork`/`forkAll`
sub-answerer windows; the decl plane (top-level declarations persisting by
name); typed forms/GUI; Codex subagents with parent-serviced tools; managed
worktrees and repository events; caller-side receipts everywhere.

**New runtime work, in order of risk:**

1. **Mounting a live value into a window as an invocation-local binding**
   (`realizedLayer :: ThoughtF (NodeResult s)` visible in scope, `:t` works,
   record-dot works). Preferred over a `ReadInput` effect: it reads as
   ordinary GHCi, which is the whole surface philosophy. Mechanism sketch:
   the closure-tenuring path (a suspended request's closure field evacuated
   as a persistent GC root, then handle-minted) already proven by the
   green-threads lane, pointed the other direction — a handle installed
   under a name in a window's declaration scope.
2. **Recursive context/scope trees with reachability lifetimes** — the
   persistent `DeclEnv` chain over realm machinery, snapshot identity
   (digests in receipts), and retire-on-unreachability. The realm registry's
   `stowed_roots_count == parked_count` accounting is where this gets
   pinned.
3. **Per-invocation effect rows for node windows** (GUI, Codex, worktrees
   inside a branch). Feasible — rows are per-turn compiles — but widening
   answer windows beyond `[AskUser, Fork, ReadState, Finalize T]` is a
   containment-posture decision reserved to the operator (lane C5).

## Lanes

- **C0 — pure fixture.** `ThoughtF`/`Branch`/`NodeResult`/budgets and the
  branch-order recursive driver as pure code over the existing hylo
  patterns; property tests: layer-at-a-time discovery, completion-order
  permutation invariance, failure accumulation, caps forcing local finish.
- **C1 — the mount spike (THE de-risk).** Prove a function-bearing value
  mounts into a window as a named binding, using a minimal record
  independent of C0 (`data Mounted = Mounted { applyMounted :: Int -> Int }`).
  The acceptance sequence exercises the whole lifetime: producer window
  finalizes `Mounted`; producer invocation ends; consumer window receives it
  as named `mounted`; consumer SUSPENDS on a parking effect; consumer
  resumes and evaluates `mounted.applyMounted 41`; optionally returns a
  closure capturing `mounted`; all handles/closures drop and root accounting
  returns to baseline. Mounted bindings are a new ownership class: either
  represent them through an existing counted handle class or extend the
  invariant with a mounted-root count — never quietly preserve
  `stowed_roots_count == parked_count` while adding non-parked roots.
- **C2 — context/scope trees.** Frozen post-coalgebra snapshots as explicit
  harness operations; persistent declaration environments; sibling prefix
  cache alignment (receipts record snapshot digest, shared-prefix and
  suffix token counts, provider-reported cache hits); lifetime/retirement.
- **C3 — vertical slice.** The fixed recursive driver in a new dogfood
  harness (`harness-dogfooding/recursive-companion/`): four constructors,
  concurrent siblings, hard budgets, folded text result, tree inspectable
  in the GUI without being the primary answer surface.
- **C4 — checked edits.** Artifacts by id, caller-applied with invariants,
  previews, `EditReceipt`s; `FoldDecision` composition; only state and
  receipts cross checkpoints (closures never do). Settled (2026-08-18,
  operator): approval is the PARENT'S fold — a child's edits are proposals
  to its parent's algebra, and selection in `FoldDecision` IS the approval
  that mints the consuming `ApprovedEdits` capability; no operator gate
  inside the tree (the operator sees receipts; a root-level gate can grow
  later if dogfood wants one). v1 apply-time invariants are the closure's
  own `Either EditFailure s` — no build/typecheck gate, because the
  companion's v1 edit targets are markdown/state, not code. The `ThoughtF`
  task-slot change stays DEFERRED: v1 checked edits receive artifacts from
  children up the fold and never need algebra access to coalgebra-minted
  values; open question 5's precondition rule stands for whichever future
  feature first needs it. Endorsement propagation settled (2026-08-18,
  operator): a fold's selection IS its endorsement — a selected artifact
  republishes upward under its original id, every non-root apply is a
  PREVIEW against the frozen turn-start draft, and only the root's own
  selection is the turn's one real, persisted application. Root's own
  `foldProposed` is forbidden in v1 (root selects, it never authors); an
  edit a fold omits is dropped from that route permanently.
- **C5 — effects in nodes.** Design fully settled (2026-08-18, operator);
  what remains is implementation. Row-widening: decision 10 as amended
  (direct subagent-spawn effect, no raw Worktree verbs) plus the
  worktree-coordination section. **Precondition this lane's GUI work should
  check first (learned in C4):** C4 landed the checked-edits MECHANISM
  (`FoldProduct`, the consuming `ApprovedEdits` capability, `resolveSelection`/
  `approve`/`applyEdits`, `EditReceipt`) as pure, property-tested code in
  `Tidepool.Thought` — it does not yet wire into
  `harness-dogfooding/recursive-companion`'s live fold window
  (`Harness.foldWindow` still finalizes the plain `FoldProposal`, never
  `FoldProduct`); nothing a real turn does today can produce a
  `ProposedEdits` value. Deriving an artifact-checklist form from
  `FoldDecision` needs that wire-level integration done first — it is a
  small, well-scoped follow-on (HarnessTypes gains a JSON `FoldProduct`/edit
  wire type per the existing `ProposedBranch`-style precedent, `foldAt`
  gathers a selection pool from children, `render`/journal gain a receipts
  line), not a re-derivation of the mechanism itself. Comparison/choice GUI:
  DERIVED through the existing generic askUser/forms surface (PRD 14/15) —
  `FoldDecision` renders as a ranked select over branch results + an artifact
  checklist +
  composition order; no bespoke widget unless dogfood proves the derived
  form cramped (the goal is strong generic tools that support many harness
  shapes). Cancellation: propagates transitively to leaves, including
  spawned subagents via cycle cancellation; interruption lands at the next
  suspension point (no mid-stream preemption in v1); a cancelled child
  folds as typed `InvocationExit` data at its branch position, completed
  siblings keep their results, the algebra runs over the partial layer,
  and cancelled nodes' lazy worktrees are discarded unmerged.
- **C6 — daily dogfood.** Friction log per turn (depth/width discovered,
  cache hit rates, cost, operator interventions, whether branching beat a
  single window). Promote surface changes only from real runs; baseline is
  the existing single-context companion at similar spend.

C0 and C1 start immediately and in parallel. C2 does not semantically depend
on C1 — they share lifetime machinery, and C1 establishing the generalized
root-ownership primitive first is efficient, not required. The real joins:
C3 needs C0 plus C2's context trees; higher-order mounted algebra input
needs C1; a prompt-rendered vertical slice could run without C1. C4/C5 are
independent after C3; dogfood begins as each lands.

## Worktree coordination (settled 2026-08-18, operator)

- **Worktrees are lazy.** Only a node whose work produces mergeable
  repository content acquires a worktree; a purely deliberative node is a
  no-op on this axis. Acquisition rides the existing managed-worktree
  machinery — nothing companion-specific.
- **Each node merges its children's worktrees into its own, in declared
  branch order** (the same order the algebra receives results, decision 5).
  The tree's commit history linearizes bottom-up — the exomonad fold
  pattern — and the root node's worktree is the turn's single integration
  point. There is no cross-tree shared trunk a node writes to directly.
- Conflict handling (settled 2026-08-18, operator): when a child's merge
  collides with an earlier sibling's, the standard move is to SPAWN AN
  AGENT on the conflict — resolve it if trivial, otherwise report why it
  is deeply nontrivial. The resolver's outcome is typed: a resolved merge
  (applied, receipt-stamped) or a conflict report folded as data at the
  branch position for the algebra to decide (drop, re-propose, escalate
  to the operator). Escalation is the algebra's choice, never the default
  path.
- **Two edit channels, by representation (settled 2026-08-18, operator).**
  In-heap state — the working draft — is edited through C4's checked
  `EditPlan s` path (propose → fold-select → `ApprovedEdits` → apply →
  receipts). Anything FILE-shaped — including the companion-memory store,
  which is already a git repo of markdown — is repository content: a
  subagent edits it in its own bound worktree, and that worktree is just
  another branch riding this merge tree. Memory edits therefore need no
  `EditPlan` vocabulary and no second mechanism; "edit memory" = "spawn
  an agent whose worktree branch merges up".
- **Node-row enforcement is by INTERPRETATION, not omission (settled
  2026-08-18, operator).** The model-visible node row carries a narrow
  delegation effect — its `Member` constraints never include `Worktree`
  (or raw `Subagent`), so raw verbs are unnameable at the TYPE level, not
  merely undocumented. A Haskell-side interpreter (freer-simple
  reinterpretation, the same family as `withHandler`'s interposition)
  lowers that effect into the real `Subagent`+`Worktree` row outside the
  model-visible compile. No new wire effect, no new Rust registry row —
  the narrow effect exists only between the model's compile and its
  Haskell interpretation.

## Persistence (v1)

Companion state, folded results, effect/edit receipts, and transcripts are
durable. Live contexts, parked continuations, closures, and partial trees
are NOT checkpointed: a process loss mid-turn reruns the turn from durable
root state. Durable branch resume adopts PRD 20's journal work later, if
real turns make restart cost painful.

## Deferred

Messages to live branches (operator amends a branch's instruction or cancels
a dead-end subtree mid-turn, over the swarm's mailbox substrate — nudges as
reconciliation hints, never rollbacks; wants green-threads select loops;
operator-endorsed for later, after C3 dogfood). A major/compacting OldSpace pass: C2's scope
retirement deregisters a retired binding's GC root but reclaims no tenured
bytes (no such pass exists), so a long-resident session's OldSpace grows
monotonically with total mounts ever made, bounded per turn and reclaimed
only at machine drop — see
[21-c2-scope-trees.md](21-c2-scope-trees.md) §2.2. Emitting
`cache_control` breakpoints (causing provider prefix-cache hits rather than
merely digesting the prefix); no provider impl parses cached-token metrics
today, so measured cache reuse is not verifiable from our side — C2 records
digests and byte counts instead, and C6's friction log should not claim
otherwise. Resumable child
continuations as values; durable context-tree checkpointing;
fresh-vs-forked descendant policies; any universal reasoning ontology;
model-authored drivers; distributed execution; public extraction before N=1
stability.

## Open questions (settle during C0–C2)

1. The smallest snapshot/overlay representation with stable identity and
   correct GC roots (C2's design core).
2. Which `NodeView`/`Contribution` fields earn their keep in daily use — the
   type shrinks freely until persistence depends on it.
3. When live mounting (C1) is worth it per fold — the tripwires are: calling
   a child-supplied function on new values, passing one child's function
   into another's, exhaustively matching an ancestor-declared rich sum, or
   consuming child-local detail through an ancestor-known eliminator.
   Rendered views otherwise.
4. C4 preconditions (path review, 2026-08-19): introduce the
   authority-bearing stage as a DISTINCT sum before implementing it —
   `FoldProduct = Narrative FoldProposal | ProposedEdits (NonEmpty EditPlan)`
   with a consuming `ApprovedEdits` capability from the operator/receipt
   path required before `apply`, so a textual algebra answer can never be
   treated as authorization to mutate. And when persistence expands beyond
   run summaries: the live/durable seed split — a `ContextRef`-carrying
   seed must be untypeable as checkpoint data (`DurableNodeSeed` without
   the capability), so a stale context capability cannot be serialized and
   replayed as durable state.
5. Precondition discovered in C3 (mechanism and policy agreeing from
   independent directions): a node's ALGEBRA cannot receive a value its own
   COALGEBRA minted — `ThoughtF` has no task slot to carry it (Swarm's
   `PlanF` does), relaying through child seeds would launder a capability
   through the fold, and a leaf has no children to read a relay off. Any
   C4/C5 feature wanting algebra access to coalgebra-minted values (refs,
   receipts, node-local capabilities) is a `ThoughtF` change FIRST, then a
   harness change.
6. A root-specific final-proposal/approval stage: root is the only fold
   that has seen the complete synthesis, so forbidding it from proposing new
   edits (rather than only selecting) may be leaving its own best editorial
   judgment unused. Whether that stage is a distinct window, a widened
   `FoldDecision` at root only, or stays out of scope entirely is unsettled.
