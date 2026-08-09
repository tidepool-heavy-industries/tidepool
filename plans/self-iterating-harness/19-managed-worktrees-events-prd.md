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
withHandler (headChanged parentTree) (\change ->
  for_ childAgents $ \child ->
    sendMessage child (RebaseWhenSafe (newHead change))
  ) $ do
    parent <- spawnAgent parentSpec parentTask
    waitAgent parent
```

The child is not re-based by a magical Haskell operation. It receives a typed
poke, finds a safe stopping point, makes a WIP commit if useful, performs its
own native Git rebase and conflict resolution, and emits new repository facts.
Likewise, an integration agent performs merges in a parent worktree; Tidepool
observes the resulting `HEAD` move rather than trusting an LLM summary.

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

Tidepool is not replacing Exomonad's whole swarm. It is building the typed
worktree/event seam through which a Tidepool resident can express a recursive
development tree and improve it at conversation cadence.

## Goals

1. Create isolated, managed Git worktrees from a clean source by default.
2. Offer an explicit, lossless opt-in path for dirty source state.
3. Retain managed worktrees indefinitely in v1; losing work is worse than
   accumulating it.
4. Make a managed worktree usable as PRD 18's caller-assigned `Workspace`.
5. Expose typed `commit` and `headChanged` sources whose handlers execute in
   the surrounding resident effect row.
6. Make handler lifetime lexical, cleanup reliable, and replay opt-in rather
   than accidental.
7. Support the recursive development-tree dogfood without adding Git workflow
   verbs or a graph DSL to the runtime.
8. Orchestrate a human-guided integration with Exomonad's relevant worktree,
   sidecar, hook, watcher, and lifecycle technology.

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

### Worktree is a separate resource effect

`Agent` and `Worktree` compose but own neither other's lifecycle. `AgentSpec`
keeps its Servant-inspired `mode` parameter for tool interpretation; workspace
topology is a value-level resource, not an agent kind.

```haskell
data WorkerRun input result = WorkerRun
  { agent    :: AgentHandle input result
  , worktree :: WorktreeHandle
  }
```

`WorkerRun` is an ordinary resident/library convenience record. Common modes
are therefore ordinary values: a read-only observer in an existing workspace,
or one read-write coding worker in a managed worktree.

The runtime rejects two active `WorkspaceWrite` turns for one managed
worktree. Read-only reviewers may coexist. A resident hands a worktree to a
replacement worker only after ending or releasing the current writer.

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
reactions from explicit `State` and stable worktree IDs.

**Dispatch mechanism (binding):** a handler invocation is an ordinary
parked continuation in the surrounding realm, consumed through the frozen
seam contract (`../post-restart/realm-lanes/SEAM.md`): exact handled-prefix
equality with the surrounding row (derived from the row that built the
handler stack, never re-declared at the dispatch site), cycle-scoped
lifetime, driver-chosen resume order at suspension points. A handler that
suspends (spawning a reviewer, asking the operator) parks like any other
continuation and blocks its own subscription's queue by design. V1 must
have a configured per-subscription queue bound whose overflow fails the
scope loudly; the bound's value is tunable, its existence is not.

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
finished or unsuitable, the resident may follow up or spawn a replacement
agent in the retained worktree after writer handoff.

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
readOnlyOf     :: WorktreeHandle -> Workspace
worktreeBranch :: WorktreeHandle -> M effs BranchName
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

## Exomonad integration, under human guidance

This is an explicit implementation lane. A Tidepool maintainer and an
Exomonad-aware human/agent jointly map useful existing components and choose
whether to extract a shared crate, adapt a proven pattern with tests, or retain
separate implementations.

The review starts from concrete precedent:

- Exomonad refuses worktree spawning from a dirty tree because children would
  miss uncommitted state (`rust/exo/src/tools/spawn.rs`). Tidepool retains that
  safe default while adding the explicit snapshot escape hatch.
- `exo-node` has a runtime-owned Unix-domain hook RPC with bounded payload,
  timeout, local permissions, and a thin client/server split
  (`rust/exo-node/src/hooksock/`). It is a strong shape for Tidepool's later
  hook wake-up adapter, not an existing Git-hook implementation.
- Exomonad's inbox reader combines `notify` wakeups, a periodic backstop,
  durable cursors, no replay for a fresh reader, and advance-after-success
  delivery (`rust/exo-node/src/inbound.rs`). Tidepool should adopt these
  reliability properties, adapted to its realm and receipt model.
- Exomonad deliberately preserves worktrees after abnormal teardown for
  post-mortem inspection. This validates Tidepool's retain-first v1 stance.

The lane produces a human-reviewed decision record containing:

1. exact Exomonad modules/contracts considered;
2. what Tidepool reuses, extracts, adapts, or rejects;
3. ownership and versioning boundary;
4. Tidepool-local tests/receipts for adopted failure behavior; and
5. confirmation that no Exomonad tmux, Claude-only, or global-process
   assumption leaks into Tidepool's public Haskell surface.

Exomonad remains usable as an external swarm/process sidecar. Tidepool's
authored Agent API remains provider-neutral. Future MCP/message integration is
possible, but not a prerequisite for this local substrate.

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

Pokes are fire-and-forget (decision: Inanna, 2026-08-08). A `sendMessage`
poke to an agent with no steerable turn surfaces PRD 18's typed runtime
error to the SENDING resident's handler — loud, not lost — and any retry,
`followupTask`, or replacement policy is authored code in the resident.
There is no runtime delivery queue and no auto-enqueue on idle agents;
PRD 18's message semantics stand unmodified.

**V1 cycle shape (consequence of decided substrate, stated honestly):**
the entire unfold/fold runs within ONE resident cycle, because durable
agent handles across cycles are deferred (deferred question 2 here; PRD
18 open decision 4) and PRD 18's v1 driver requires agent quiescence at a
cycle boundary. The trade: no mid-tree checkpoint — a crash loses
orchestration state back to the last checkpoint, while every worktree,
branch, and receipt survives by ID for post-mortem and manual restart.
Accepted for v1; revisit with durable agent identities.

## Implementation plan

### 1. Narrow vertical core

Add `Worktree`, `Event`, errors, registry/receipt types, and the `withHandler`
interpreter. Land:

- clean current-repository worktree creation;
- stable runtime-owned worktree path, branch, and registry record;
- workspace assignment with writer lease;
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
- **Exomonad:** execute the human-guided decision record, prototype/extract the
  chosen UDS/watcher seam if warranted, and retain comparative receipts.
- **Dogfood:** make `dev-tree/` compile and exercise parent poke -> native
  rebase -> child head event -> bottom-up LLM merge in a disposable repository.

### 3. Converge

Converge only after receipt suites exist. Keep the public vocabulary small:
creation/specification, lookup, workspace conversion, `commit`, `headChanged`,
and `withHandler`. Keep choreography in residents and ordinary libraries.

## Acceptance criteria

1. A clean source creates an isolated worktree/branch without changing the
   original worktree or user branch.
2. Dirty source returns `Left (SourceDirty summary)` by default.
3. `allowDirtySnapshot` gives a child exact tracked and non-ignored untracked
   content without changing source `HEAD`, index, or bytes.
4. Worktrees, branches, registry records, and receipts survive restart and are
   never automatically deleted.
5. A second active writer fails explicitly; a read-only reviewer may coexist.
6. `withHandler` runs in the surrounding effect row, cleans up lexically,
   drains already-observed events, and never replays pre-registration events.
7. A normal native commit yields reconciled `commit` and `headChanged` facts
   sharing an `EventId`; a rebase at least yields an honest `headChanged` fact.
8. Polling remains correct without hooks; a hook adapter is only a wake-up.
9. `dev-tree/Harness.hs` typechecks and proves parent-to-child typed rebase
   pokes plus bottom-up LLM-led integration in a disposable repository.
10. The human-guided Exomonad integration decision record is complete and its
    adopted behavior has Tidepool-local tests/receipts.

## Deferred questions

1. GC/archive/delete interface and retention budget.
2. Durable Agent handles across resident cycles, distinct from durable
   worktree IDs.
3. Exact Git hook protocol, environment-scoped `core.hooksPath` strategy,
   authentication token, and timeout.
4. Richer events: dirty/clean, conflicts, checks, branch movement, and external
   file changes. Add only when a real resident needs them.
5. Cross-process Exomonad orchestration beyond this reviewed integration lane.
