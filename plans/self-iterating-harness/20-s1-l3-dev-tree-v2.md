# PRD 20 S1-L3 — dev-tree v2: `runNode` factored into a hylomorphism

Refactor the dev-tree dogfood harness (`harness-dogfooding/dev-tree/`) onto the
landed PRD 20 substrate: `Tidepool.Swarm`'s `PlanF`/`hyloM`, `Tidepool.Journal`'s
`record`, the typed async spawn surface (`spawnAsync`/`awaitAgent`/`cancelAgent`),
and the `Exec` tier the widened driver row now serves.

Design authority: `plans/self-iterating-harness/20-exomonad-v3-prd.md`. This
file is the lane's implementation contract; the PRD is the decision authority
and wins on any conflict.

**Out of scope, deliberately.** Node residency (`forkNode`/`sendDown`/`sendUp`,
the resident select loop) and green threads (`forkM`/`Promise`/`withForks`) are
S1-L4, on a sibling branch. This lane leaves a NAMED seam where they land (see
"The residency seam") and builds nothing behind it.

The harness both WRITES the journal and READS the fold of it — see "Resume"
below. `record` itself stays write-only by construction (`Tidepool.Journal`
exposes no read verb, and nothing in the harness opens a file); the fold
arrives already folded, from the driver, through the opt-in `resumeLoop` entry
point (PRD 20 S1-L5).

---

## v1's `runNode` is already a hand-rolled hylomorphism

`harness-dogfooding/dev-tree/Harness.hs` (v1) reads:

```haskell
runNode tree p =
  withHandler (headChanged tree) (noteHeadMove p) (spawnWorker tree p) >>= \case
    Left err     -> pure (Left (NodeSpawnFailed (nodeName p) err))
    Right implResult ->
      runChildren tree (childPlans p) >>= \case
        Left err       -> pure (Left err)
        Right children -> integrateNode tree p implResult children
```

Three phases in strict order, and each one is a named piece of the scheme:

| v1 phase | what it is | v2 home |
|---|---|---|
| `spawnWorker tree p` under `withHandler (headChanged tree)` | the node's own worker runs to completion FIRST, so children seed from a final parent HEAD | **coalgebra** — the parent-first scaffold ladder |
| `runChildren tree (childPlans p)` — `createWorktree (fromWorktree parentTree …)` then recurse | unfold: one seed per child, each carrying its own worktree | **coalgebra** — emitting `PlanF task childSeeds` |
| `integrateNode tree p implResult children` | fold: the merge agent over the completed child branches | **algebra** |
| `runNode`'s own recursion | `traverse go` | `hyloM` |
| `IntegratedNode` (the materialized result tree) | — | **deleted.** The fused hylo never materializes a tree; `Outcome` is what a node folds to, and what persists is git plus the run journal |
| `Either NodeError _` short-circuiting through `runChildren` | first failure aborts the remaining siblings | **deleted.** Failure is data: `Outcome` carries `Failed`, `traverse` visits every sibling by construction |

So the refactor is not a rewrite into an unfamiliar shape — it is naming the
shape that is already there, and then getting four things the hand-rolled
version could not have: lazy layer-by-layer decomposition, failure that
accumulates instead of short-circuiting, policy as middleware over two
function seams, and a recursive step that S1-L4 can replace wholesale.

---

## The v2 shape

```haskell
-- The seed: everything a node needs to be unfolded.
data NodeSeed = NodeSeed
  { seedPlan   :: DevPlan          -- authored subtree (v1's DevPlan, extended)
  , seedTree   :: WorktreeHandle   -- this node's worktree — it OWNS it
  , seedDepth  :: Int              -- for `capped`
  , seedCycles :: Int              -- agent-cycle allowance for THIS SUBTREE
  }

-- The task: what the coalgebra decided, handed to the algebra unchanged.
data NodeWork = NodeWork { workSeed :: NodeSeed, workScaffold :: Maybe Outcome, workRefusal :: Maybe Failure }

decompose :: NodeSeed -> Harness (PlanF NodeWork NodeSeed)   -- Coalg
integrate :: PlanF NodeWork [Outcome] -> Harness Outcome     -- Alg  (via PlanF NodeWork Outcome)

runTree :: NodeSeed -> Harness Outcome
runTree = Swarm.hyloM integrate decompose
```

`Tidepool.Swarm` is imported QUALIFIED. This repo's own
`.tidepool/lib/Schemes.hs` defines an unrelated `hyloM` and is auto-imported
unqualified into every eval run from this worktree; an unqualified
`import Tidepool.Swarm` is ambiguous there. (Same rule
`works_swarm_planf_hylo_on_jit` follows.)

### The coalgebra — how to split

1. **Depth/budget guards run first** (wave 2's `capped`/`budgeted` middleware).
   A refusal does not throw and does not return an outcome — a coalgebra
   cannot produce one. It TRUNCATES the node to a childless `PlanF` whose task
   carries `workRefusal`, and the algebra reads that as ordinary data. This is
   the only shape a coalgebra-side veto can have, and it is why the wrappers
   are `Coalg -> Coalg` rather than exceptions.
2. **The scaffold worker**, for a node that has children: `spawnAgent
   @WorkerResult` in the node's OWN worktree, under `withHandler (headChanged
   tree)`. This is v1's `spawnWorker`, unmoved, and it is why children seed
   from a parent HEAD that is already final. A LEAF spawns nothing here — its
   implementation is the algebra's job, because for a leaf "how to combine
   nothing" IS "implement it" (PRD: *integrate — leaf implementation, or the
   merge agent plus checks*).
3. **`record "split" branch …`** — decomposition is cognition, so it is
   journaled, never re-derived. A resumed run replays it (see "Resume").
4. **Child worktrees**: `createWorktree (fromWorktree parentTree name)` per
   child plan, from the scaffold HEAD. A worktree failure is not a split
   failure — that child's seed is dropped and the failure is carried in the
   parent's task so the algebra can fold it as `Failed`.
5. **The layer gate** (wave 2's `gated`) runs LAST, on the produced `PlanF` —
   the operator approves a layer with its parent's real scaffold outcome
   attached, never a speculative whole-tree sign-off.

Decomposition is lazy by construction: a child's `decompose` runs only after
its parent's `decompose` returned, which is after the parent's scaffold worker
finished. v2's evaluation order is v1's "wave boundary is where understanding
accumulates" — as evaluation order rather than as TL discipline.

### The algebra — how to combine

`PlanF NodeWork [Outcome]` in plan order (never completion order).

**Leaf** (`kids == []`, no refusal):

1. `worktreeHead` before, `spawnAgent @WorkerResult` under `withHandler
   (headChanged tree)`, `worktreeHead` after — rung 1, repository observation.
   A worker that claims completion without moving HEAD is caught HERE and
   never reaches rung 2.
2. Rung 2 — `runIn cwd` each of the plan's `nodeChecks` at the actual fold sha.
3. Boundary — `git diff --name-only <seed>..HEAD` in the worktree, matched
   against `nodeBoundary` (exact or directory-prefix). Out-of-boundary paths
   are named in the receipt and fail the fold.
4. Stamp `FoldReceipt`, `record "outcome" branch (toJSON receipt)`.

**Interior** (`kids` non-empty):

1. **The eager rebase cascade** (below) brings every child tip onto this
   node's current HEAD before anything merges.
2. The integration agent — `spawnAgent @WorkerResult` in this node's worktree,
   handed the child branch names AND the escalations, told never to discard a
   child's work.
3. Rungs 1–3 exactly as the leaf case, at the integration sha.
4. Stamp, record.

Rung 3 (adversarial review) is NOT in this lane. The receipt has the slot and
the ladder's shape is `reviewLadder` in the PRD's public surface; wiring a
reviewer spawn is the next increment on the same seam.

### The eager rebase cascade

This supersedes v1's "no rebase propagation" note in the module header, and
the reason v1's rationale expired is structural, not stylistic. v1 created a
child worktree only when it was that child's turn to run, so a child was
always seeded from a HEAD that could no longer move. v2's coalgebra creates
EVERY sibling worktree at once (that is what "emit `PlanF task childSeeds`"
means), so the moment the algebra folds sibling *i*, siblings *i+1..n* are
sitting on a stale base. The drift v1 designed around now genuinely exists.

Three tiers, in order, per PRD "Mechanical first, cognition second,
escalation third":

1. **Mechanical.** `runIn childCwd "git rebase <parentHead>"`, `git rebase
   --abort` on any nonzero exit. Clean means done at zero tokens. This is
   authored policy running plain git in a worktree the node owns — PRD 19's
   freeze (no git workflow verbs in the RUNTIME crates) is untouched.
2. **Cognition.** Every child that conflicted gets an ephemeral resolution
   agent, and this is where the typed handles earn their place: all of them
   are `spawnAsync @ResolutionResult`'d at once, then `awaitAgent`'d **in plan
   order**. Completion order is not an input to anything. `cancelAgent` reaps
   the still-in-flight handles when an earlier escalation triggers `Abandon`.
3. **Escalation as data.** An unresolved conflict is not an exception and does
   not stop the fold: it becomes `Failed … (RebaseEscalation …)` in that
   child's outcome, and the parent's failure policy — an ordinary exhaustive
   case — decides. The remaining siblings still fold. `traverse` already
   visited them.

Cascades converge because the task is always "rebase onto the parent's
CURRENT tip": arrival order changes how much work each rebase does, never the
terminal state.

### Budgets without mutable state

PRD puts budgets in `State`, but a whole run happens inside ONE `loop` call
and the row carries no shared-mutable-state effect — by decision, not
omission. So the agent-cycle budget is carried STRUCTURALLY on the seed: a
node reserves its own cycles (scaffold + integration + a resolution
allowance) and divides the remainder among its children. The total across a
run is bounded by the root allowance, the division is deterministic, and
completion order cannot affect it.

This is conservative — a subtree that finishes under its share does not
return the remainder to its siblings. That is the honest v1 mechanism; the
seam for a driver-serviced global counter is the same `budgeted` slot, which
is effectful (`a -> M …`) precisely so it can consult the journal or the
driver later without the wrapper changing shape.

---

## Resume — what a resumed run actually skips

The driver folds this run's journal at boot and injects it into the harness's
opt-in second entry point:

```haskell
resumeLoop :: ResumeFold -> State -> Harness State
loop       = resumeLoop emptyResume
```

One spelling of the run, not two. The resume wrapper `resumed` is the
IDENTITY when `isResumed` is false, so a fresh boot performs exactly the git
reads and spawns it performed before resume existed — the fresh path is not a
special case, it is the empty case.

`resumed` is the OUTERMOST coalgebra wrapper, outside `gated`/`capped`/
`budgeted`. A subtree the journal already accounts for must not be re-gated
(the operator approved that layer), re-capped, or re-budgeted (those cycles
were spent by a process that is gone, and refusing finished work on a budget
would discard it). Work that is genuinely NEW on a resumed run — an amended
subtree, an unfold from an adopted scaffold — goes back through the full
stack.

| the fold says | the resumed run does |
|---|---|
| an `outcome` for this branch | the subtree is not re-entered at all; the recorded receipt IS the algebra's input |
| a `split`, no `outcome` | replay the recorded plan and REBIND the recorded child worktrees; no scaffold worker, no planner |
| a `replan` NEWER than both | re-unfold under the AMENDED plan (`amendPlan` replaces the task and nothing else), or refuse the subtree when `abandonSubtree` |
| nothing, and the worktree's HEAD moved | orphaned work: adopt-and-verify (below) |
| nothing, and the HEAD never moved | ordinary work |

`resumePlanFor` decides all of that from the fold alone — PURE, before any git
runs — and `amendmentIsNewest` is the seq comparison behind the third row.
Both are exported and directly callable, in the spirit of the coalgebra's pure
policy slots.

Resuming a run whose ROOT outcome is recorded does no work: the root's outcome
stands, so the hylo folds it and returns.

### Adopt and verify, never redo blind, never trust blind

A commit found in a retained worktree is neither redone nor trusted. The
orchestrator runs its OWN checks (`runChecks`) and boundary diff
(`boundaryViolations`) in that worktree at that sha, stamps a `FoldReceipt`,
and hands it to `foldLadder` — the same rungs, in the same order, that judge a
fold this process performed. There is no second judge, so there is no path on
which an orphaned commit is trusted because it exists. A rejected verification
rides on as an ordinary `Failed` outcome, where the parent's `OnFailure` policy
already lives.

What the orphaned work MEANS depends on the plan, and that is the one place
resume reads more than the ladder:

- A **leaf**'s commit is its whole fold. Verified ⇒ adopted as the node's
  outcome.
- An **interior node with no recorded split** has an orphaned SCAFFOLD, not an
  orphaned outcome — its children still have to run. Verified ⇒ `decompose`
  unfolds from it (`seedAdopted`) instead of spawning the scaffold worker
  again.
- An **interior node with a recorded split** is adopted only when the
  integration is provably COMPLETE: every child in the recorded plan recorded a
  `Done` outcome and every one of those branches is an ancestor of this node's
  HEAD. A partial integration is replayed instead, which is safe because
  merging an already-merged branch is a no-op.

**Adoption APPENDS.** The journal is append-only; adopting means recording the
outcome the crashed process never got to record, which is what makes the NEXT
resume skip the subtree. A REPLAYED outcome is not re-appended — it is already
there, and re-appending it every resume would be duplicate noise.

### Rebind, never recreate

PRD 19 retains worktrees indefinitely, so on resume a branch the journal names
already HAS one: `rebindWorktree` looks it up by BRANCH (its durable identity —
a managed branch name carries the worktree id) through `listWorktrees` +
`lookupWorktree`, and `spawnSpecIn` binds agents into it. Nothing on this path
creates a second tree beside an existing one, and nothing deletes. A tree a
human removed comes back as data (`present = False` / `WorktreeLost`) and is
never silently recreated.

The root's branch is named structurally by the fold — the `split` or
receipt-carrying `outcome` entry whose payload node is the root plan's name —
so nothing reconstructs a branch from a worktree label.

### The split is appended twice, deliberately

`emitSplit` records the split before allocating children and again after, under
the same `(kind, key)`; the fold keeps the later one (max seq). The two appends
close two different crash windows:

- The FIRST records the decision, so a crash during child allocation replays
  the plan instead of re-running the scaffold worker.
- The SECOND adds `childTrees` (child node name → branch), the only durable
  record of which retained tree belongs to which child. Without it a resumed
  run would create a second worktree beside a child's orphaned commits and redo
  its work blind — which is the single most likely crash case, a leaf worker
  caught mid-flight.

One window remains and is not closable at this granularity: a crash between
`createWorktree` and the FIRST split append leaves nothing journaled at all, so
the fold is empty and the next boot is an ordinary fresh run (the orphaned tree
is retained, not reused). Closing it would need a new record kind for
allocation, which is a bigger write-side change than it buys.

### Carried into the summary

A resumed run's `RunSummary` is about the RUN, not the process that finished
it: the prior process's journaled `rebase` steps lead its `runTrail` and its
journaled `escalation`s lead its `runEscalations`. A node adopted from an
orphaned commit carries its own branch's recorded rebase note and escalation in
the receipt it is stamped with.

---

## The residency seam

`hyloM`'s recursive step is

```haskell
go a = coalg a >>= traverse go >>= alg
```

and `traverse go` is the ONE place S1-L4 changes. Today it is a one-shot
sequential traversal: a node is a stack frame, it runs its children to
completion in plan order, and it holds no state between them. Under residency
a node becomes a green thread whose body, after forking children, is a select
loop (`nextEvent (childFolded <|> inbox <|> agentDone worker <|> headChanged
tree)`) holding node-local state — and `traverse go` becomes `forkNode` per
child plus a fold over `folded` handles.

What this lane must NOT do, and does not: hold node-local mutable state
across children, read completion order, or thread anything through the
traversal that is not the plan's own data. Those are the three things that
would make the swap expensive. The seam is marked in `Harness.hs` at the
`hyloM` call site.

Two consequences of the swap are already anticipated in this lane's shape:

- The rebase cascade is sibling-ward and executed by the folding node, because
  without residency the folding node is the only owner of anything below it.
  Under residency it becomes `sendDown (RebaseOnto oid)` down child handles,
  and each owner executes it in its own loop — the same three tiers, moved to
  the owner. Descendant-ward propagation is deliberately NOT simulated here.
- The failure policy is a pure exhaustive case over `Failure`, so it moves
  into a resident node's message handler unchanged.

---

## Wave 2 — the policy middleware

Extracted FROM this harness after it works, not designed ahead of it. Landing
in `haskell/lib/Tidepool/Swarm.hs`, which stays DOMAIN-FREE: every wrapper is
polymorphic in the task and seed types, and every policy decision is an
effectful slot the caller supplies (`a -> m b`), so a slot can tier
deterministic heuristic → model turn → operator inside one ordinary function.

| PRD name | shape | what dev-tree passes it |
|---|---|---|
| `receipted` | `(PlanF t b -> b -> m b) -> Alg m t b -> Alg m t b` | `stampFold` — journal the outcome, then apply `foldLadder` (the rung ordering) and fill the outcome's trail |
| `budgeted` | `(a -> m (Maybe t)) -> Coalg m t a -> Coalg m t a` | `cycleRefusal` — the structural cycle allowance; `Just` truncates the node to a refusal leaf |
| `capped` | `(a -> Int) -> Int -> (a -> m (Maybe t)) -> Coalg m t a -> Coalg m t a` | `depthRefusal` at `seedDepth`/`maxDepth`; the slot returns `Nothing` for a childless plan, since a leaf at the limit was never going to unfold |
| `gated` | `(PlanF t a -> m (Maybe t)) -> Coalg m t a -> Coalg m t a` | `layerGate` — TIERED: a deterministic width heuristic, then `askUser @LayerApproval` |

Composed at the `hyloM` call site by ordinary function application:

```haskell
let coalg = gated (layerGate b)
              (capped seedDepth b.maxDepth depthRefusal
                 (budgeted cycleRefusal decompose))
    alg   = receipted stampFold integrate
```

Three things the shapes had to get right, and each one came from the harness
rather than from the sketch:

- **A refusal returns the TASK to truncate with, not an outcome.** A coalgebra's
  result type is `PlanF t a`; it cannot produce a `b`. So every slot returns
  `Maybe t` and the wrapper emits `PlanF t []`. That is what makes
  failure-as-data structural here rather than a rule to remember.
- **`gated`'s slot sees the produced layer, not the seed** — PRD locks "the
  operator approves the unfolds layer by layer, each proposed with its parent's
  real outcomes attached". It therefore runs AFTER the unfold and refuses the
  DESCENT, which is where the cost is.
- **`capped`'s slot still returns `Maybe`.** The first draft had it decide on
  depth alone; the harness immediately wanted "a leaf at the limit is not
  capped — there was nothing to unfold", which only the caller knows.

`budgeted` is the primitive guard and `capped` is its depth-shaped
specialization; saying so is better than pretending they are independent
mechanisms.

Journaling is NOT a wrapper. `record`'s payload is domain-shaped
(`SwarmStep`-shaped facts about what was decided, spawned, and folded), and
`Tidepool.Swarm` cannot know it. It stays in the harness's own algebra and
coalgebra, at each swarm step.

---

## Verification

- `scripts/battery.sh -p tidepool-harness -E 'binary(dogfood_harness_typecheck)'`
  — `dev_tree_typechecks` compiles v2 against the driver's real widened row
  (`[RunLLMTurn, AskUser, Console, Worktree, RepoEvent, Exec, Subagent,
  Journal]`). It is a fold gate for other lanes; its row moves with the
  harness or not at all. The probe names BOTH entry points at their declared
  signatures (`loop` and `resumeLoop :: ResumeFold -> State -> Harness State`)
  plus the two pure resume decisions, so a drift in either half is a compile
  failure here rather than a `DriverError::ResumeEntryMissing` at boot.
- `cargo check --workspace --tests`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo fmt --all -- --check`, `cargo nextest run`.
- No new `jit_surface` probe: the wrappers are ordinary higher-order functions
  over the same derived dictionaries `works_swarm_planf_hylo_on_jit` already
  pins, so they joined THAT probe as four more `check` lines (part (c)) rather
  than paying a second extract compile. They run on the JIT there — including
  that a coalgebra-side refusal truncates rather than throws.
