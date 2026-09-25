# Parcel-7 shared-identity audit (read-only)

Scope: every Haskell heap object one actor mutates in place and another actor
reads by identity (the `ExitCell` class), whether it can be captured into a
value that crosses a session boundary under parcel 7 (`SelectedContext`
launches get a dedicated machine; entries/mailbox values cross via heap-copy
`Parcel`s, not shared memory), and whether the reader can legitimately be on
a different machine than the writer.

`grep -rn "MutVar\|IORef" bridge/haskell/lib bridge/haskell/actors` finds
nothing but a negative doc comment (`Tidepool/Async.hs:22`, "No
`IORef`/`MVar`/shared-cell effect is..."). `Tidepool.Internal.ExitCell`
(`bridge/haskell/lib/Tidepool/Internal/ExitCell.hs`) is confirmed the only
shared-mutable-identity primitive in the authored surface.

## 1. `Response`'s cell — CONFIRMED BROKEN (already reproduced this session)

- Cell minted: `newRequestHandles` (`bridge/haskell/lib/Tidepool/Agent/Reply/Internal.hs:314-316`),
  `newExitCell pending`, embedded in the `Response` returned to the caller of
  `requestWithProgress`/`requestWithSited` (owner side).
- Mutator: `fillResponse` (`Reply/Internal.hs:318-319`, `fillExitCell cell`),
  called from `runRequest` (`bridge/haskell/actors/Tidepool/Actors/Internal/Agent.hs:469-470`).
  `runRequest` is the continuation of `requestSessionSited`'s suspend/resume
  (an `AgentSessionWith` boundary, captured by
  `ResidentInteractiveSession::capture`, `exomonad/actor/src/interactive_session.rs:183-230`)
  and — confirmed via `agentLoop`'s own `Actor.receive handle` /
  `handle (RunRequest action) = action >> pure ((), True)`
  (`Agent.hs:681-691`) — always runs as part of the **target's own** mailbox
  loop, i.e. on the target/child's session.
- Reader: `readResponse`/`pollResponse` (`Reply/Internal.hs:336-337`,
  `Reply/Internal.hs:362-377`), called by the **owner**, on the owner's
  session — `pollResponse`'s own error text ("Tidepool response became ready
  before its Haskell cell was filled") and `Tidepool/Agent/Watch/Internal.hs:249`'s
  matching error in `pollWatch` are exactly this failure mode.
- Cross-machine reader confirmed: **yes**. `runRequest`'s closure (closing
  over the owner-minted `response`) is captured into the `RunRequest`
  payload `submitRequest` sends (`Reply/Internal.hs:303-311`), delivered as a
  `MailboxValue` `Cast` to the target
  (`exomonad/actor/src/resident_actor.rs:3710-3762`,
  `RequestSubmission` boundary) — a heap-copy `Parcel` crossing
  (`ResidentSession::export_custody`/`import_parcel`), not shared memory, for
  any target on a session other than the submitter's. No compensating
  transfer of the *filled* value back to the owner exists: `stage_request_reply`
  (`resident_actor.rs:2390-2489`) and `finish_reply`
  (`exomonad/actor/src/request.rs:1206-1223`) never call
  `transfer_custody`/`import_shared_custody`; the one-way-Cast branch of
  `finish_receiver` (`resident_actor.rs:4944-4965`) explicitly
  `drop(reply.value)`s the settled Cast's own return. Progress has an
  equivalent explicit publish/import pair (`PublishProgressWith`/
  `ObserveProgressWith`, `resident_workbench.rs:7341-7470`); response has
  none. **Reproduced this session**: `progress_retains_closures_and_watch_snapshots_across_calls`
  fails at `pollWatch combined`'s `awaitValue answer` with exactly this
  invariant violation, against a `startAgent (readonlyAgent ...)` target,
  which is `SelectedContext`-eligible (`exomonad/actor/src/start.rs:344`).

## 2. `ActorRef`'s exit cell (`Tidepool.Actor`) — LIKELY BROKEN, same shape, not yet reproduced

- Cell minted: `startActor`, `startActorFork`, `replaceActor`
  (`bridge/haskell/lib/Tidepool/Actor.hs:129-148` / `176-229` / `211-231`),
  `let cell = newExitCell startup`, embedded in the returned
  `ActorRef actorId incarnation cell` (caller/parent side).
- Mutator: `fillExitCell cell result` inside each function's own `entry`
  closure (`Actor.hs:157`, `187`, `231`), run as the launched actor's own
  top-level program body (`result <- raiseKernel (install startup initial)`,
  same lines) — i.e. on the **child's** session once it has one of its own.
  `entry` (which closes over `cell`) is exactly the value that becomes the
  child's compiled entry and crosses as a parcel on a dedicated-machine
  launch, per `start.rs`'s own doc comment: "a `SelectedContext` launch from
  here is eligible for its own machine session too, its entry crossing as a
  parcel" (`exomonad/actor/src/start.rs:352-354`).
- Reader: `awaitExit`/`pollExit` (`Actor.hs:394-420`), called by whoever
  holds the `ActorRef` — normally the parent/launcher, on its own session.
  `awaitExit`'s own doc comment states the invariant this class of bug
  violates verbatim: "Completion is published into the reference's cell
  before the Rust terminal transition. Seeing completion with an empty cell
  is therefore an engine invariant violation, not a lifecycle case authors
  must handle." (`Actor.hs:394-397`).
- Cross-machine reader: **yes, and broadly so**. `Tidepool.Actor.start`/`fork`
  (`startActor`/`startActorFork`) always route through
  `ResidentActorStart::capture` (`start.rs:293-352`), which hard-codes
  `context: ForkContext::SelectedContext` for both the `ActorStartWith` and
  `ActorForkWith` shapes — every direct `Tidepool.Actor.startActor`/
  `startActorFork` launch is dedicated-machine-eligible today, not only
  read-only agents. `unfold`/`childWithProgress` (the `Forks` effect) instead
  passes `fork_context` straight through from Haskell
  (`resident_workbench.rs:5974-5991`, `context: fork_context`) — so any
  caller that sets it to `SelectedContext` (the coordinator's own earlier
  note: "the producer and consumer children run on fresh machines (selected,
  `coding` via `childWithProgress`)") is equally exposed, not just
  `errand`/`startAgent`. Not yet reproduced in a facade test — flagging by
  code shape and the confirmed-broken sibling in §1, which shares the exact
  `newExitCell`/closure-capture/`fillExitCell`/`readExitCell` pattern and the
  exact same crossing mechanism (entry-as-parcel to a dedicated child).

## 3. `ActorRef`'s exit cell via `launchFreshActor`/`launchForkedActor` (`Tidepool.Actors.Internal.Agent`) — LIKELY BROKEN, same shape

- Cell minted and filled identically to §2, in
  `bridge/haskell/actors/Tidepool/Actors/Internal/Agent.hs:527-561`
  (`launchFreshActor`, backs `startAgent`) and `:565-611`
  (`launchForkedActor`, backs `startForkedAgent`/fork-based agent launches).
  Same `newExitCell startup` / `entry`-closure `fillExitCell` / returned
  `ActorRef ... cell` shape as §2; feeds the same `awaitExit`/`pollExit`
  readers.
- `launchFreshActor`'s own comment (`Agent.hs:551-559`) asserts its `entry`
  "is never eligible for its own child session" — **this is stale**: it
  predates this session's widening of `child_session_eligibility` to every
  `SelectedContext` launch (`start.rs`, parcel 7). `AgentLaunchWith`
  (`launchFreshActor`'s effect) decodes in Rust through the exact same
  `ResidentActorStart::capture_decoded` path as `ActorStartWith`
  (`resident_workbench.rs:5918-5934`, hard-coded
  `context: crate::ForkContext::SelectedContext`) — i.e. `startAgent`
  launches (including the `readonlyAgent` case already confirmed broken in
  §1, whose *response* travels this exact path) are now dedicated-machine
  eligible for their *own actor-completion exit cell* too, contradicting the
  comment's premise.

## 4. `Async`'s exit cell (`Tidepool.Async`) — lower risk, same-machine by construction

- Cell minted/filled/read entirely within `async`/`wait`/`waitCatch`/`poll`
  (`bridge/haskell/lib/Tidepool/Async.hs:112-160`). `asyncSpawn` runs a green
  thread cooperatively scheduled *within the same prepared program/session*
  (not a separate actor, no session boundary in the ordinary API surface).
  Flagged for completeness, not as a live risk: nothing in the stdlib
  surface exposes a captured `Async` handle to cross a session boundary the
  way `Response`/`ActorRef` do (both explicitly returned to a caller that
  may live on a different session from the value's origin).

## 5. `AgentRef`'s placeholder cell (`Tidepool.Agent.Ref`) — no risk

- `internalAgentRef` (`bridge/haskell/lib/Tidepool/Agent/Ref.hs:47-51`) mints
  `newExitCell ()` and never fills or reads it — the type's own doc comment:
  "a permanently pending placeholder; use this reference for its address,
  not for observing the actor's exit." Not part of this bug class.

## Watches, routes, Replies more broadly

Everything else the `Replies`/`Watches` effect rows carry structurally —
`Progress` (`newtype Progress progress = Progress RequestId`, no cell),
`Watch result` (`Watch WatchId (Await result)`, no cell), `Reply result`
(`Reply RequestId`, no cell) — resolves through Rust-tracked registry state
(`RequestRegistry`, `exomonad/actor/src/request.rs`) plus, for progress
specifically, an explicit cross-session import
(`PublishProgressWith`/`ObserveProgressWith`, §1 above). None of them embed
an `ExitCell`; they are not exposed to this specific bug class. The
`ExitCell` sites in §1-§3 above are the exhaustive list.
