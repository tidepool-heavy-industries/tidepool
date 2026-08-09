# PRD — Managed worktrees and typed repository events

**Status:** proposed (2026-08-08)  
**Owner:** self-iterating harness / workspace substrate  
**Depends on:** [PRD 18](18-typed-subagent-spawning-prd.md),
[PRD 14](14-generic-derived-askuser-prd.md), and the
[realm verdict](../post-restart/realm-verdict.md)  
**Executable design target:**
[dev-tree/Harness.hs](../../harness-dogfooding/dev-tree/Harness.hs)

## Summary

Tidepool should start managing isolated Git worktrees and observing repository
facts so typed headless agents can work safely on one codebase. This is a small
substrate, not a built-in Git workflow engine.

The runtime creates retained worktrees, assigns them to agents, records Git
truth, and exposes typed events. An authored resident decides what to do with
those facts:

```haskell
let pokeChildren oid =
      for_ childAgents $ \child ->
        pokeAgent child (whenSafe (RebaseWhenSafe
          { upstreamNode = renderWorktreeId (worktreeId parentTree)
          , upstreamHead = renderOid oid
          }))
in withHandler (headChanged parentTree)
     (pokeChildren . newHead . value) $ do
    current <- worktreeHead parentTree
    when (current /= checkpointedParentHead st) (pokeChildren current)
    parent <- spawnAgent parentSpec parentTask
    observeUntilQuiescent parent
```

The child is not re-based by a magical Haskell operation. It receives a typed
poke, finds a safe stopping point, makes a WIP commit if useful, performs its
own native Git rebase and conflict resolution, and emits new repository facts.
Likewise, an integration agent performs merges in its own worktree created
from the parent branch (agents are isolated — one worktree per agent);
Tidepool observes the resulting `HEAD` move rather than trusting an LLM
summary.

The first rich dogfood is an ordinary recursive unfold/fold:

```text
unfold: allocate worktree tree -> spawn coding agents -> poke descendants
fold:   wait for workers      -> spawn integration agents bottom-up
```

## Product boundary

This follows the per-layer fluency rule from PRD 18:

- Haskell is the compact, typed language for resident orchestration, policy,
  event reactions, and receipts.
- Coding agents retain native edit, shell, test, and Git tools.
- Git observations and process receipts are authority. Agent prose is useful
  context, never proof of a commit, rebase, merge, or clean tree.

Tidepool is building the typed, headless successor to Exomonad's swarm. The
worktree/event seam lets a resident express a recursive development tree and
improve it at conversation cadence; Exomonad remains prior art and migration
input, not a runtime dependency or public API.

## Goals

1. Create isolated, managed Git worktrees from a clean source by default.
2. Offer an explicit, lossless opt-in path for dirty source state.
3. Retain managed worktrees indefinitely in v1; losing work is worse than
   accumulating it.
4. Couple agent creation to worktree allocation: a managed worktree is
   PRD 18's `Workspace`, and — once this PRD lands — the only workspace an
   agent can receive. One worktree per agent; all agents isolated.
5. Expose typed `commit` and `headChanged` sources whose handlers execute in
   the surrounding resident effect row.
6. Make handler lifetime lexical, cleanup reliable, and prohibit implicit
   replay.
7. Support the recursive development-tree dogfood without adding Git workflow
   verbs or a graph DSL to the runtime.
8. Orchestrate a human-guided migration/review against Exomonad's relevant
   worktree, hook, watcher, and lifecycle technology, recording which
   reliability properties Tidepool reimplements natively.

## Non-goals

- A built-in dev-tree scheduler, OODA framework, hylo operator, or graph DSL.
- Public `rebase`, `merge`, `cherryPick`, conflict-resolution, or
  branch-promotion effects.
- Automatic deletion, garbage collection, or retention policy for worktrees.
- Automatically merging to the canonical branch.
- Importing Exomonad's process topology or public types into Tidepool.
- Editing the user's normal `.git/hooks` directory.
- Replaying historical events to a newly registered handler.
- Persisting Haskell closures, event subscriptions, or raw agent handles across
  a resident cycle.

## Locked decisions

### Agent and worktree creation are coupled (revised: Inanna, 2026-08-08)

An earlier draft made `Agent` and `Worktree` fully separate resources
composed by the resident, which required a writer-lease mechanism with an
unspecified enforcement point. Revised: **agent creation is tightly
coupled to worktree allocation — one worktree per agent, every agent
isolated.** Spawning a worker allocates (or is handed, atomically at
spawn) its OWN managed worktree; once this PRD lands, a managed worktree
is the only workspace an agent can receive. `AgentSpec` keeps its
Servant-inspired `mode` parameter for tool interpretation; the worktree
is part of the spawn, not an agent kind.

```haskell
data WorkerRun input result = WorkerRun
  { agent    :: AgentHandle input result
  , worktree :: WorktreeHandle
  }
```

`WorkerRun` is the natural result shape of a coupled spawn rather than a
resident-assembled composition.

Consequences of isolation:

- The writer-lease problem dissolves structurally. At most one agent is
  ever bound to a worktree at a time; binding a second fails explicitly.
  Rebinding a retained worktree to a replacement agent is permitted only
  after the previous agent is terminal or released.
- Reviewers are isolated like everyone else: a reviewer of a child's work
  gets its own worktree created `fromWorktree` off the child's branch —
  no shared-directory coexistence, no read-only lease machinery.
  `readOnlyOf` survives only if a concrete need appears that isolation
  cannot serve.

### Retain first; garbage-collect later

Every created worktree receives a stable `WorktreeId` and durable registry
record. V1 never automatically removes a managed worktree, branch, synthetic
snapshot ref, or diagnostic receipt. A restart must leave the tree
discoverable by ID. Manual deletion becomes `WorktreeLost`; it is never
silently recreated.

Registry and worktree paths live outside the source working tree, so Tidepool
does not dirty the repository it manages. Managed branch/ref names use a
Tidepool-owned namespace and opaque identity.

### Clean source by default; explicit dirty snapshots

Creating from the current repository or another worktree defaults to
`RequireClean`. A dirty source returns a case-matchable error:

```haskell
data WorktreeError
  = SourceDirty DirtySummary
  | NotARepository FilePath
  | WorktreeLost WorktreeId
  | DirtySubmoduleUnsupported FilePath
  | GitFailure GitFailureReceipt
```

The authored escape hatch is explicit:

```haskell
createWorktree (allowDirtySnapshot (fromCurrentRepository "dev-tree/root"))
```

`allowDirtySnapshot` writes a hidden synthetic commit through a temporary Git
index. It must not alter the user's branch, `HEAD`, ordinary index, staged
state, or working-tree bytes. It includes tracked staged/unstaged content plus
non-ignored untracked files; ignored files are excluded. The receipt records
source `HEAD`, selected paths, and pre-snapshot status. Dirty submodules fail
loudly in v1, and so does a source with an in-progress merge, rebase, or
cherry-pick (`MERGE_HEAD`/`REBASE_HEAD` present) — a synthetic commit of a
half-merged tree is a reproducible base for the wrong program.

The synthetic commit is a reproducible base, not a claim that the user made a
commit. Child work uses a Tidepool branch rooted at it; later integration cares
only about the child's delta above that base.

### Repository facts are typed event sources

The shared abstraction is an event description, not a callback registry:

```haskell
data Event a

data Observed a = Observed
  { eventId :: EventId
  , value   :: a
  }

commit      :: WorktreeHandle -> Event (Observed CommitReceipt)
headChanged :: WorktreeHandle -> Event (Observed HeadChangeReceipt)

withHandler
  :: Event a
  -> (a -> M effs ())
  -> M effs b
  -> M effs b
```

`withHandler event handler body` atomically registers, runs `body`, and
unregisters when its lexical body completes or fails. Registration itself does
not block. The closure runs in the same `M effs` environment, so it may send a
typed agent message, spawn a reviewer, ask the operator, or record a receipt.

For same-typed alternatives, `Tidepool.Event` exports a deliberately small
union operator:

```haskell
(<|>) :: Event a -> Event a -> Event a
```

It means “observations from either source, merged into one subscription”
(subscriptions repeat for their lexical lifetime; this is not one-shot); it
need not fake a general `Applicative` instance. Mapped sum events keep
heterogeneous selection typed.

### Handler semantics are structured concurrency

- A subscription begins at registration and never replays older journal rows.
- Events broadcast to all registered handlers; they are not globally consumed.
- Each subscription invokes one handler at a time; later matches queue in
  observation order.
- Separate handlers interleave only at realm suspension points.
- When the body ends, intake closes, already-observed events plus an in-flight
  handler drain, then the subscription unregisters.
- Handler failure fails the enclosing scope and triggers normal structured
  cleanup. It is never logged-and-forgotten.
- Queue overflow, source loss, or inability to drain before runtime deadline
  fails loudly; commits are never silently dropped.

Closures live only in the current realm. A later resident cycle re-registers
reactions from explicit `State` and stable worktree IDs. Because registration
does not replay, the first action inside the newly registered `withHandler`
body compares `worktreeHead tree` with the last head stored in its checkpoint.
Registration is active before that read: a movement before registration is
found by reconciliation, while a movement after registration is queued for the
handler. The resident deduplicates by observed head/EventId if both paths see
the same movement. This closes the between-cycle race without turning the
journal into implicit callback replay.

**Design stance (Inanna, 2026-08-08):** this surface is designed as an
ideal DSL first — the vocabulary a fluent Haskell author would naturally
write (`withHandler`, `Event`, `<|>`, ordinary closures in the ambient
effect row) — and the runtime is made to serve it. Runtime machinery is
never part of the authored contract and never shapes the vocabulary.

The authored semantics above are complete in themselves: handlers run in
the surrounding effect row, may themselves suspend (spawn a reviewer, ask
the operator), run one-at-a-time per subscription with later observations
queued in order, and live exactly as long as their lexical scope. V1 has
a configured per-subscription queue bound whose overflow fails the scope
loudly; the bound's value is tunable, its existence is not.

*Implementation note (not authored surface):* an invocation is realized
as an ordinary parked continuation in the surrounding realm under the
frozen parking contract
(`../post-restart/realm-lanes/continuation-parking-contract.md`) — exact
handled-prefix equality derived from the row that built the handler
stack (never re-declared at the dispatch site), cycle-scoped lifetime,
driver-chosen resume order at suspension points. These are constraints
on the implementation; if the runtime cannot meet the authored semantics
within them, the runtime work grows — the DSL does not shrink.

### `commit` and `headChanged` serve different jobs

`commit tree` is the high-signal semantic checkpoint: normal commit, merge,
cherry-pick, or amend observed in that worktree. It is for review, test, and
receipt reactions.

`headChanged tree` reports observed movement of the worktree's current
`HEAD`: normal advance, amend, rebase/rewrite, reset, or checkout. It is the
dependency-propagation signal: children should receive a rebase poke even when
their parent was itself rebased.

Observations are coalesced state deltas, not a complete movement log: a
polling observer that finds `HEAD` at C after last seeing A reports one
transition, even if the tree passed through B in between, and
classification degrades honestly to `UnknownChange` when the intermediate
history is not recoverable. No consumer may treat the stream as exhaustive
history; the dependency-propagation job needs only latest-state semantics,
which coalescing preserves.

```haskell
data HeadChangeKind
  = Advanced [GitOid]
  | Amended GitOid GitOid
  | Rewritten [(GitOid, GitOid)]
  | Rewound
  | Switched
  | UnknownChange
```

A normal commit produces both observations with one underlying `EventId`.
`EventId` is opaque runtime identity; Git OIDs remain domain data. The initial
polling implementation must be conservative: it reliably emits `headChanged`
for a tip movement and `commit` only when it can honestly infer one. It must
never invent LLM/agent causal attribution.

### Hooks wake; reconciliation decides

The source of truth is reconciled Git inspection, never raw hook payload,
filesystem notification, or an agent's command transcript.

V1 starts with polling and reconciliation after observed coding-agent command
activity. A later managed-worktree hook adapter sends a local wake-up with
worktree identity and optional hints; Tidepool then reads Git state, assigns an
`EventId`, journals the result, and dispatches subscribers. The adapter must be
scoped to a Tidepool-owned hook path or managed agent environment; it may not
overwrite the user's `.git/hooks`. Polling remains the fallback for missed
hooks, external writers, and restart recovery.

### Git operations remain agent work

The public surface has creation, workspace lookup, inspection, and events. It
has no `rebaseOnto`, `merge`, or conflict-resolution operation. Recursive
behavior is authored with typed messages:

```haskell
data DevMessage
  = RebaseWhenSafe { upstreamNode :: Text, upstreamHead :: Text }
  | FinishAndCommit { finishReason :: Text }
  deriving (Generic)
```

The child decides how to make itself safe and uses native Git. If it is
finished or unsuitable, the resident may send an interrupting finish/handoff
poke or spawn a replacement agent in the retained worktree after writer
handoff.

## Public surface

The exact row spelling follows existing Tidepool effects; the intended small
vocabulary is:

```haskell
data WorktreeSpec
data WorktreeHandle
data WorktreeId
data BranchName

fromCurrentRepository :: Text -> WorktreeSpec
fromRef               :: GitRef -> Text -> WorktreeSpec
fromWorktree          :: WorktreeHandle -> Text -> WorktreeSpec
allowDirtySnapshot    :: WorktreeSpec -> WorktreeSpec

createWorktree
  :: WorktreeSpec
  -> M effs (Either WorktreeError WorktreeHandle)

workspaceOf    :: WorktreeHandle -> Workspace
worktreeBranch :: WorktreeHandle -> M effs BranchName
worktreeHead   :: WorktreeHandle -> M effs GitOid
worktreeId     :: WorktreeHandle -> WorktreeId

lookupWorktree :: WorktreeId -> M effs (Either WorktreeError WorktreeHandle)
listWorktrees  :: M effs [WorktreeSummary]
```

There is deliberately no `releaseWorktree` or `deleteWorktree` in v1.

## Receipts and persistence

At minimum, durable registry data records:

```haskell
data WorktreeReceipt = WorktreeReceipt
  { worktreeId  :: WorktreeId
  , cwd         :: FilePath
  , branch      :: BranchName
  , sourceHead  :: GitOid
  , snapshotRef :: Maybe GitRef
  , createdAt   :: Timestamp
  }
```

Every repository event records source, reconciliation result, timestamp, and
`EventId`. Agent receipts retain command/diff activity from PRD 18. They are
complementary: agent receipts say what the harness observed the worker doing;
worktree receipts say what Git actually became. The persistent event journal is
for traceability and restart diagnosis, not implicit callback replay.

## Exomonad migration/review, under human guidance

This is an explicit implementation lane. The endgame is replacement: the Exo
swarm is hosted on the dev-tree resident, whose headless agents communicate
through PRD 18 rather than Exomonad's interactive-session MCP sidecar. Tidepool
does not import Exomonad public types, process topology, hooksock, inbox reader,
or a shared runtime crate.

A Tidepool maintainer and an Exomonad-aware human/agent still perform a
property-by-property review before implementation. This preserves the hard-won
failure semantics while implementing them against Tidepool's realm, registry,
and receipt model rather than copying mechanisms designed for tmux/interactive
Claude sessions.

The review starts from concrete precedent:

- Exomonad refuses worktree spawning from a dirty tree because children would
  miss uncommitted state (`rust/exo/src/tools/spawn.rs`). Tidepool retains that
  safe default while adding the explicit snapshot escape hatch.
- `exo-node` has a runtime-owned Unix-domain hook RPC with bounded payload,
  timeout, local permissions, and a thin client/server split
  (`rust/exo-node/src/hooksock/`). It is prior art for Tidepool's native hook
  wake-up adapter, not an existing Git-hook implementation to port.
- Exomonad's inbox reader combines `notify` wakeups, a periodic backstop,
  durable cursors, no replay for a fresh reader, and advance-after-success
  delivery (`rust/exo-node/src/inbound.rs`). Tidepool should reproduce these
  reliability properties in its own realm and receipt model.
- Exomonad deliberately preserves worktrees after abnormal teardown for
  post-mortem inspection. This validates Tidepool's retain-first v1 stance.

The lane produces a human-reviewed decision record containing:

1. exact Exomonad modules/contracts considered;
2. the reliability properties retained and mechanisms rejected;
3. the Tidepool-native ownership and versioning boundary;
4. Tidepool-local tests/receipts for each retained failure behavior; and
5. confirmation that no Exomonad tmux, Claude-only, MCP-sidecar, or global-process
   assumption leaks into Tidepool's public Haskell surface.

## Dogfood: recursive development tree

[dev-tree/Harness.hs](../../harness-dogfooding/dev-tree/Harness.hs) is the
acceptance-shaped example. It proves an ordinary recursive resident can:

1. create a root worktree, then recursively allocate children from parent HEADs;
2. register propagation handlers before starting each parent worker;
3. spawn coding workers asynchronously;
4. turn parent head movement into `RebaseWhenSafe` pokes;
5. wait for results while handlers remain live; and
6. spawn fresh integration agents bottom-up to merge finished child branches.

No runtime primitive knows what a rebase, merge, development tree, or
integration policy is. The only hard runtime behavior is single-writer
assignment and accurate repository observation.

Poke semantics are PRD 18's tagged `pokeAgent`, whose revision is authoritative.
A poke is accepted into a durable per-agent FIFO, remains queued until
deliverable, and is never silently discarded. Repository propagation normally
uses `whenSafe`: `RebaseWhenSafe` asks the child to choose a coherent stopping
point without discarding its current turn. Escalation may use `interrupting`
with a typed `FinishAndCommit` or handoff message; Tidepool then cancels the
active turn and delivers that message as the next turn. There are no separate
authored steer, follow-up, or interrupt operations.

What stays authored policy in the resident is reaction: when to escalate, what
message to send, and whether to wait, replace, integrate, or stop. Delivery,
turn interruption, durable ordering, and idle follow-up mechanics belong to
the runtime.

For example, the resident may answer a first `AgentWentIdle` with
`whenSafe (FinishAndCommit ...)`, record that escalation in checkpointed
`State`, and answer a later idle/deadline observation with
`interrupting (FinishAndCommit ...)`. The runtime performs both requests; the
resident chooses the escalation schedule and handles the resulting typed
lifecycle events.

**Cycle shape (updated to PRD 18's revised summary, 2026-08-08):** agents
may CONTINUE RUNNING between resident cycles — their stable identities and
the resident's plan for them are ordinary checkpointed data. What still
never crosses a cycle boundary: an attached Haskell handle, a parked
Haskell continuation, or an event subscription. So the dev-tree unfold/
fold can span cycles: each cycle re-registers its `withHandler` reactions
from explicit `State` and stable worktree IDs, re-attaches to running workers by
checkpointed identity, then reconciles `worktreeHead` inside the registered
scope before proceeding. No-replay handler semantics therefore cannot hide a
between-cycle move. A crash loses only the orchestration decisions since the
last checkpoint; worktrees, branches, receipts, queued pokes, and still-running
agent threads all survive and are re-discoverable by ID.

## Implementation plan

### 1. Narrow vertical core

Add `Worktree`, `Event`, errors, registry/receipt types, and the `withHandler`
interpreter. Land:

- clean current-repository worktree creation;
- stable runtime-owned worktree path, branch, and registry record;
- coupled agent binding (one worktree per agent; second binding fails
  explicitly, rebinding only after the previous agent is terminal/released);
- `headChanged` polling/reconciliation; and
- one handler closure executing in its parent's effect row while an agent runs.

The first acceptance test uses a temporary repository, one coding worker, one
managed worktree, and a real commit/HEAD transition.

### 2. Parallel follow-on lanes

- **Dirty snapshot:** alternate-index synthetic commit; prove source branch,
  index, staged/unstaged bytes, and ignored files remain correct.
- **Event monitor:** command-triggered reconciliation, poll/backstop, identity,
  ordering, `commit` classification, and no-replay subscriptions.
- **Handler/realm:** repeated callbacks, suspension, queue/drain/failure rules,
  and bounded overflow behavior.
- **Durability:** restart lookup, loss reporting, and never-delete policy.
- **Exomonad migration/review:** execute the human-guided property review,
  implement the selected guarantees Tidepool-natively, and retain comparative
  receipts while moving Exo workflows onto dev-tree.
- **Dogfood:** make `dev-tree/` compile and exercise parent poke -> native
  rebase -> child head event -> bottom-up LLM merge in a disposable repository.

### 3. Converge

Converge only after receipt suites exist. Keep the public vocabulary small:
creation/specification, lookup, workspace conversion, `commit`, `headChanged`,
`worktreeHead`, and `withHandler`. Keep choreography in residents and ordinary
libraries.

## Acceptance criteria

1. A clean source creates an isolated worktree/branch without changing the
   original worktree or user branch.
2. Dirty source returns `Left (SourceDirty summary)` by default.
3. `allowDirtySnapshot` gives a child exact tracked and non-ignored untracked
   content without changing source `HEAD`, index, or bytes.
4. Worktrees, branches, registry records, and receipts survive restart and are
   never automatically deleted.
5. Binding an agent to an already-bound worktree fails explicitly;
   rebinding succeeds only after the previous agent is terminal or
   released. Reviewers run isolated in their own worktrees.
6. `withHandler` runs in the surrounding effect row, cleans up lexically,
   drains already-observed events, and never replays pre-registration events;
   register-then-`worktreeHead` reconciliation catches between-cycle movement.
7. A normal native commit yields reconciled `commit` and `headChanged` facts
   sharing an `EventId`; a rebase at least yields an honest `headChanged` fact.
8. Polling remains correct without hooks; a hook adapter is only a wake-up.
9. `dev-tree/Harness.hs` typechecks and proves parent-to-child `whenSafe`
   rebase pokes, typed interrupting escalation, and bottom-up LLM-led
   integration in a disposable repository.
10. The human-guided Exomonad migration/review record is complete and each
    retained reliability property has Tidepool-local tests/receipts.

## Deferred questions

1. Exact Git hook protocol, environment-scoped `core.hooksPath` strategy,
   authentication token, and timeout.
2. Richer events: dirty/clean, conflicts, checks, branch movement, and external
   file changes. Add only when a real resident needs them.
3. GC/archive/delete interface and retention budget for durable agents and
   worktrees, once measurement justifies one.

## Addendum — decisions locked pre-flight (Inanna + root, 2026-08-09)

3. **Atomic coupled spawn is the transaction.** One authored call —
   `run <- spawnAgent spec task` — yields `WorkerRun { agent, worktree }`
   with no observable state where one exists without the other. Any
   partial failure (allocation, binding, backend acceptance) surfaces as
   ONE typed error with no half-state; a durable saga state machine
   (Allocating → WorktreeReady → Bound → ThreadAccepted → Running, plus
   failed/orphaned + restart reconciliation) lives entirely BEHIND the
   call. Retain-first makes reconciliation cheap: every crash state is
   re-discoverable by ID; recovery is rebind-or-mark-lost, never cleanup.
4. **Coupled-only public surface.** `createWorktree` leaves the public
   vocabulary (internal plumbing only). `spawnAgent` accepts either a
   `WorktreeSpec` (allocate + bind) or an existing UNBOUND worktree
   handle (bind — the retained-worktree rebind rule already stated
   above), which covers replacement and integration agents. Revisit only
   if a real resident needs an agent-less worktree.
