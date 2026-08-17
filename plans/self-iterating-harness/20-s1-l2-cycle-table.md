# PRD 20 S1-L2 — the cycle table and the typed async spawn surface

Lift `SubagentHandler`'s one-agent-at-a-time constraint into a concurrent cycle
table, and add `spawnAsync` / `awaitAgent` / `cancelAgent` over an opaque
`AgentHandle r`. `spawnAgent` keeps its exact signature and becomes the derived
synchronous form (spawn + await) — ONE implementation.

Design authority: `plans/self-iterating-harness/20-exomonad-v3-prd.md`
§"Concurrency substrate". This file is the lane's implementation contract; the
PRD is the decision authority and wins on any conflict.

**Out of scope, deliberately.** The event algebra (`agentDone`, `nextEvent`,
`after`) is a separate lane — this lane delivers a DIRECT blocking
`awaitAgent` verb so the surface works standalone. No `ModelPolicy` /
model-selection changes. No `forkM` / green threads. No changes to
`tidepool-harness/src/selfharness/driver.rs` (a parallel lane owns it):
driver-side servicing of the new verbs rides the generated
`SubagentReq::from_value` + `EffectHandler::handle` path automatically, and is
wired at merge time.

---

## The shape of the problem

Today the whole concurrency limit is two facts:

1. `CoupledSpawner` (`tidepool-agent/src/spawn.rs`) holds `running:
   Option<RunningAgent>` and refuses a second `begin` with `NotRunning`.
2. `SubagentHandler` (`tidepool-handlers/src/handlers/agent.rs`) holds ONE
   `Box<dyn AgentBackend + Send>`.

Both have to go, and the awkward part is that they cannot go the same way.
The worktree substrate (`WorktreeManager` + the flocked `BindingTable`) is
single-owner by construction — `BindingTable::open` takes a lifetime flock on
its binding root, so there is exactly one `CoupledSpawner` per binding root and
"one spawner per cycle" is not available. The backend, by contrast, must be
per-cycle: an `AgentBackend` is a step function over one live thread.

So the split is:

- **Substrate — shared, mutex-guarded, SHORT critical sections.** Allocate a
  worktree, take a binding, settle a binding, mint an agent id. Milliseconds.
- **Saga — per-cycle, detachable, owns no lock while it blocks.** Every
  `start_thread` / `start_turn` / `resume` call happens with no substrate lock
  held. This is the load-bearing invariant of the whole lane: a saga that
  holds the substrate mutex across a backend call serializes every cycle and
  silently reinstates one-at-a-time.

---

## Wave plan

Three lanes; A and B are concurrent, C follows the fold.

| Lane | Crate(s) | Depends on |
|---|---|---|
| **A** — concurrent saga substrate | `tidepool-agent` | — |
| **B** — wire + Haskell contract | `tidepool-bridge-effects`, `tidepool-mcp`, `haskell/lib/Tidepool/Agent/Spawn.hs`, `tidepool-harness/src/engine.rs`, stub methods in `tidepool-handlers/src/handlers/agent.rs` | — |
| **C** — the cycle table | `tidepool-handlers/src/handlers/agent.rs` | A + B |

A and B are disjoint by file. A deliberately keeps `CoupledSpawner::begin` /
`answer` / `spawn_one_cycle` at their EXACT current signatures so that
`handlers/agent.rs` needs no edit in wave 1 — that is what makes A and B
mergeable in either order.

---

## Lane A — `tidepool-agent`: a saga that detaches from its spawner

### A1. `SpawnSubstrate`

Extract the shared, single-owner state out of `CoupledSpawner`:

```rust
/// The single-owner worktree substrate a cycle touches, and the ONLY thing
/// concurrent cycles contend on. Every method here is a SHORT critical
/// section — no backend call may happen under this lock.
pub struct SpawnSubstrate {
    manager: WorktreeManager,
    bindings: BindingTable,
    next_agent: u64,
}
```

Methods, all short: `resolve_workspace`, `bind`, `settle`, `roll_back`,
`mint_agent_id`, `manager()`, `bindings()`, `source_repository()`.

`CoupledSpawner` becomes `{ substrate: Arc<Mutex<SpawnSubstrate>>, running:
HashMap<AgentId, CycleSaga> }`. Add `pub fn substrate(&self) ->
Arc<Mutex<SpawnSubstrate>>` (a clone of the `Arc`) — that is the handle a
cycle thread carries.

A poisoned mutex is a panic in another cycle's saga, which means the substrate's
invariants are unknown. Propagate it as a loud failure
(`SpawnError::Binding`-shaped or a dedicated rendering) — never `unwrap()` it
silently and never recover by ignoring poisoning.

### A2. `CycleSaga` — the ONE saga implementation

```rust
/// One cycle's saga state, detached from the spawner that started it.
///
/// Carries its own `Arc<Mutex<SpawnSubstrate>>`, so a saga can be moved to
/// another thread and driven there. Locks the substrate ONLY to bind, settle,
/// and roll back — never across a backend call.
pub struct CycleSaga { /* agent, worktree, thread, binding_ref, parked, rounds, substrate, finished */ }

impl CycleSaga {
    /// Run the saga from Allocating to its first stop. Returns the saga
    /// alongside the step even when the step is `Done`, so a caller can read
    /// the terminal state uniformly; `is_finished()` says which it is.
    pub fn begin(
        substrate: &Arc<Mutex<SpawnSubstrate>>,
        backend: &mut dyn AgentBackend,
        request: &SpawnRequest,
    ) -> Result<(Self, SpawnStep), SpawnError>;

    /// Answer the parked call and drive on. Same checks as today's
    /// `CoupledSpawner::answer`: wrong agent, wrong call, nothing parked, and
    /// the `MAX_TOOL_ROUNDS` backstop all keep their current `SpawnError`
    /// spellings verbatim.
    pub fn answer(
        &mut self,
        backend: &mut dyn AgentBackend,
        agent: AgentId,
        call: ToolCallId,
        outcome: ToolOutcome,
    ) -> Result<SpawnStep, SpawnError>;

    /// Drive to completion, refusing every tool call — today's
    /// `spawn_one_cycle` body, now living on the saga.
    pub fn run_to_completion(
        &mut self,
        backend: &mut dyn AgentBackend,
    ) -> Result<OneCycleRun, SpawnError>;

    pub fn agent(&self) -> AgentId;
    pub fn worktree(&self) -> &WorktreeHandle;
    pub fn is_finished(&self) -> bool;

    /// Settle the binding `Released` for a cycle that is being ABANDONED
    /// (cancelled) rather than completed. Idempotent on an already-settled
    /// binding, so cancelling a terminal cycle is a no-op.
    pub fn abandon(&mut self) -> Result<(), WorktreeError>;
}
```

`abandon` is what `cancelAgent` needs: retain-first is locked
(`tidepool-worktree/CLAUDE.md`), so cancellation settles the binding
`Released` and leaves the worktree registered and rebindable. It does NOT
delete anything.

**Cancel SETTLES; it does not merely kill (root, hold 1).** Reaping the
backend process is half of cancellation — the half that stops the work. The
other half is that the cycle's binding row must not be left `Active` pointing
at an agent that will never run again; that is precisely the orphan the saga's
rollback semantics exist to prevent, and a cancelled cycle is no more entitled
to leak one than a failed cycle is. Order matters: **kill first, then take the
substrate mutex briefly to settle.** Settling before the kill would hold the
lock across a reap of unknown duration, and the invariant above forbids that.
`abandon` is idempotent: on a saga that already settled (completed, rolled
back, or previously abandoned) it is a no-op returning `Ok(())`, never a
second write and never an error.

### A3. `CoupledSpawner` keeps its API, loses its refusal

- `begin(&mut self, backend, request) -> Result<SpawnStep, SpawnError>` —
  **identical signature**, now `CycleSaga::begin` + insert into
  `running: HashMap<AgentId, CycleSaga>`. The one-at-a-time refusal at the top
  of today's `begin` is DELETED.
- `answer(&mut self, backend, agent, call, outcome)` — **identical
  signature**, now a lookup in `running` + `CycleSaga::answer`; a finished
  saga is removed from the map. An unknown agent stays
  `NotRunning { agent, detail: "no agent is mid-turn" }` — that exact string
  is asserted by `handler_resume_with_no_agent_running_is_a_drive_failure` in
  `tidepool-handlers/src/handlers/agent.rs`; do not reword it.
- `spawn_one_cycle(&mut self, backend, request)` — **identical signature**,
  now `CycleSaga::begin` + `run_to_completion`, holding no map entry.
- `begin_detached(&self, backend, request) -> Result<(CycleSaga, SpawnStep), SpawnError>`
  — NEW. The saga is handed to the caller instead of stored. This is what a
  cycle thread uses.
- `running_agent() -> Option<AgentId>` is DELETED (ambiguous once N run) and
  replaced by `running_agents() -> Vec<AgentId>`, sorted. Only
  `tidepool-agent/tests/spawn_saga.rs` uses it.

Update the `CoupledSpawner` doc comment: the "ONE at a time, deliberately"
paragraph is now false and must go, replaced by the substrate-lock invariant
from "The shape of the problem" above.

### A4. Per-cycle backends: a factory

```rust
// tidepool-agent/src/backend/mod.rs
/// Makes one backend instance per cycle. Concurrent cycles never share a
/// backend: an `AgentBackend` is a step function over ONE live thread.
pub trait AgentBackendFactory: Send {
    fn create(&mut self) -> Result<Box<dyn AgentBackend + Send>, AgentBackendError>;
}
```

Provide:
- a closure adapter so `|| CodexAgentBackend::new().map(|b| Box::new(b) as _)`
  is a factory without a named type per call site;
- a `CodexAgentBackend` factory. **Verify and state in the doc comment** that
  `CodexAgentBackend::new()` per cycle shares no mutable config — the config
  isolation rule in `tidepool-agent/CLAUDE.md` (no worker run may mutate the
  operator's `~/.codex`; `cwd` at TURN start, never at thread start) must hold
  for N concurrent instances exactly as for one. If N instances would contend
  on anything writable, say so loudly in the lane report rather than papering
  over it.

### A5. Cancellation that actually reaps

A cycle thread blocked inside `backend.start_turn` cannot be interrupted by a
flag the thread itself would have to check. The backend must hand out a
canceller BEFORE it starts running:

```rust
/// Reaps a backend's underlying process/session FROM ANOTHER THREAD, making
/// its in-flight seam call return. `Send + Sync`, because the whole point is
/// that it is held by someone other than the cycle thread.
pub trait BackendCanceller: Send + Sync {
    fn cancel(&self);
}

trait AgentBackend {
    /// A handle that reaps this backend from another thread. Default: a
    /// no-op canceller, for a backend with nothing to reap.
    fn canceller(&self) -> Box<dyn BackendCanceller> { /* no-op */ }
}
```

- `CodexAgentBackend::canceller` kills the app-server child process. Whatever
  the concrete mechanism (an `Arc` to the child's kill handle, a shutdown on
  the transport), it must be reachable while the cycle thread holds
  `&mut backend`. If the current `driver.rs` structure does not permit that
  without restructuring the session, report it and implement the closest
  honest thing — do NOT fake a canceller that returns without reaping.
- The default no-op is correct for a backend with no process, and is what
  keeps `replay` and any future in-process backend compiling.

### A6. `MockBackend` supports N concurrent cycles, deterministically

Tests must control completion ORDER without sleeps. Add one scheduling
primitive — this is seam-level timing, NOT protocol behavior, so it does not
violate the "the mock implements the seam, never the protocol" rule in
`tidepool-agent/CLAUDE.md`. Say that in the doc comment.

```rust
/// A test-controlled gate: a scripted turn can BLOCK at a stop until the test
/// releases it, or until the backend is cancelled. Scheduling, not protocol —
/// it is how a test pins completion ORDER without a sleep.
pub struct MockControl { /* Mutex<{ releases: usize, cancelled: bool }> + Condvar */ }
impl MockControl {
    pub fn release(&self);          // let one blocked step through
    pub fn cancel(&self);           // every blocked and future step fails
}

pub enum MockStep {
    Calls { .. }, Completes(..), Fails(..),
    /// Block until `MockControl::release`, then continue to the NEXT step.
    /// A `cancel` while blocked returns `RunFailed { detail: "cancelled" }`.
    Blocks,
}

impl MockBackend {
    pub fn control(&self) -> Arc<MockControl>;
}
impl AgentBackend for MockBackend { fn canceller(&self) -> Box<dyn BackendCanceller> { /* the control */ } }
```

`MockBackend` must be `Send` (it already is in practice) so it can be moved to
a cycle thread. Every existing `MockBackend` constructor and every existing
assertion keeps working unchanged.

### A7. Lane A tests (`tidepool-agent/tests/spawn_saga.rs`)

- The existing "a second begin while one runs is refused" test is now WRONG.
  Replace it with its opposite: two `begin`s under one spawner both succeed,
  both agents appear in `running_agents()`, and answering either one drives
  only that one. Keep the misroute rows (answering the wrong agent / the wrong
  call) — those stay refusals and are more load-bearing than ever.
- Add: three detached sagas driven on three threads against three
  `MockBackend`s, released in an order different from the spawn order,
  completing in RELEASE order. Assert three distinct worktrees, three distinct
  bindings, all settled `Terminal`.
- Add: a saga blocked on `MockStep::Blocks`, cancelled from another thread;
  the blocked call returns, `abandon()` settles `Released`, and the worktree is
  still registered (retain-first). Assert the binding row is NOT left `Active` —
  that is the whole point of settling rather than only killing.
- Add: `abandon()` on an already-settled saga (completed, and rolled back) is
  a no-op returning `Ok(())`, so a cancel racing a completion cannot double-write.
- Every existing row in this file must still pass.

---

## Lane B — the wire and Haskell contract

### B1. `tidepool-bridge-effects`

```rust
/// Haskell `CycleId` — the handler-scoped identity of one running cycle.
/// Opaque: Tidepool mints it, echoes it, and never parses it. Cycle-scoped
/// like every PRD 19 handle — it never crosses a resident-cycle boundary.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
#[core(name = "CycleId")]
pub struct AgCycleId { pub raw: i64 }
```

In the `Ag*` section, following `AgAgentId` exactly. Field ORDER is the wire
contract; a one-field newtype has nothing to get wrong, but keep it in the
section and keep the `#[core(name)]`.

### B2. `tidepool-mcp/src/effect_defs.rs` — `subagent_effect_def!`

**Everything APPENDS.** Field order and constructor order are the wire
contract; nothing existing moves.

- `type_defs` += `"data CycleId = CycleId Int deriving (Show, Eq)"`.
- `errors SpawnError` += a LAST constructor:

  ```
  { ctor SpawnCapacityExhausted, fields { capacityLimit: "Int" as i64 },
    doc "the handler's cycle table is full — a BOUND, not a queue: a spawn past
         the cap is refused immediately so an operator sees the ceiling instead
         of an unbounded backlog forming behind it" },
  { ctor SpawnCancelled, fields { cancelledCycle: "CycleId" as tidepool_bridge_effects::AgCycleId },
    doc "the cycle was cancelled before it produced a result — the terminal an
         await on a cancelled handle resolves to. A distinct constructor rather
         than a drive failure: 'I cancelled this' and 'this broke' call for
         different handling, and an author who raced their own cancel against
         their own await must be able to tell them apart by case, not by
         reading a string" },
  ```

- `verbs` += three, LAST, in this order:

  ```
  { ctor SubagentSpawnAsync, method subagent_spawn_async,
    args { spec: "SpawnSpec" as tidepool_bridge_effects::AgSpawnSpec,
           schema: "Value" as crate::effect_glue::JsonArg },
    ret "CycleId", errors SpawnError },
  { ctor SubagentAwait, method subagent_await,
    args { cycle: "CycleId" as tidepool_bridge_effects::AgCycleId },
    ret "SpawnOutcome", errors SpawnError },
  { ctor SubagentCancel, method subagent_cancel,
    args { cycle: "CycleId" as tidepool_bridge_effects::AgCycleId },
    ret "()" },
  ```

  `SubagentCancel` is TOTAL — no `errors` block. Cancelling an already-terminal
  or unknown handle is a no-op, per the PRD. `SubagentAwait` returns the SAME
  `Either SpawnError SpawnOutcome` the sync path returns; the `(outcome, r)`
  tuple is assembled Haskell-side by the existing `decodeOutcome`.

  (If the `verbs` grammar has no spelling for a unit return, use the closest
  existing precedent in this file rather than inventing one, and say which in
  the lane report.)

- `helpers` += `agentSpawnAsyncRaw` / `agentAwaitRaw` / `agentCancelRaw`,
  each with a doc comment in the voice of the existing three, each pointing at
  the typed wrapper in `Tidepool.Agent.Spawn`:

  ```haskell
  agentSpawnAsyncRaw :: SpawnSpec -> Value -> M (Either SpawnError CycleId)
  agentAwaitRaw      :: CycleId -> M (Either SpawnError SpawnOutcome)
  agentCancelRaw     :: CycleId -> M ()
  ```

- `renderSpawnError` += the `SpawnCapacityExhausted` arm.
- The `description` block gains a sentence on the async trio, in its voice.

### B3. Stub handler methods (so the workspace compiles in wave 1)

The generated dispatch calls `subagent_spawn_async` / `subagent_await` /
`subagent_cancel`, so `tidepool-handlers/src/handlers/agent.rs` needs the three
inherent methods to exist. Lane B adds them as SHORT stubs returning
`SpawnError::SpawnDriveFailed(AgSpawnStage::StageAllocating, "…")` (and `()`
for cancel), each with a one-line comment saying lane C fills it in. Lane C
replaces the bodies. Lane B changes NOTHING else in that file.

### B4. `haskell/lib/Tidepool/Agent/Spawn.hs`

```haskell
-- | An opaque, cycle-scoped handle to a running agent, phantom-typed by the
-- result the agent was spawned to produce.
--
-- The phantom @r@ is what makes 'awaitAgent' need no type application: the
-- schema the child is held to was fixed at 'spawnAsync', and the handle
-- carries that choice to the await. A handle never crosses a resident-cycle
-- boundary (PRD 19's rule for every runtime handle) — what crosses is the
-- recorded outcome.
newtype AgentHandle r = AgentHandle CycleId

spawnAsync  :: forall r. JsonSchema r => SpawnSpec -> M (Either SpawnError (AgentHandle r))
awaitAgent  :: forall r. FromJSON r => AgentHandle r -> M (Either SpawnError (SpawnOutcome, r))
cancelAgent :: AgentHandle r -> M ()

spawnAgent :: forall r. (FromJSON r, JsonSchema r) => SpawnSpec -> M (Either SpawnError (SpawnOutcome, r))
spawnAgent spec = spawnAsync @r spec >>= either (pure . Left) (awaitAgent @r)
```

- `spawnAgent`'s type signature is UNCHANGED, character for character.
- **The rederivation is lane C's, not lane B's.** Lane B's handler methods are
  stubs, so a `spawnAgent` routed through `SubagentSpawnAsync` would return
  `Left (SpawnDriveFailed …)` and turn
  `tidepool-handlers/tests/subagent_one_cycle.rs` — which drives
  `spawnAgent @WorkerResult` through the real extract/JIT — red from lane B's
  fold until lane C lands. So lane B lands the async surface with `spawnAgent`
  still bodied as `spawnAgentWithTools @NoTools @r (ToolRounds 0) NoTools`,
  and lane C flips that one line together with the bodies that make it true.
  A lane that hands up a knowingly-red file spends the fold's attention on a
  failure everyone already understands, and buries the next lane's real
  regressions in the noise. There is still exactly ONE spawn implementation at
  every moment — just the tool-loop one until C lands.
- `awaitAgent` reuses the existing `decodeOutcome` — `SpawnResultMalformed` is
  still produced in exactly one place.
- `spawnAgentWithTools` is UNTOUCHED: a child that calls back must be driven
  stop-by-stop through `agentBeginRaw`/`agentResumeRaw`, which the async path
  deliberately does not do (an async cycle is the no-tools combinator, same
  saga).
- Export `AgentHandle` as an ABSTRACT type — the constructor is not exported;
  a `CycleId` an author could forge is not a handle.
- Module docs: the "__Scope.__" paragraph currently says the async/`waitAgent`
  surface "is later work and is deliberately not here." Rewrite it — it is
  here now. Note in one sentence that `spawnAgent` IS `spawnAsync` +
  `awaitAgent` so there is no second implementation, mirroring how it is
  already the zero-tools case of the tool loop.
- One behavioral note worth writing down at the call site: on the async path a
  tool call from a child that declared no tools is refused by the RUNTIME
  (`spawn_one_cycle`'s "no such tool: X — this agent was created with no
  dynamic tools") rather than by the Haskell loop ("no such tool: X"). Same
  outcome, different text, in a path that only fires if a toolless child
  invents a tool.

### B5. `tidepool-harness/src/engine.rs`

Extend the existing Subagent classify arm — the only change in that crate:

```rust
Some("SubagentSpawn") | Some("SubagentBegin") | Some("SubagentResume")
| Some("SubagentSpawnAsync") | Some("SubagentAwait") | Some("SubagentCancel") => ...
```

and the `HoleRouting::Subagent` doc comment's constructor list. **Do not open
`tidepool-harness/src/selfharness/driver.rs`.**

---

## Lane C — the cycle table in `SubagentHandler`

### C1. The table

```rust
/// One running cycle, keyed by the `CycleId` an authored `AgentHandle` wraps.
enum Cycle {
    /// Driven one stop at a time by `SubagentBegin`/`SubagentResume`, inline
    /// on the caller's thread — the tool-dispatch path, whose loop lives in
    /// Haskell.
    Stepped { saga: CycleSaga, backend: Box<dyn AgentBackend + Send> },
    /// Driven to completion on its OWN thread — the async path. Awaited or
    /// cancelled; never stepped.
    Async(AsyncCycle),
}
```

- Key: a `CycleId(u64)` newtype minted by the handler, monotonic from 0.
- `AsyncCycle` holds the join handle, a result receiver, the canceller taken
  from the backend BEFORE the thread started, and a memo slot for the received
  result (so a second `await` on the same handle is not a lost-receiver panic —
  decide and document what a second await returns; the natural answer is the
  same memoized result).
- **Capacity.** `with_cycle_capacity(n)`, default 8, counted over NON-terminal
  entries. A spawn past the cap is `SpawnError::SpawnCapacityExhausted { capacityLimit }`
  — refused immediately, never queued.
- The single `backend` field is replaced by a `Box<dyn AgentBackendFactory>`.
  Keep `SubagentHandler::new(...)`'s current signature working by wrapping the
  passed `Box<dyn AgentBackend + Send>` in a ONE-SHOT factory (it yields that
  instance for the first cycle and then fails `BackendUnavailable`, naming the
  wiring) — that keeps all five existing call sites (`live_tool_loop.rs`,
  `subagent_tool_loop.rs`, `subagent_one_cycle.rs`, `outer_subagent.rs`,
  `tidepool-web/src/bin/tidepool-selfharness.rs`) compiling untouched. Add
  `SubagentHandler::with_backends(registry_root, worktree_root, binding_root,
  source_repository, factory)` as the N-cycle constructor. The one-shot
  factory's exhaustion is a fact about the WIRING, not a policy refusal — say
  that in its doc comment so it is never mistaken for the constraint this lane
  deleted.
- `backend_transcript_jsonl()` today reads the single backend. Redefine it over
  the table (concatenate live cycles' transcripts in cycle order) and document
  the change; the live acceptance in `live_tool_loop.rs` is its only caller.

### C2. The verbs

- `subagent_spawn_async(spec, schema) -> Result<AgCycleId, SpawnError>`:
  capacity check → make a backend from the factory → take its canceller →
  `spawner.begin_detached` is NOT called here (it blocks); instead the whole
  saga runs on the spawned thread, which sends `Result<AgSpawnOutcome, SpawnError>`
  down the channel. Returns the minted `CycleId` immediately. A factory failure
  is `SpawnBackendFailed(StageAllocating, …)`.
- `subagent_await(cycle) -> Result<AgSpawnOutcome, SpawnError>`: look up,
  block on the receiver, join the thread, memoize, mark terminal. Unknown or
  already-reaped handle → `SpawnDriveFailed(StageRunning, "no such cycle …")`,
  which is exactly that variant's documented meaning (the caller sequenced the
  loop wrongly; the backend did nothing).
- `subagent_cancel(cycle)`: canceller → join → `saga.abandon()` (settle
  `Released`, mutex taken briefly AFTER the kill) → mark the entry terminal
  with a cancelled result. TOTAL: unknown or terminal is a no-op. Never
  panics, never blocks forever, and never leaves a binding row `Active`.

**Cancel/await races resolve to typed terminals (root, hold 2).** The two
verbs can arrive in either order against the same handle, and neither order
may hang or panic:

| sequence | `await` returns | `cancel` does |
|---|---|---|
| cancel, then await | `SpawnCancelled { cancelledCycle }` | reaps + settles |
| await, then cancel | the real outcome or error | no-op (terminal) |
| cancel, then cancel | — | no-op (terminal) |
| await, then await | the SAME memoized result | — |
| cancel/await on an unknown id | `SpawnDriveFailed(StageRunning, "no such cycle …")` | no-op |

A cancelled cycle is therefore RETAINED in the table as a terminal entry
carrying `SpawnCancelled`, not dropped — dropping it would make a subsequent
await indistinguishable from a typo'd handle, which is the one thing the
`SpawnDriveFailed` spelling is supposed to mean. Entries are reclaimed when
the handler is dropped.
- **Flip `spawnAgent`.** With the bodies real, rewrite
  `haskell/lib/Tidepool/Agent/Spawn.hs`'s `spawnAgent` to
  `spawnAsync @r spec >>= either (pure . Left) (awaitAgent @r)`, delete lane
  B's one-line comment marking the pending rederivation, and add the sentence
  to the module docs that `spawnAgent` IS `spawnAsync` + `awaitAgent` —
  mirroring how it is already the zero-tools case of the tool loop. This is
  the ONE Haskell edit lane C makes; the rest of that file is lane B's and is
  already correct. `tidepool-handlers/tests/subagent_one_cycle.rs` is the gate
  that this flip preserved behavior — it must be green before and after.
- **Rewrite `tidepool-handlers/CLAUDE.md`'s "three verbs, one saga, one
  running agent" section.** Its "One agent at a time" bullet documents the
  constraint this lane deletes, and its heading counts verbs that are now six.
  Lane B deliberately left it alone rather than half-updating it — it
  describes behavior lane C changes, so lane C owns it. State the new shape:
  the cycle table, per-cycle backends, the capacity bound, and what cancel
  settles. Keep the two consequences that are still true (model tier is
  handler configuration; the handler owns the backend and dropping it is what
  bounds a parked child).
- `subagent_spawn` / `subagent_begin` / `subagent_resume` keep their EXACT
  current behavior. `subagent_begin` now inserts a `Stepped` entry;
  `subagent_resume` looks the saga up by agent id. Every existing assertion in
  the file's `mod tests` and in `tidepool-handlers/tests/subagent_*.rs` must
  pass unchanged.

**Blocking.** `EffectHandler::handle` is synchronous and today's
`subagent_spawn` already blocks for a whole cycle, so blocking in `await` is no
new hazard — do not add a tokio dependency or an async seam for it. Write one
sentence in the method doc saying so.

### C3. Lane C tests

Join the EXISTING subagent test families rather than adding per-test extract
compiles (root `CLAUDE.md`, "Suite wall time is a standing constraint"). The
handler-level rows belong in `agent.rs`'s own `mod tests` (pure Rust, no
extract); anything that must go through the JIT joins
`tidepool-handlers/tests/subagent_one_cycle.rs`'s existing bundle.

Named rows, all on `MockBackend` with `MockControl` (no sleeps, no timing
assumptions):

1. **Three in flight.** Three `spawnAsync` calls under one handler; all three
   backends report a started thread before any completes.
2. **Await order ≠ spawn order.** Release cycle 2, then 0, then 1; await in
   spawn order and assert each gets ITS OWN payload — completion order is not
   an input to any result.
3. **Cancel reaps.** A cycle blocked on `MockStep::Blocks`; `cancel` returns,
   the thread is joined, the binding is settled `Released`, and the worktree is
   still registered.
4. **Cancel is total, and the races are TABLE-DRIVEN.** One table over the
   five rows in the cancel/await matrix above — every sequence asserted to
   reach its typed terminal, none of them hanging and none of them panicking.
   Cancelling an already-awaited cycle and an unknown `CycleId` are rows in
   that table, not separate tests.
5. **Table full is typed.** Capacity 2, three spawns; the third is
   `SpawnCapacityExhausted { capacityLimit: 2 }` — and nothing was allocated
   for it (no extra worktree registered, no extra binding row), asserted the
   way `handler_rejects_path_unsafe_existing_id_before_touching_disk` asserts
   it.
6. **The sync path is unchanged.** The existing
   `handler_spawn_happy_path_maps_outcome_to_wire` and the begin/resume rows
   pass untouched.

No live-Codex test outside the `TIDEPOOL_AGENT_LIVE` gate. No test may assume
thread scheduling order beyond what `MockControl` pins.

---

## Standing rules this lane must not break

- **No second spawn implementation.** `spawn_one_cycle`, `spawnAgent`, and the
  async path are all combinators over ONE saga.
- **A sum result type at the agent boundary is forbidden.** Single-constructor
  records only (`tidepool-agent/CLAUDE.md`, `outputSchema` section).
- **Retain-first.** Cancellation settles a binding; it never deletes a
  worktree, branch, ref, or record.
- **Wire field/constructor order is contract** — every widening appends.
- **Do not run bare `scripts/battery.sh`.** The environment kills at ~380s.
  Use `cargo nextest run` (fast tier) and
  `scripts/battery.sh -p <crate> -E 'test(<name>)'` (targeted).

## Verification

```
cargo check --workspace
cargo nextest run
scripts/battery.sh -p tidepool-handlers \
  -E 'binary(subagent_one_cycle) + binary(subagent_tool_loop) + test(handler_)'
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

**`-E 'test(subagent)'` selects ZERO tests** — `subagent_one_cycle` and
`subagent_tool_loop` are BINARY names, not test names, and the handler's own
rows are named `handler_*`. `battery.sh` refuses a zero-test run rather than
reporting a false pass, which is the only reason this was caught rather than
banked as a green tier. Use the filter above, and check the reported test
COUNT against what you expected to run — a filter that silently matches
nothing is the failure mode this line exists to prevent.
