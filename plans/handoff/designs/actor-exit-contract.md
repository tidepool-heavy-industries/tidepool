# Actor exit contract: decision memo

Branch `engine/stg-production-cutover`. Scratch memo; read-only investigation, nothing built or run.

## Verified facts (source, current tree)

Exit writers, i.e. every caller of `RetainedActorExit::publish` outside tests (via `publish_terminal`, `local_actor.rs:1457`):

| # | Site | Record written | Cleanup proof retained |
|---|---|---|---|
| W1 | `finish_actor` `local_actor.rs:1451` | own | yes (`retain_cleanup` at 1443, before publish) |
| W2 | replacement fence `local_actor.rs:944` | own, `Cancelled "replaced by …"` | no. Children, resources and forgotten cleanup move to the successor first (897-921) |
| W3 | `shutdown_children` timeout/err arm `local_actor.rs:1566-1570` | child's, owner's requested kind | no, then `kill()` |
| W4 | `handle_supervisor_evt` `local_actor.rs:1255-1257` | child's, `Failed` (child died without publishing) | no |

`termination.rs` lets `publish` return `ActorExitAlreadyPublished` (first write wins). `publish_paused` never overwrites an exit. The docs call the "actor lifecycle owner" the only writer they expect, but nothing enforces that.

**Q1 (pause while draining) is confirmed.**
- `Drain` (`local_actor.rs:972-994`): `begin_drain`, then `close_with_fence`, then `Fencing`, then `DrainFence`, then `Draining`. Mailbox admission is closed.
- A queued `Cast`/`Call`/`Source`/`Resume` that fails calls `fail_handler` (1354), which calls `ResidentActor::pause_failed_handler` (`resident_actor.rs:5347`). The actor pauses when a checkpoint and an active input exist. Standing becomes `Paused`, `publish_paused` runs, and the supervisor is notified.
- After that `accepts_mailbox()` is false (5294). `DrainMailbox` returns early (796) and the drain step (1198-1210) needs `accepts_mailbox()`, so neither ever fires again.
- `begin_drain` rejects `Paused` (5298-5308). Nothing more happens until `Shutdown` or `Replace` (replacement accepts `Paused`: `replacement.rs:262`). `ActivateReplacement` carries `draining`.
- Inconsistency: when the drain continuation itself fails (1207), no active input is set, so pause returns false and the actor fails. A failing queued message pauses instead.
- Haskell: `drainActor` returns once the drain request is accepted (`resident_actor.rs:2629-2650`), not when the actor exits. `awaitExit` and `pollExit` cannot see a pause; only the `Tidepool.Actor.Source` lifecycle source sees `ActorPaused` (`resident_workbench.rs:3913`). An owner doing `drainActor r >> awaitExit r` therefore parks silently forever.

**Q2 (cleanup rewrites the exit kind) is confirmed.**
- `finish_actor` 1436-1442: if the hook or realm cleanup is `Unconfirmed`, the requested exit becomes `Failed "actor shutdown failed: …"`. The same outcome is also kept in `retain_cleanup`, so the information is stored twice.
- A completed actor has already filled its Haskell exit cell (`Actor.hs:388-392`). The rewrite hides that value, and supervisors see `Failed`.
- `run_shutdown` (`resident_workbench.rs:3796`) waits at most 30s to check out the machine (`checkout_wait`). Once admitted, the hook runs with no deadline (`spawn_blocking`).
- `retire_root_placement` and `close_realm` (5026-5065) use `with_host_machine(.., None)`, i.e. `checkout_queued`, which has no deadline.
- Timeouts don't line up: the parent's per-child timeout is 15s (1431), but the child's own hook admission waits up to 30s. A parent can kill a child partway through a cleanup that would have been confirmed. W3 then publishes on the child's behalf with no cleanup proof.
- `AbortReplacement` (823-833) already treats `cleanup().is_confirmed()` as a separate fact from the exit kind.

**Does an unavailable machine make checkout fail fast?** It depends on why it is unavailable (`tidepool-runtime/src/session/registry.rs:300-350`):
- **Unknown, removed or terminal session:** fails immediately (`CheckoutError::Unknown`/structural). The error becomes `Unconfirmed` realm cleanup.
- **Busy (`Running`):** `checkout_queued` waits on FIFO notifications with no bound. So `close_realm`/`retire_root_placement` can hang `finish_actor` indefinitely before the exit is published. Only the hook's admission is bounded (30s).

---

## Q1: a handler fails after a drain was requested

**Option A: keep the pause as-is and document it.**
- **Owner:** `awaitExit` parks until someone replaces or stops the actor. The owner has to subscribe to the lifecycle source to notice.
- **Supervisors:** get a text notification only.
- **Retirement:** nothing happens.
- **Haskell:** no API change, but `drainActor >> awaitExit` is a silent liveness trap.

**Option B: once a drain has been requested, a handler failure is terminal.** In `fail_handler`, if `state.drain != Open`, skip the pause and call `fail_actor`.
- **Owner:** `awaitExit` returns `Failed "actor cast failed: …"` straight away.
- **Supervisors:** `child_exited` sees `Failed`; restart policy applies.
- **Retirement:** normal `finish_actor`. The shutdown hook runs with `Failed`; the deferred backlog is dropped and pending calls get their reply channels dropped. Receipts carry real cleanup proof.
- **Haskell:** no API change. `drainActor` now guarantees that an exit will eventually be published. Replacing a paused actor still works if it paused *before* the drain; `begin_drain` keeps rejecting `Paused` with an explicit error.
- **Cost:** a draining actor can no longer be rescued in place by replacement.

**Option C: pause, but make drain re-requestable and add a pause deadline that escalates to failure.**
- **Owner:** eventually gets `Failed`, after a wall-clock delay.
- **Cost:** adds a timer mechanism to the kernel. Timeouts already belong to `TurnSupervisor` (mechanism index), so this would copy that mechanism. It also adds a "retry drain" that changes nothing.

**Recommendation: B.** Drain is the owner saying "finish what you accepted, then exit." Pausing exists so a state can be recovered by replacement, and a caller who wants that should replace before draining. B makes a failing queued message behave like a failing drain continuation (which already fails). It removes the only path where the owner parks with no exit and no error, and it needs no new mechanism or Haskell change. Document on `drainActor`: "after drain, a handler failure ends the actor as `Failed`."

## Q2: does cleanup affect the exit kind?

**Option A: status quo, unconfirmed cleanup turns the exit into `Failed`.**
- **Owner:** `Completed v` is lost; `awaitExit` returns `Failed` even though the exit cell holds `v`.
- **Supervisors:** a restart policy may redo work that already completed (duplicate side effects).
- **Receipts:** the cleanup record says the same thing again.
- **Haskell:** `ActorExit` mixes "what the actor did" with "what the host could confirm."

**Option B: the exit kind is the kind that was requested. Cleanup is a separate fact, retained before publication.**
- **Owner:** `awaitExit` returns what the actor actually did.
- **Supervisors and retirement:** read `RetainedActorExit::cleanup()`, which is already retained atomically-before-publish, through `child_exited` / `ResidentShutdown.cleanup` / `retirement_acknowledged_by`. That matches what `AbortReplacement` already does.
- **Haskell:** `ActorExit` is unchanged. Cleanup uncertainty stays a host and operator fact, reported through the supervisor notice and the retirement receipt. If authored code ever needs it, add `exitCleanup :: ActorRef api exit -> Eff effs (Maybe CleanupStatus)` later; no ADT change is needed now.

**Option C: add a cleanup field to `ActorTerminal` and the Haskell `ActorExit`** (e.g. a `CleanupUnconfirmed` wrapper).
- **Cost:** every `awaitExit`/`pollExit` consumer has to handle it. Breaks the generated effect surface, fixtures and every authored harness, for a fact most authors can't act on.

**Recommendation: B.** Kind comes from the actor and the cleanup outcome comes from the host. Both are already stored, so the rewrite at 1436-1442 is the only thing to delete. Pair it with **one shutdown deadline**:
- `finish_actor` computes `deadline = now + budget` once.
- It passes that deadline to `shutdown_components` (hook admission *and* realm/placement checkout use `checkout_wait` up to the deadline, so a busy machine gives `WaitTimeout`, which becomes `Unconfirmed`) and to each child (the child gets `deadline - margin`).
- The parent's W3 timeout is the parent's own deadline, so the child's budget always expires first and reports `Unconfirmed` itself instead of being killed.
- Result: a busy machine can no longer delay exit publication without bound, and a removed or terminal machine still fails fast.

---

## Single publication owner

**Function:** rename `publish_terminal` to `publish_exit`. It becomes the only caller of `RetainedActorExit::publish` in the crate (enforce by making `publish` `pub(crate)` or test-only). Signature:

```rust
fn publish_exit(retained: &RetainedActorExit, terminal: &ActorTerminal, authority: ExitAuthority<'_>)
enum ExitAuthority<'a> {
    /// W1 + W2: the actor's own finish_actor. Carries the children snapshot taken after
    /// child admission closed (empty for a replaced predecessor: custody moved to the successor).
    Own { children: &'a [LocalActorRef] },
    /// W3 + W4: the direct supervisor publishes for a child whose task is gone or was killed.
    /// No cleanup proof is retained; cleanup() stays None ("forced").
    SupervisorForced,
}
```

- **W2** moves into `finish_actor`: `finish_actor(myself, state, Disposition::Stop(requested) | Disposition::Replaced { successor })`. `Replaced` skips `shutdown_children`, the hook and the realm; it asserts the transferred children map is empty and retains cleanup as confirmed-by-transfer. `Replaced` runs `replacement_retired`, `Stop` runs `stopped`.
- After that, `finish_actor` is the only publisher for an actor's own record. W3/W4 remain as `SupervisorForced` only.

**Invariant asserted in `Own`:** `children.iter().all(|c| c.terminal().get().is_some())`. That is, the owner publishes only after every child has an exit.
- The snapshot is valid because `child_admission_closed` is set before it (1430).
- `shutdown_children` already guarantees the property: success means the child published; a timeout or error goes through W3.
- Use `debug_assert!` so it panics in tests, plus `tracing::error!` in release. Don't refuse to publish, because an owner with no exit is worse.
- With Q2-B also in place, `publish_exit` asserts `terminal.kind == requested.kind` for `Own`.

## Deterministic tests (`local_actor.rs` `mod tests`)

All use `#[tokio::test(start_paused = true)]`. They record transitions with `terminal().connect_lifecycle(move |e| tx.send(e).is_ok())` into a `std::sync::mpsc` channel and check the exact `ActorLifecycle` sequence. `ProbeBehavior` has no `pause_failed_handler` override, so the tests need a small `PausingProbe` that returns true while it holds a failed input.

1. **`paused_handler_while_draining_publishes_failed_exit`** (Q1-B)
   - Queue two casts; the first blocks on a `Notify`. Call `drain()`, then release; the second cast fails.
   - The sink sees exactly `[Live, Exited(Failed "actor cast failed…")]`, with no `Paused`.
   - `terminal().wait()` resolves; `cleanup()` is `Some` and confirmed.
2. **`pause_before_drain_still_rejects_drain_and_stays_live`**
   - Fail a cast first (sink: `[Live, Paused]`). Then `drain()` returns `Rejected`, `get()` is `None`, and a later `shutdown(Cancelled)` gives `[.., Exited(Cancelled)]`.
3. **`unconfirmed_cleanup_preserves_requested_exit_kind`** (Q2-B)
   - The probe's `shutdown_components` returns `(Unconfirmed("hook"), Confirmed)`. `shutdown(Completed)`, then the sink gets `Exited(Completed)` and `cleanup().unwrap().is_confirmed() == false`. Same for `Cancelled`.
4. **`owner_exit_follows_every_child_exit`**
   - Parent spawns two children. Each child's sink pushes `(id, Exited)` into a shared `Mutex<Vec>`, and so does the parent's sink.
   - `shutdown(parent)`. Both child entries come before the parent entry in the vector.
5. **`hung_child_is_forced_before_owner_publishes`** (W3)
   - The child's `shutdown_components` awaits a never-notified `Notify`. Shut down the parent; the paused clock advances past the deadline.
   - The child sink gets `Exited(Cancelled "owner actor stopped")` with `cleanup() == None`, then the parent sink gets its exit. Parent `cleanup().children` is `Unconfirmed`.
6. **`busy_machine_cannot_delay_exit_past_deadline`** (only once cleanup uses the deadline)
   - The probe's realm cleanup awaits `tokio::time::sleep(Duration::MAX)` wrapped by the deadline. Shut down; with the paused clock the exit is published at exactly the deadline, and the realm is `Unconfirmed`.
   - The resident-machine version (a real `checkout_wait` on a `Running` session) belongs in the resident actor suite, not this unit.
7. **`replacement_fence_publishes_through_finish_actor`**
   - Staged successor, run the fence. The predecessor sink gets `Exited(Cancelled "replaced by …")`, the successor holds the children, and the empty-children assertion passes.

Caveat: `start_paused` auto-advances the clock only when every task is idle. These probes are pure async, so that's fine, but real `spawn_blocking` machine work would make timeouts fire early. Keep those tests out of this module.
