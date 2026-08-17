# PRD — Exomonad v3: the typed swarm

**Status:** proposed (2026-08-15)  
**Owner:** self-iterating harness / swarm substrate  
**Depends on:** [PRD 18](18-typed-subagent-spawning-prd.md),
[PRD 19](19-managed-worktrees-events-prd.md),
[one-session](../one-session.md),
[companion-memory](../companion-memory.md)  
**Executable design target:**
[dev-tree/Harness.hs](../../harness-dogfooding/dev-tree/Harness.hs)  
**Prior art / migration input:** `../exomonad` (Classic + v2 node-mode) — the
protocol being compiled here; never a runtime dependency.

## Summary

Every multi-agent system today — exomonad v2 included, and it is the most
honest about it — runs its coordination logic through expensive context
windows interpreting prose protocols. The TL is an interpreter of a protocol
document; the sidecar/bus/ledger machinery exists to compensate for
coordinators that cannot hold typed state. Exomonad v3 compiles the protocol:
**coordination is a typed resident Haskell program**, and cognition is
purchased only where it is genuinely needed —
implementation leaves, review judgment, planning, conflict resolution.

Four properties nothing else in this space can claim, each falling out of
substrate we already have or charter here:

1. **Compiled coordination.** Orchestration overhead costs zero tokens. In an
   exomonad v2 tree the TLs burn 10–30× leaf-rates on bookkeeping; here that
   is `runNode`. Spend concentrates on implementation and judgment.
2. **Git is the persistence layer.** Durable work lives where git already
   puts it — commits on branches in retained worktrees — indexed by an
   append-only run journal. Resume is "from last good": recorded
   splits replay instead of re-asking the planner, verified folds stand,
   and orphaned commits found in a worktree are verified and adopted, never
   redone blind. No second artifact store.
3. **Order-insensitive, testable coordination.** Policy inputs are child
   outcomes in plan order, never completion order — scheduling
   nondeterminism structurally cannot change a decision — and because every
   policy is middleware over the two function seams, swarm logic is tested
   by passing pure algebras. No agent processes anywhere in the logic test
   path.
4. **Evidence-carrying changes.** Every fold carries a typed chain — repo
   events, orchestrator-run checks, review verdicts. Trust is computed, not
   claimed.

**Vision decisions (Inanna, 2026-08-15):** built for us first, platform
extraction later; this IS exomonad's successor (v3); the end-state is a
**resident organism** per repository (backlog, memory, scheduled sweeps), not
an invoked-per-goal tool; autonomy is hybrid — fold receipts land now, all
folds gated in v1, with a repo-level "swarm can cook" policy for repos where
autonomous folding is safe, and graduated per-change-class autonomy added
where obvious.

## Product boundary

The per-layer fluency rule (PRD 18/19) is unchanged and load-bearing:

- Haskell is the compact typed language for orchestration, policy, event
  reactions, evidence, and receipts.
- Coding agents retain native edit, shell, test, and Git tools. Merges and
  rebases are agent work (PRD 19 frozen: no git workflow verbs in the
  runtime).
- Repository observations and process receipts are authority. Agent prose is
  context, never proof.

Exomonad Classic and v2 node-mode remain prior art and design input. Their
mechanisms are studied for what they defended against, then either compiled
(protocol → program), typed (prose vocabulary → sums), or dissolved
(bus/cursor/spill/paste delivery → values in one heap).

## What v2 becomes under v3

| exomonad v2 | exomonad v3 |
|---|---|
| TL = Opus window interpreting `root.md` | `runNode` — deterministic recursion; model windows only at planning points |
| Sub-TL as compression boundary | `render` of typed node State — O(fan-out) context by construction |
| Agent triad (worktree + context + actor) | `WorktreeHandle` + agent cycle + node value, lifetime-scoped lexically |
| Filesystem bus, tmux-paste, cursors, spill files | dissolved — typed values in one heap |
| `notify_parent` / `send_message` (tree-edge routing) | typed `Up`/`Down` messages over lexically scoped node handles |
| `[READY]`/`[idle]`/`[FAILED]` prose vocabulary | dissolved — sum types |
| Ledger / papers / `.exo` scans | checkpointed `State` + the durable worktree registry |
| Specs as prose templates | typed spec records rendered to prose at the agent boundary |
| Standing directives (.md + hash audit) | data threaded through spawn specs; hash audit trivial |
| File boundary checked at `merge` | deterministic check against the observed diff |
| Receipts + transfer proof | largely unnecessary: the orchestrator observes HEAD and runs checks itself at the fold sha |
| Review: `submit_branch`/`verdict`/sidecar gate | typed review ladder (below); findings schema inherited |
| Hooks/gates (`pre_tool_use` nudges) | stronger: parent services child tool calls in typed Haskell |
| tmux as UI | operator GUI: live tree, transcripts, typed triage |
| Waves | `Traversable` structure over concurrent spawns |

What stays model-work, deliberately: decomposition, spec-writing,
implementation, review judgment, merge-conflict resolution, re-planning after
failure. The intelligence gradient, made literal.

## Program structure — three stages

- **Stage 1 — the orchestration substrate.** Concurrency primitives, the
  unified event algebra, green threads, the
  journal/resume machinery, the trust ladder, the orchestration stdlib, the operator
  surface. Exit: a 20-node plan runs overnight, survives crashes, re-runs
  cost only the delta, every fold carries evidence.
- **Stage 2 — the resident factory.** The long-lived per-repo organism: typed
  goal backlog, scheduled sweeps, per-repo institutional memory (the curator
  pattern generalized), a goal-directed conversational front-end (companion
  *machinery*, distinct creature), and the autonomy policy engine.
- **Stage 3 — the flywheel.** Tidepool's own backlog runs through the
  factory. Every substrate improvement improves the factory's ability to
  improve the substrate; the repo becomes the demo (factory-produced,
  receipt-carrying commits).

## Goals

1. N concurrent agent cycles under one orchestrator, with typed async
   handles.
2. One event algebra over repository facts, agent lifecycle, deadlines,
   node mailboxes, and operator interrupts; one blocking primitive. Typed
   parent↔child messaging over lexically scoped handles: amend specs,
   surface issues, communicate replans, cancel subtrees.
3. Direct-style concurrent orchestration: fork an `M` computation as a green
   thread over the parked-continuation substrate; `forConcurrently` and every
   wave/pool/ladder idiom is authored stdlib Haskell, not Rust mechanism.
4. Data races unrepresentable: forked computations communicate by return
   value and events only; the row carries no shared-mutable-state effect.
5. Order-insensitivity by construction — completion order never reaches a
   policy input — plus full-fidelity wake journaling for observability.
   Swarm-logic tests take pure algebras/coalgebras; only the Rust substrate
   seam uses the existing `MockBackend`, and the integrated system's receipt
   is a live run.
6. Git-backed persistence: work durable in branches and retained worktrees;
   the append-only run journal (recorded splits, recorded folds);
   resume by folding the journal, adopting and verifying work found in
   worktrees; eager rebase propagation cascading a landed fold to descendant
   tips.
7. A trust ladder — observations → deterministic checks → adversarial review
   — with a typed receipt on every fold.
8. Failure as data: outcome-annotated plan trees (accumulate, never
   short-circuit), typed failure policies, operator triage as typed forms.
9. Residency: backlog, schedule, memory, and autonomy policy as durable typed
   state.
10. The operator surface to run all of the above from the GUI.

## Non-goals

- **Distribution/federation.** One orchestrator = one heap = one box. Leaves
  are remote API processes, so the ceiling is rate limits and budget, not
  CPU. Federation across orchestrators is a deferred question.
- **Multi-tenancy, hardening-for-others, product packaging.** Us-first.
- **Per-spawn model/backend policy.** Explicitly skipped for now (Inanna,
  2026-08-15); model tier and backend remain handler configuration. The seam
  is noted where it would land; nothing here forecloses it.
- **A workflow-graph DSL.** The plan is ordinary data; the orchestration is
  ordinary recursive Haskell. No new coordination language.
- **Git workflow verbs in the runtime.** PRD 19's freeze stands; integration
  agents merge.
- **Companion-at-root.** The factory's front-end is its own creature; it may
  share machinery with the companion, never identity or state.

## Design language

The authored surface is deliberately conservative Haskell — the production
core: sum types with exhaustive case, `Either` for every failure, newtype
keys, `Map` state, derived `Functor`/`Foldable`/`Traversable`, records of
functions for policies, pure decision cores inside effectful shells.
Branching logic, iterated heuristics, and long functions are welcome; what is
not welcome is a niche feature where a plain one expresses the same thing. A
non-obvious feature earns its place exactly when no simpler expression exists
— the recursion-scheme core below is the canonical example (nothing simpler
separates how-to-split from how-to-combine from how-to-traverse), and
phantom-indexed state is the canonical counterexample (a wrapper plus a test
says the same thing in plain language).

## Locked decisions

### The hylo core

- **The swarm is a monadic hylomorphism, and agents are its algebra and
  coalgebra.** The engine: `hyloM alg coalg = go where go a = coalg a >>=
  traverse go >>= alg`, over `data PlanF a = PlanF { task :: Task, kids ::
  [a] }` with derived `Functor`/`Foldable`/`Traversable`. Cognition enters at
  exactly two typed seams: `decompose` (how to split — the planning window
  plus the parent-first scaffold ladder; child worktrees seed from the
  reviewed scaffold HEAD) and `integrate` (how to combine — leaf
  implementation, or the merge agent plus checks). Everything the v2 TL does
  lives in one of those two seams or is compiled.
- **Fused: the plan never materializes.** A hylo unfolds a node, works it,
  folds it; what persists is git plus the run journal, not a tree.
  Decomposition is
  thereby lazy — a node's planning window runs only after its parent's
  scaffold folded, so every layer is planned with the parent's actual
  outcomes in hand (v2's "the wave boundary is where understanding
  accumulates," as evaluation order rather than TL discipline). Replanning a
  failed subtree is re-unfolding its seed with an amended goal, never
  rewriting a tree in place. A whole-plan artifact exists only when a
  preview render is explicitly requested.
- **Policies are middleware over the two seams.** `type Alg = PlanF Outcome
  -> M Outcome`, `type Coalg = Seed -> M (PlanF Seed)`. Every policy below
  ships as a named wrapper — `receipted`, `budgeted`,
  `gated`, `capped` — composed by ordinary function application and testable
  in isolation against a mock algebra. A new policy is a new wrapper, never
  a driver change.
- **Policy slots are effectful: `a -> M b`.** A gate may run cheap
  deterministic heuristics, escalate to a specifically-prompted model turn
  (`runLLMTurn @GateDecision`), consult memory or the run journal, or ask
  the operator — tiered inside one ordinary function with ordinary
  branching. Pure gates are the degenerate `pure .` case, and gates stay
  pure where they can for testability. A model call inside a gate is
  journaled and mock-substitutable like any other model turn.
- **The traversal strategy is a first-order value.** The strategy applied at
  `traverse go` — `Sequential | Concurrent | Pooled Int` — is plain data
  interpreted at the one recursive call site. The schedule is the plan's
  data-dependency structure; the green-thread substrate plugs in underneath
  without the hylo changing shape.
- **Approval gating for non-`SwarmCanCook` repos lives in the coalgebra**
  (`gated`): the operator approves unfolds layer by layer, each proposed
  with its parent's real outcomes attached — not a speculative whole-tree
  sign-off.
- **Model-authored functions are drop-in upgrades, not architecture.** A
  policy slot takes an ordinary function and does not care where it came
  from; `runLLMTurn @([Finding] -> GateDecision)` or `@(Spec -> Spec)`
  (riding the landed closure-delivery machinery) may replace a hand-written
  policy when evidence argues for it.

### Concurrency substrate

- **One-agent-at-a-time is lifted.** `SubagentHandler` holds a cycle table of
  running agents keyed by handle. This supersedes PRD 18's chartered
  single-slot constraint; the saga per cycle is unchanged. Backend processes
  are per-cycle and independent.
- **`spawnAsync :: SpawnSpec -> M (AgentHandle r)`** starts a cycle and
  returns immediately. `AgentHandle` is a typed, cycle-scoped runtime value —
  like every PRD 19 handle it never crosses a resident-cycle boundary; what
  crosses is the recorded outcome.
- **`spawnAgent` is the derived synchronous form** — spawn + await, one
  implementation, exactly as it is today the zero-tools case of the tool
  loop. No second code path.
- **Agent completion is an Event source.** `agentDone :: AgentHandle r ->
  Event (AgentExit r)` joins `commit`/`headChanged` in the ONE algebra —
  there is no second notification channel, no callback registry, no bus.
  `AgentExit r` is the typed terminal: success carries the decoded `r` plus
  the spawn receipt; failure carries the `SpawnError`. `await h = nextEvent
  (agentDone h)`; select is `nextEvent` over `<|>`-merged sources.
- **`nextEvent :: Event a -> M (Observed a)` is the one blocking
  coordination primitive.** Subscribe, drain-until-first-match, unsubscribe —
  the scoped sibling of `withHandler`, sharing its registry, its no-replay
  rule, its queue bounds, and its loud-overflow semantics.
- **Deadlines are events.** `after :: Millis -> M (Event Tick)` (a per-call
  timer source). Reviewer abandonment, cycle timeouts, and watchdogs are
  authored `nextEvent (agentDone h <|> tickOf deadline)` selects — timeout
  policy is Haskell, only the clock is Rust.
- **Cancellation is real.** `cancelAgent :: AgentHandle r -> M ()` reaps the
  backend process and retires the cycle; the worktree is retained (PRD 19).
  Cancelling an already-terminal handle is a no-op, not an error.

### Green threads (the scheduler)

- **`forkM :: M a -> M (Promise a)`** parks the forked computation as a new
  continuation in the session's hole registry and returns a promise; `promiseDone
  :: Promise a -> Event a` joins the algebra (so `awaitP = nextEvent .
  promiseDone`, and mixed selects over agents and forks are ordinary). The
  multi-hole registry ("a set of parked holes, each resumable by identity in
  any order") is the scheduler substrate; parked continuations are GC roots
  exactly as parked holes are today.
- **Cooperative, single-threaded semantics.** The driver's loop: an agent
  cycle, timer, or repository observation completes → resolve which parked
  continuation it wakes → resume it until it parks again. FIFO ready queue.
  No preemption: a forked computation that never performs a parking effect
  starves its siblings — acceptable, it is authored code, and the round/
  budget caps bound it.
- **Structured concurrency.** A fork is created within a lexical scope
  (`withForks` / the combinators built on it) and cannot outlive it: scope
  exit awaits or cancels stragglers. An uncaught failure in a fork fails the
  join point — the same propagation posture as PRD 19 handler failure. No
  detached daemons.
- **Races unrepresentable.** No `IORef`/`MVar`/shared-cell effect is added to
  the row — this is a decision, not an omission. Forked computations
  communicate by return value, events, and typed messages over handles —
  message passing, never shared state. Checkpointed `State` is
  threaded through the loop exactly as today; forks return values that the
  single loop folds.
- **Order-insensitivity is the concurrency-correctness contract.** Stdlib
  folds read child outcomes in plan order, and completion order is never an
  input to any policy or gate — scheduling nondeterminism structurally
  cannot change a decision. Pinned by a seeded mock-tier property test that
  permutes completion orders and asserts an identical outcome tree. Wakes
  are still journaled to the durable transcript (source, continuation,
  payload digest) — observability, not a replay mechanism.
- **Checkpoint/rotation discipline.** Checkpoints and machine rotation happen
  only at quiescent boundaries (no live forks, no in-flight cycles) — the
  existing rotation refusal generalizes. A crash mid-wave loses only
  in-flight cycles; the run journal (below) makes re-entry
  incremental.
- **`forConcurrently` and friends are stdlib derivations,** not primitives:
  `forConcurrently xs f = withForks (traverse (forkM . f) xs) (traverse awaitP)`
  — written once in `Tidepool.Swarm`, testable in the harness, visible to
  authors as ordinary Haskell.

### Node residency and messaging

- **An interior node is a resident computation, not a stack frame.** A node
  is a green thread whose body, after forking children, is a select loop —
  `nextEvent (childFolded <|> inbox <|> agentDone worker <|> headChanged
  tree)` — holding node-local state (phase, review rounds, queued
  instructions) and spawning ephemeral workers for everything cognitive:
  implement, review, resolve a merge, resolve a rebase. An idle node costs
  zero tokens. **The select loop itself is stdlib plumbing** — a harness
  supplies policies, specs, and message handlers, never the loop.
- **Cognition windows are concurrent; only the machine serializes.** Each
  `runLLMTurn` window (planning, gate, triage) is its own context window —
  an answerer realm bound to the node that opened it — and N windows may be
  open at once across the tree. A window spends its wall time in inference,
  off-machine, with its continuation parked — exactly when the scheduler is
  free to run anything else — so windows overlap freely alongside the
  concurrent agent cycles. The one genuine serialization is machine
  occupancy (compiling and executing a window's block), interleaved at
  suspension points like everything else; loosening even that is a noted
  substrate aspiration (Inanna, 2026-08-15), not a design constraint.
  S1-L4 implication: answerer realms mint per window, not per loop.
- **Exactly one owner per worktree: its node.** Every operation on a
  worktree — worker spawns, rebases, checks — is serialized through the
  owning node's loop. This is what makes "rebase while a worker is
  mid-cycle" unrepresentable rather than merely forbidden.
- **Handles are the addressing: possession is permission.** Forking a child
  yields a `NodeHandle` (send down + await fold); the child's body receives
  its `Uplink` and inbox as arguments. No registry, no node ids, no router —
  v2's "tree-edges only" rule is enforced by lexical scoping: a node cannot
  message a node it was never handed.
- **Message vocabulary is a harness-defined pair of plain sums,** uniform
  across the tree (the plan functor is homogeneous): e.g. `Down = RebaseOnto
  Oid | AmendSpec SpecPatch | Cancel`, `Up = Escalate Failure | Progress
  Text`. Steering — amending a live child's spec, surfacing a stuck worker's
  question, communicating a replan, cancelling a subtree — is ordinary
  message handling in the select loop, and each node reaps its own workers
  on `Cancel`, so teardown drains locally. A node handles a message the
  moment it selects it; what it does about an in-flight worker cycle is
  policy — cancel-and-respawn with the amended spec, or let the cycle
  finish and fold the amendment into the next review round.
- **Sends never block; receives always select.** `sendDown`/`sendUp` are
  nonblocking; a node's loop selects over its whole event set, inbox
  included. No cycle of blocking waits can form, so parent-awaiting-fold
  while child-escalates-upward resolves instead of wedging. Mailboxes
  coalesce per message type (`RebaseOnto` keeps only the latest target;
  `Cancel` overrides everything), which is also what bounds them.
- **Messages are reconciliation hints, never truth.** A crash loses in-heap
  mailboxes, and that is safe by design: every instruction is re-derivable
  on resume from git plus the run journal (`RebaseOnto` from comparing a
  child's base against its parent's current HEAD; escalations from recorded
  outcomes). The repo is the state; messages only accelerate convergence
  toward it. No durable mailbox machinery exists or is wanted.

### Persistence and resume: git + the run journal

- **Git is the store; there is no second artifact store.** Durable work
  product lives where git already puts it — commits on branches in retained
  worktrees, resolvable through the durable registry. The swarm never
  duplicates it into a cache, memo, or trace database.
- **Progress is an append-only run journal, serviced by the driver.** The
  loop-boundary State checkpoint cannot carry swarm progress — a whole run
  happens inside ONE `loop` call, and under green threads no single fork
  owns a mutable State to checkpoint anyway. So progress is event-sourced
  instead: one effect, `record :: SwarmStep -> M ()`, appends each completed
  step durably as it happens — `SplitRecorded { branch, plan }` (the
  coalgebra's output — decomposition is cognition, so it is recorded, never
  re-derived), `OutcomeRecorded { branch, headOid, receipt }`,
  `RebaseRecorded { branch, ontoOid }`. Appends are tiny, serialize through
  the driver, and are safe from any concurrent node loop. The crash-replay
  fold precedent (`replay.rs`'s `fold_tree_state`) is the model; the
  loop-boundary State checkpoint is unchanged and still owns everything that
  is not swarm progress.
- **Resume folds the journal.** Boot folds the run's journal into a map
  (branch → recorded split / outcome / rebase target) and the hylo re-enters
  against it: recorded splits replay instead of re-asking the planner
  (without this, a nondeterministic re-plan orphans every completed child
  below it), recorded outcomes stand, and only genuinely unstarted work
  spawns agents.

  Open sub-questions, deliberately listed: (i) is `SwarmStep` a
  stdlib-fixed shape or harness-extensible (fixed is simpler; extensible
  lets a harness journal domain facts — lean fixed with one opaque payload
  field); (ii) is the folded map injected into the harness at boot or read
  through an effect (lean inject — resume should not be able to forget to
  look); (iii) journal lifecycle — one file per run id, compacted or
  deleted after the run's terminal fold (lean per-run file, retained like
  worktrees).
- **Adopt and verify, never redo blind.** The crash window between "agent
  committed" and "step journaled" is detectable deterministically: a
  worktree whose branch moved past its seed with no recorded outcome holds
  orphaned work. Resume runs the ladder on what it finds — checks, then
  review — and respawns implementation only if that fails. The trust ladder
  doubles as the crash-recovery protocol.
- **Retained worktrees rebind, not recreate.** The journal names the
  branch; the registry rebinds it (`spawnSpecIn`) — the companion's
  `memWorktree` pattern, generalized.

### Eager rebase propagation

- **A fold landing on a node cascades rebases to its descendants — delivered
  as messages, executed by each owner.** When a node's branch moves it sends
  `RebaseOnto newHead` down its child handles. Each child processes the
  instruction in its own loop at a select point — never during a live worker
  cycle (ownership serializes; instructions queue and coalesce to the latest
  target). An interior child, having rebased, sends onward to its own
  children: the cascade is structural recursion on the tree's edges. This is
  PRD 19's original `pokeChildren` sketch, revived now that subtrees run
  concurrently.
- **Mechanical first, cognition second, escalation third.** The receiving
  node first attempts the rebase mechanically — `git rebase` via Exec in the
  worktree it owns, aborted on any conflict — clean means done at zero
  tokens. A conflict spawns an ephemeral resolution agent (native git tools;
  trivial conflicts are its bread). Exhaustion or review rejection sends
  `Escalate` up the uplink into the parent's ordinary failure policy:
  re-plan from the new base, defer to integration, or operator triage. The
  Exec tier is authored policy running plain git in an owned worktree; the
  PRD 19 freeze (no workflow verbs in the runtime crates) is untouched. The
  same tier makes a conflict-free fold the mechanical case of integration —
  the fast-forward question dissolves into it.
- **This supersedes dev-tree v1's "no rebase propagation" rationale.**
  Depth-first ordering answered the problem only while execution was
  sequential; under concurrent subtrees a parent's branch moves while
  children live (sibling folds land), so children are kept current instead
  of drifting until integration. Initial seeding is unchanged: children
  still seed from the parent's reviewed scaffold HEAD.
- **Cascades converge.** A rebase task is "rebase onto the parent's current
  tip," so regardless of arrival order every descendant ends rebased onto
  the final parent HEAD; intermediate arrival order affects only how much
  work each rebase does, never the terminal state.

### The trust ladder and evidence

- **Three rungs, strictly ordered:** (1) repository observations —
  authoritative, incorruptible; (2) orchestrator-run deterministic checks —
  `Exec` on the outer row, sandboxed to the node's worktree, running the
  spec's verify commands at the actual fold sha; (3) adversarial review —
  semantic judgment, the only rung that catches "compiles but wrong." **A
  higher rung never overrides a failing lower rung.** A reviewer approval
  with a red check is a red node.
- **The review ladder (shape A: fresh one-cycle spawns, state as data).**
  After an implementation cycle: spawn a reviewer in the same worktree,
  prompted to refute — verify the claims, run the checks yourself, inspect
  the diff. It finalizes a single-constructor record (the schema-sum rule):
  `Review { approved :: Bool, findings :: [Finding], evidence :: [Text] }`,
  `Finding { file, line, severity, body, suggestion }` — the schema inherited
  from v2's `verdict`. Not approved + rounds remaining → respawn the
  implementer with the findings verbatim; re-review. Unresolved findings
  carry across rounds (v2's ReviewLog continuity), the rounds live in the
  node's journal record, and exhaustion is a typed `ReviewExhausted` outcome carrying
  the findings into triage. The policy is a handle-pattern record with an
  effectful decision slot — `ReviewPolicy { maxRounds :: Int, gate ::
  [Finding] -> M GateDecision }` (default `maxRounds` 2) — so a gate can
  tier: deterministic severity rules first, a specifically-prompted model
  turn on ambiguity, the operator past that. Review
  applies to implementation AND integration nodes; a per-node/per-subtree
  `skipReview` flag exists and is stamped into the fold receipt — an
  audited escape hatch, exactly v2's `dangerously_skip_reviewer`.
- **Review must gate before descent.** Children seed from a parent HEAD that
  passed the parent's ladder; an unreviewed parent never seeds children.
- **File boundaries are checked, not requested.** A spec's boundary is data;
  at fold time the orchestrator diffs the branch against its seed (worktree
  diff verbs, below) and refuses out-of-boundary folds with the offending
  paths named; override is explicit and stamped into evidence. Exact-or-
  directory-prefix matching, v2's separator rule included.
- **The fold receipt is a typed value per fold:** seed and head oids,
  observed HEAD moves, checks run with exit codes, review rounds and
  verdict, boundary result, skip/override stamps. Rendered for humans in the
  GUI and the fold commit's notes; carried in the journal's
  `OutcomeRecorded`. Nothing merges without one. After a MECHANICAL rebase,
  checks re-run at the new head and a standing review verdict survives;
  after an AGENT-RESOLVED rebase, checks and review both re-run — the
  resolution is new, unreviewed work.
- **No transfer proof needed.** v2's `commit_tested` machinery closed the gap
  between "sha the child verified at" and "sha the parent merges" — here the
  orchestrator runs the checks itself at the fold sha, so the gap never
  opens. The mechanism is honored by deletion.

### Failure as data

- **Failure flows through the fold as values, never short-circuits.**
  `Outcome` carries a plain sum (`Done Receipt | Failed Failure | Skipped
  Reason`), and the algebra receives `PlanF Task [Outcome]` — failed
  children arrive as ordinary values in its input list. `traverse` visits
  every sibling by construction, so "accumulate, don't short-circuit" is
  what the engine does rather than a rule to remember; the algebra's failure
  policy is an exhaustive case the compiler audits. `render` shows the
  annotated outcomes from the journal.
- **Failure policy is a sum, applied by deterministic code:** `OnFailure =
  Retry | Replan | AskOperator | Abandon` — per node, defaulted per plan.
  `Replan` opens a planning window (`runLLMTurn`) scoped to the failed
  subtree's render; `AskOperator` is a typed triage form. Model cognition
  enters only through those two constructors.
- **Budgets are enforced, not advisory:** per-run agent-cycle cap, per-node
  retry cap, wall-clock deadline — all in `State`, all visible in render,
  all refusing loudly at the cap.

### Residency (Stage 2)

- **The factory is a long-lived per-repo resident** on the selfharness
  driver: its `State` carries the backlog (a typed DAG of goals → plans →
  outcomes), the schedule, the autonomy policy, and the memory digest.
  One-shot invocation is a resident with a single-item backlog.
- **Per-repo institutional memory** is the companion-memory architecture
  generalized: a git store curated by a spawned agent, holding conventions,
  past failures, review patterns ("what reviewers keep catching" feeds
  future specs and directives). Same store layout, same receipt discipline,
  separate store.
- **Autonomy policy is data, evaluated against fold receipts.** v1: every
  fold gates on the operator. A repo marked `SwarmCanCook` auto-folds any
  node whose receipt is fully green (checks pass, review approved,
  boundary clean, no overrides). Graduated per-change-class rules
  (docs-only, test-only, mechanical migration) are added where obvious, each
  class a typed predicate over the receipt — never prompt prose.
  Every autonomous fold is journaled and surfaced in the next operator
  session.

## Public surface (sketch)

```haskell
-- Tidepool.Agent.Spawn (extended)
spawnAsync   :: JsonSchema r => SpawnSpec -> M (AgentHandle r)
cancelAgent  :: AgentHandle r -> M ()
agentDone    :: AgentHandle r -> Event (AgentExit r)

-- Tidepool.Event (extended)
nextEvent    :: Event a -> M (Observed a)
after        :: Millis -> M (Event Tick)

-- Tidepool.Fork (green threads)
forkM        :: M a -> M (Promise a)
promiseDone  :: Promise a -> Event a
withForks    :: M a -> M a          -- structured-concurrency scope

-- Tidepool.Swarm (authored stdlib, all derived)
data PlanF a = PlanF { task :: Task, kids :: [a] }
  -- deriving (Functor, Foldable, Traversable)
type Alg     = PlanF Outcome -> M Outcome
type Coalg   = Seed -> M (PlanF Seed)
hyloM        :: Strategy -> Alg -> Coalg -> Seed -> M Outcome
data Strategy = Sequential | Concurrent | Pooled Int

receipted    :: Alg -> Alg                   -- stamp the fold receipt; refuse without one
budgeted     :: Budget -> Coalg -> Coalg     -- refuse to unfold past caps
gated        :: (Layer -> M Approval) -> Coalg -> Coalg  -- layer-by-layer operator gate
capped       :: Depth -> Coalg -> Coalg

forConcurrently :: [a] -> (a -> M b) -> M [b]
pool         :: Int -> [Spec] -> M [Outcome] -- flat work, bounded pull
reviewLadder :: ReviewPolicy -> WorktreeHandle -> Spec -> M Outcome

-- node residency (handles as capabilities; harness defines Down/Up sums)
data NodeCtx up down = NodeCtx { uplink :: Uplink up, inbox :: Event down }
forkNode     :: (NodeCtx up down -> M r) -> M (NodeHandle down r)
sendDown     :: NodeHandle down r -> down -> M ()  -- nonblocking; coalesces
sendUp       :: Uplink up -> up -> M ()            -- nonblocking
folded       :: NodeHandle down r -> Event r
data ReviewPolicy = ReviewPolicy
  { maxRounds :: Int
  , gate      :: [Finding] -> M GateDecision -- tiered: heuristics → model turn → operator
  }
```

The ten-tests wave, as the surface should read:

```haskell
scaffold <- reviewLadder policy tree scaffoldSpec
trees    <- traverse (childWorktree tree) testNames
outs     <- forConcurrently (zip trees testSpecs) $ \(t, s) ->
              reviewLadder policy t s          -- reviews pipeline behind impls
integrate tree scaffold outs
```

## Implementation plan — Stage 1 lanes

- **S1-L1 — row servicing.** Widen `outer_decls` (+ Console, RepoEvent,
  Exec); service Console/Worktree/RepoEvent/Exec suspensions the
  `service_outer_subagent` way; shared handler roots (Worktree ↔ Subagent
  must resolve the same registry) and explicit source-repo config in the
  bin; mock-tier acceptance (two-node plan, MockBackend, TestRepo).
- **S1-L2 — the cycle table.** Lift one-agent-at-a-time; `spawnAsync` /
  `AgentHandle` / `cancelAgent`; `agentDone` as an observation source;
  `AgentExit`. Acceptance: three mock cycles in flight, completion events
  observed in completion order, cancellation reaps.
- **S1-L3 — the hylo swarm.** `nextEvent` + `after`; `Tidepool.Swarm` v1:
  `PlanF`/`hyloM` with `Sequential` strategy, the middleware wrappers
  (`receipted`/`budgeted`/`gated`/`capped`), `pool`, the review ladder with
  its effectful gate slot; dev-tree v2 as the factoring of `runNode` into
  algebra + coalgebra + wrappers — the run journal + resume, boundary
  checks, fold receipts, budgets. Acceptance: the ten-test wave live in a scratch
  repo (`Concurrent` arrives with L4; `pool` covers flat fan-out
  meanwhile); kill -9 mid-wave and resume finishes only the unfinished.
- **S1-L4 — green threads + node residency.** `forkM`/`Promise`/`withForks`;
  the driver scheduler; the wake journal (observability); the
  order-insensitivity property test; `forkNode`/`sendDown`/`sendUp` with
  inboxes as Event sources and per-type coalescing. dev-tree v3 goes
  direct-style: interior nodes become resident select loops. Acceptance:
  permuted completion orders under a seeded mock backend produce the
  identical outcome tree; a parent amends a live child's spec and cancels a
  subtree, and teardown drains bottom-up.
- **S1-L5 — resume hardening + rebase cascade.** Recorded splits replayed on
  resume; adopt-and-verify for orphaned commits; the message-delivered
  rebase cascade with the mechanical-first tier. Acceptance: kill -9 between
  an agent's commit and its journal entry → resume verifies and adopts the work
  without a fresh implementation cycle; a fold on a parent leaves every live
  descendant tip rebased onto it, with a clean rebase spawning zero agents;
  a gnarly conflict escalates parent-ward instead of wedging the child.
- **S1-L6 — operator surface.** Live outcome-tree pane (with per-node
  wall-clock and cost observability), one GUI tab per node keyed by its
  managed branch name (the natural substrate-level id — unique, durable,
  human-legible), agent transcript streaming over the backend seam, typed
  triage forms, fold-receipt view.

Lanes L1→L3 are strictly ordered; L4 and L5 are independent of each other
after L3; L6 tracks alongside from L3.

Stage 2 lanes (chartered here, spec'd when Stage 1 exits): backlog DAG +
scheduler; per-repo memory store; the goal-directed resident front-end; the
autonomy policy engine; the audit surface.

## Acceptance criteria (program-level)

1. A 20-node plan runs overnight unattended in a `SwarmCanCook` scratch repo:
   crashes resumed, budget respected, every fold receipt-carrying.
2. kill -9 at arbitrary points: resume never re-asks the planner for a
   recorded split, never redoes a recorded fold, and adopts orphaned
   commits after running the ladder on them.
3. Swarm logic — wrappers, folds, budgets, resume — is covered by tests
   that pass pure algebras, zero agent processes; the seeded permutation
   test shows completion order cannot change the outcome tree. The
   substrate seam keeps its existing `MockBackend` coverage; the
   integrated receipt is criterion 1's live run.
4. A worker that claims completion without committing is caught by rung 1
   (no observed HEAD move) and never reaches review.
5. A reviewer approval over a failing check does not fold.
6. The operator can watch the live tree, read any agent's transcript, and
   triage any failure from the GUI without touching a terminal.
7. Mid-flight steering works end to end: a parent amends a live child's
   spec, a `Cancel` drains a subtree bottom-up, and a gnarly rebase
   escalates to the parent's policy instead of wedging the child.

## Deferred questions

1. **Federation.** Multiple orchestrators over one backlog (multi-box, or
   org-of-repos). Not before Stage 2 exits.
2. **Per-spawn model/backend policy.** Skipped now; revisit when reviewer
   diversity or cost pressure demands it.
3. **Machine renewal under long residency.** The resident heap fragments
   over hours of allocation; today the driver swaps in a fresh machine at a
   quiescent loop boundary past a fragment ceiling. A Stage-1 run gets that
   boundary between runs; a Stage-2 resident that never sleeps does not.
   Punted until residency: likely answer is renewing at root-fold
   boundaries.
4. **Harness distillation.** Harnesses generating harnesses — the
   README's "manual precursor to autonomous distillation" promise. Stage 3
   territory.
5. **Answer-contract streaming for planning windows.** Whether `Replan`
   windows want multi-turn structured planning (propose → critique → commit)
   rather than one `runLLMTurn`.
