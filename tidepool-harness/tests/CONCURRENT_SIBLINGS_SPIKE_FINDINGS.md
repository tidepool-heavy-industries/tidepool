# Spike findings: `hyloConcurrentM` blocked on driver-side Subagent servicing

Spec: add `Swarm.hyloConcurrentM` (concurrent sibling driving for
`Tidepool.Swarm.hyloM`) so multi-child dev-tree runs stop paying linear wall
time. Spec required verifying, BEFORE writing the combinator, that the green
scheduler actually supports N concurrently-parked `Subagent` awaits from
async green threads — dev-tree's `decompose`/`integrate` do real work
(`spawnAgent`) inside each child, so that is the workload the combinator
exists to speed up.

**Result: it structurally cannot, for the one suspension kind dev-tree's
children actually spend wall time on.** No Haskell was written; per the
spec's own instruction this is a findings report instead of a combinator.

## What the green scheduler DOES support

The FIFO ready queue and thread table (`tidepool-harness/src/selfharness/driver/green.rs`)
have no artificial limit on how many threads can be simultaneously PARKED —
`GreenReady` items for distinct `GreenChain::Thread(tid)`s coexist in `ready`/
`waiters` freely, and `mapConcurrently`/`forConcurrently` (`Tidepool.Async`)
correctly start N threads before the first `wait`.

**`Fork`-routed suspensions are the one kind that is actually driven
concurrently once several are ready at once:**
- `tidepool-harness/src/selfharness/driver/green.rs:896-941` (inside
  `service_green_round`) drains every currently `Fork`-routed ready item out
  of `green.ready` into a batch and drives the WHOLE batch through
  `drive_fork_ready_batch` (`fork.rs:1006-1150`), which admits each item
  against the fork budget then runs them all via `drive_concurrent`
  (`fork.rs:106-123`, `buffer_unordered` + re-sort to declaration order).
- `tidepool-harness/src/selfharness/driver/lifecycle.rs:1448-1478` (the
  AUTHORED outer loop's own `run_loop_fragment_inner`) routes
  `runLLMTurnFork`/`runLLMTurnFanout` through `service_outer_fanout`
  (`fork.rs:424-515`), which likewise drives every child concurrently via
  `drive_concurrent` (`fork.rs:471`).
- Confirmed independently by `tidepool-harness/tests/answerer_async_fork.rs`'s
  own module doc: "Fork children now overlap (the green scheduler drives
  every currently fork-ready thread CONCURRENTLY, not one at a time)" — i.e.
  this was previously sequential and was deliberately fixed for `Fork`.

`drive_concurrent` (`fork.rs:106-123`) is the ONE concurrency shell in this
driver. Its only three call sites — `fork.rs:471`, `fork.rs:1083`
(`drive_fork_ready_batch`), and `fork.rs:1331` (`drain_answerer_fork`'s direct
`fork`/`forkAll` batching) — all drive `Fork`-classified suspensions only.

## What it does NOT support: `Subagent`

`Subagent` (`spawnAgent`/`spawnAgentRaw`) is a suspension in this driver (not
a JIT-handled effect here — see `tidepool-agent`'s own row for the
standalone-test shape where it IS a handled effect); the resident harness
services it externally via `SelfHarnessDriver::service_outer_subagent`
(`tidepool-harness/src/selfharness/driver/delegate.rs:81-133`):

```rust
let dispatched =
    tokio::task::block_in_place(|| Self::dispatch_outer_effect(handler, request, table));
```

`block_in_place` runs the dispatch to completion — a real
`tidepool_agent::spawn::CoupledSpawner` cycle (spawn a subprocess agent,
drive it, await its terminal state) — SYNCHRONOUSLY, before returning
`Ok(Value)`/`Err`. It is called from exactly two places, and both call it
**one ready item at a time, with no batching shell around it**:

- `tidepool-harness/src/selfharness/driver/lifecycle.rs:1283-1301` — the
  AUTHORED outer loop's own ready-queue loop (`run_loop_fragment_inner`) pops
  ONE `GreenReady` off the FIFO queue per iteration; its `Subagent` arm calls
  `service_outer_subagent` directly and only then continues the loop to pop
  the next item (which may be a sibling green thread's own pending `Subagent`
  request, already sitting in `ready` as inert data the whole time).
- `tidepool-harness/src/selfharness/driver/green.rs:1163-1297`
  (`service_thread_ready`, the answerer-plane sibling scheduler) — reached
  only for a popped item that is NOT `Fork`-routed (`Fork` was already
  drained into the batch above); its `Subagent` arm (`green.rs:1260-1278`)
  is the same single synchronous call.

Both loops are single-threaded dispatch loops over one `VecDeque<GreenReady>`:
there is no `tokio::spawn`/`buffer_unordered` anywhere near either `Subagent`
arm, unlike every `Fork` call site above.

**Consequence for `hyloConcurrentM`:** wrapping dev-tree's `PlanF` children in
`Tidepool.Async` threads (`mapConcurrently go children`) would start all N
child threads, and all N would legitimately reach and park on their own
`Subagent` suspension around the same logical moment (dev-tree's
`decompose`/`integrate` call `spawnAgent` for the real implementation work).
But the driver then services those N parked `Subagent` requests **one fully
to completion before even looking at the next** — the exact same total
ordering and wall time as today's sequential `traverse go`, just reached via
more machinery. The combinator would compile, pass an order-preservation
property test, and even pass a "two concurrent children" integration test
(nothing errors; both children DO finish, in the right order) — while
delivering **zero** wall-clock improvement on the one workload the spec
motivates this with. Shipping it un-flagged would be a green combinator that
silently doesn't do the thing it's for.

## Smallest driver change that would enable it

Generalize the `Fork` batching pattern to `Subagent` (and, if useful, the
other `OuterEffect` kinds that are safe to overlap — `Console`/`Worktree`/
`Exec`), in both places `Fork` already gets it:

1. `green.rs:896-941` — widen the drain-into-batch predicate from
   `matches!(c.routing, SuspensionRouting::Fork { .. })` to also catch
   `SuspensionRouting::Subagent`, and extend `drive_fork_ready_batch`
   (or add a sibling `drive_subagent_ready_batch`) to dispatch through
   `service_outer_subagent` per item via `drive_concurrent`.
2. `lifecycle.rs`'s `run_loop_fragment_inner` — before popping a single ready
   item, drain every currently-ready `Subagent`-routed item out of `ready`
   the same way `service_green_round` already drains `Fork` ones, and drive
   the batch concurrently (mirroring `service_outer_fanout`'s shape, but
   sourced from already-produced `ready` items rather than issuing N fresh
   children itself).
3. `service_outer_subagent`'s `tokio::task::block_in_place` would need to
   become `tokio::task::spawn_blocking` (or the handler call moved onto
   `drive_concurrent`'s own future) — `block_in_place` pins the *calling*
   worker thread for the call's duration, which is fine for one call at a
   time but does not compose with running several overlapping instances
   through `buffer_unordered`.

Both sites live in `tidepool-harness/src/selfharness/driver/{lifecycle,green}.rs`,
outside this spec's ALLOWED PATHS (`haskell/lib/Tidepool/Swarm.hs`,
`haskell/test`, `tidepool-harness/tests`, `tidepool-testing`) — this is
driver-team follow-up work, not something this spec could implement even if
it tried.

## What was deliberately NOT done

No changes to `haskell/lib/Tidepool/Swarm.hs` (`hyloM` untouched, no additive
combinator added), no new property tests, no new GHC-heavy integration test.
Per the spec: "if it structurally cannot, STOP and submit a findings report
... instead of forcing a broken combinator through."

## Addendum (driver-concurrent-subagent lane): the smallest change, implemented

The three-part change this doc named above is now implemented, in the two
files it named (`lifecycle.rs`'s `run_loop_fragment_inner`, `green.rs`'s
`service_green_round`/`service_thread_ready`), plus `delegate.rs`'s
`service_outer_subagent` (now `async`, dispatching via
`tokio::task::spawn_blocking` instead of `tokio::task::block_in_place` — the
composability fix part 3 asked for) and `mod.rs` (`Self::subagent` is now
`Arc<Mutex<Option<SubagentHandler>>>` so the dispatch closure can clone a
`'static` handle into `spawn_blocking`).

**The Sync/Send story, checked honestly (this doc's own ask).** `SubagentHandler`'s
`SubagentSpawn`/`SubagentAwait` methods take `&mut self` for their FULL
synchronous duration (`tidepool_agent::spawn::CoupledSpawner::spawn_one_cycle`'s
own signature is `&mut self` end to end, even though its BODY only touches
`&self.substrate` — an `Arc<Mutex<SpawnSubstrate>>` `tidepool-agent/CLAUDE.md`
already documents as safe for N concurrent cycles). Since the driver holds
exactly ONE `SubagentHandler` instance behind ONE lock, two dispatches that
BOTH need the full `SubagentSpawn`/`SubagentAwait` call cannot literally
overlap at the Rust type level — narrowing that requires a `tidepool-agent`/
`tidepool-handlers` signature change, out of this lane's ALLOWED PATHS. This
is NOT papered over: it is the literal reason `service_outer_subagent`'s own
doc comment states plainly that dispatch "still serializes on ONE lock."

**Real overlap is achieved anyway, because `spawnAgent` is not one call.**
`haskell/lib/Tidepool/Agent/Spawn.hs`: `spawnAgent spec = spawnAsync spec >>=
either (pure . Left) awaitAgent` — two SEPARATE suspensions, not the
single blocking `SubagentSpawn` this doc's original text described.
`SubagentSpawnAsync` (`tidepool-handlers/src/handlers/agent.rs`'s
`subagent_spawn_async`) touches `&mut self` only BRIEFLY (admit the cycle,
mint a backend, `std::thread::spawn` the actual agent conversation onto a
DETACHED thread carrying its own substrate handle) before releasing the
lock — the slow work runs OUTSIDE the handler's exclusive borrow entirely.
Batching N siblings' `spawnAsync` calls concurrently lets each kick off its
own background cycle thread in quick succession; each sibling's LATER
`awaitAgent` still queues on the one lock, but by then its own background
work has typically already been running for as long as its sibling's, so the
batch's total wall time approaches `max` of the cycles rather than their
`sum`. Pinned by `tidepool-harness/tests/outer_subagent.rs`'s
`outer_loop_two_concurrent_spawn_agent_calls_overlap_in_wall_time`
(`DelayingBackend` + `max_concurrent()`, the same receipt
`answerer_async_fork.rs` uses for Fork).

**One real difference from Fork's own drain trigger, worth recording.** Fork's
`async (fork …)` spawn step is pure-JIT (never itself a driver suspension), so
by the time a straight-line block's own `wait` genuinely blocks, every
`fork` it already issued is guaranteed to be sitting fully parked in `ready`
— draining exactly then is correct. `spawnAsync` is NOT pure-JIT (it is a
real `Subagent` suspension needing driver dispatch), so a sibling's own
`async (spawnAgent …)` needs ITS OWN `Green` spawn step serviced first before
its `Subagent` request even exists. `green.rs`'s `service_green_round`
already has the right trigger for this for free (it re-drives one node's own
continuation via `GreenDelivery::Node` until IT genuinely blocks, so every
sibling spawn along the way is serviced before the block-and-drain point is
ever reached) — but `lifecycle.rs`'s `run_loop_fragment_inner` has no
equivalent per-chain sub-loop; it pops one flat FIFO queue mixing every
chain together. Draining as soon as ANY `Subagent` item was ready (this
lane's first attempt) fired batches of one, reproducing the very bug this
change exists to fix. The fix that shipped: gate the outer loop's batch-drain
on "EVERY item currently in `ready` is `Subagent`-routed" — deferring the
batch until every sibling's own fast/synchronous `Green` spawn step has
already run through the ordinary single-item path (each such step is
JIT-fast and never blocks, so servicing it individually costs nothing) — see
that gate's own doc comment in `lifecycle.rs` for the full trace.
