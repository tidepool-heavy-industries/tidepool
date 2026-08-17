# S1-L4 scaffold — green threads (`Tidepool.Async`) + capability mailboxes

**Status:** scaffold (2026-08-17) — the contract the implementation waves build.
**Parent:** [PRD 20](20-exomonad-v3-prd.md) §"Green threads (the scheduler)",
§"Node residency and messaging".

## What a green thread IS

**A green thread is a continuation parked in the session's multi-hole
registry, under its own realm.** Forking one starts a NEW suspension-capable
top-level run on the shared machine; that run parks independently of its
spawner, so two threads blocked on two different effects have BOTH holes
pending at once, resumable by identity in either order. This is PRD 20's
locked substrate (lines 255–267), and it is what makes the properties below
true rather than approximated:

- **Threads blocked on effects progress independently.** `race blockingX
  blockingY` genuinely overlaps. Mirroring `Control.Concurrent.Async`'s names
  is only honest if the semantics travel with them.
- **Concurrent cognition windows are reachable.** N `runLLMTurn` holes pending
  simultaneously — each its own answerer realm — is exactly "several threads
  parked at once". Nothing needs a begin/await split to overlap, so no
  blocking verb has to warp its shape.
- **Cooperative, no preemption.** A thread runs until it performs an effect,
  then parks. The driver services whichever pending hole is ready and resumes
  that thread until it parks again. FIFO ready queue.
- **Order-insensitivity is the correctness contract.** Two pending holes
  resumed in either order must produce identical results.

## The mechanism, and how little of it is new

The load-bearing discovery: **the closure-payload tenure path is already
effect-agnostic.** In `tidepool-codegen/src/jit_machine.rs`,
`request_carries_closure_sentinel` deep-scans a suspended request's FIELDS for
`CLOSURE_SENTINEL` — it keys on the sentinel, never on the effect or
constructor name — and `tenure_finalized_payload` evacuates **field index 1**
of the request Con into old-space, registering it as a persistent GC root, and
lands the slot on the parked `ContinuationFrame`. `ResidentSession::
finalized_handle(hole)` then mints a `ValueHandle` over it.

So a spawn request shaped

```haskell
AsyncSpawnWith :: Int -> (Int -> M a) -> Green Int
```

— a dummy `Int` at field 0, the thread body as a FUNCTION at field 1 — gets
its body tenured and handle-able with **zero `tidepool-codegen` changes**. The
body is a lambda rather than a bare `M a` on purpose: the sentinel scan needs a
closure to fire, and `async (pure 5)` (a body with no closures anywhere) would
otherwise take the lossy data bridge. A lambda always fires.

### The one new entry — `ResidentSession::run_forked` (landed)

`tidepool-runtime` gains ONE method, alongside — never replacing —
`run_child`/`run_child_pure`. Their refusal of a suspending child
(`ResidentError::ChildSuspended`) is a policy protecting their value-shaped
signatures and stays exactly as it is; this is a third entry with a different
contract:

> **run a handle-rooted body as a NEW suspension-capable top-level run**,
> under a caller-chosen `RealmId`.

Its body is `apply_finalized`'s expression synthesis — `App(Var(BODY), Lit 0)`
with `BODY` bound through an `ExternalEnv` to the handle's slot address —
driven through `run_fragment_suspendable_parked` (the registry-parking,
suspension-capable entry) instead of `run_fragment_pure`, and registered as a
TOP-LEVEL fragment rather than a child (a green thread is a peer, not a run
over a suspended parent's world). It returns the ordinary `ResidentOutcome`.

**The realm propagates.** `resume_parked` replays a frame's OWN realm, so every
later suspension of a thread parks under the realm it was forked with. That is
what makes `close_realm` a complete cancellation rather than a first-frame one,
and it is a property of the machine, not something the driver maintains.

**Handle ownership on completion is the SESSION's realm, deliberately** — a
result must outlive the thread realm that produced it, because cancelling or
retiring a thread closes that realm while a waiter may still hold the value.

### The return trip reuses the outbound mechanism

A thread's last act is to SUSPEND on `AsyncDoneWith` carrying its own result at
field 1. So the result crosses back by exactly the crossing the body crossed
out by: a closure result tenures and rides as a `ValueHandle`, a data result
bridges to a `Value` — the same dichotomy `finalize` already has, serviced by
the same `finalize_is_closure` / take-handle-or-take-value pair the harness
already implements. Nothing needs a new outcome type, a result stash, or a
`ParkKind::Binding` path.

### GC rooting — verified, not assumed

PRD 20 claims parked continuations are GC roots "exactly as parked holes are
today". That is true and machine-enforced: each `ContinuationFrame` is a
registered stowed root, and `stowed_roots_count() == parked_count()` is
`debug_assert`ed at **every** registry mutation
(`tidepool-codegen/CLAUDE.md`, the realm machinery). The multi-hole registry
with several realms parked at once is already the production harness path, not
a new capability. The thread body's own slot is a persistent root from the
moment it is tenured. Nothing here extends the rooting model.

If this turns out to be wrong under N threads — a rooting receipt that fails
to hold at quiescence — **stop and report**; do not restructure the rooting
model to accommodate green threads.

### Realms are the unit of cancellation

**One `RealmId` per green thread.** `cancel` is then `close_realm(realm)`,
which drops that thread's parked frames, releases its outstanding handles,
leaves sibling realms untouched, and reconciles the session's parked-hole list
against the machine's surviving frames. "Cancel discards the thread's pending
suspensions" is therefore a mechanism, not bookkeeping to keep in sync.

In-flight EXTERNAL work (a running agent turn) is not this lane's to stop: it
settles through the Subagent cycle's own typed terminal states. `cancel` drops
the thread that was awaiting it.

### Result delivery is by handle, never by bridge

A thread's result crosses to its waiter the way finalize-by-reference already
delivers closures: the completing run tenures its result, `mint_handle_from_root`
mints a handle, and the waiter's parked continuation is resumed through
`ResidentSession::resume_handle` — the payload feeds the continuation verbatim,
on the same heap, closures included. No JSON round-trip, so a thread may return
a function or a record of functions.

## The `Green` effect

```
AsyncSpawnWith   :: Int -> (Int -> M ()) -> Green Int  -- body at field 1 (tenured)
AsyncDoneWith    :: Int -> a -> Green ()               -- a thread's last act; result at field 1
AsyncJoinAnyWith :: [Int] -> Green Int                 -- park until ANY is terminal; the winner's id
AsyncStatusWith  :: Int -> Green Int                   -- 0 running, 1 settled, 2 cancelled; never parks
AsyncResultWith  :: Int -> Green a                     -- a settled thread's result, by handle
AsyncCancelWith  :: Int -> Green ()                    -- close_realm; idempotent
```

`AsyncResultWith`'s free `a` is the same shape `finalize`'s free `a` already
has, and `Async a`'s phantom carries the type — the same posture as
agent-cycles' phantom-typed `AgentHandle`.

Splitting the join into **park-until-terminal** (`AsyncJoinAnyWith`) then
**read-state** (`AsyncStatusWith`) then **read-value** (`AsyncResultWith`) is
what makes `waitCatch` race-free: a `cancel` landing while a waiter is parked
is observed by the state read that follows the wake, never missed. It also
means the driver only ever constructs primitives (an `Int` id, an `Int` state)
— the one typed value it moves is the result, and that moves by handle.

`Green` goes at the **END** of `outer_decls()`; `RunLLMTurn` must stay at
index 0 (`outer_row_suspends_everything`).

## The driver scheduler

The driver owns a thread table (`tid → realm, state`) where state is
`Ready | Parked(hole) | Settled(handle) | Cancelled`, plus a waiter map
(`tid → [waiting hole]`), and a FIFO ready queue. Its loop is unchanged in
character — service a pending hole, resume its continuation — with the routing
extended: a hole belongs to a thread, a settled thread resumes its waiters by
handle, and a cancelled thread's holes vanish with its realm.

Servicing `AsyncSpawnWith`: take the body handle off the spawner's parked
frame, resume the spawner with the fresh `tid`, and enqueue the new thread as
READY. Which of those two runs first is the ready queue's business, not the
author's.

## The authored surface (`haskell/lib/Tidepool/Async.hs`)

Names AND semantics track `Control.Concurrent.Async`. Under the registry form
each verb is one suspending send, so the module is thin — there is no scheduler
in Haskell.

```haskell
data Async a                                   -- opaque: phantom-typed thread id
asyncThreadId   :: Async a -> Int

async           :: M a -> M (Async a)
wait            :: Async a -> M a
waitCatch       :: Async a -> M (Either AsyncCancelled a)
poll            :: Async a -> M (Maybe a)
cancel          :: Async a -> M ()

waitEither      :: Async a -> Async b -> M (Either a b)
waitBoth        :: Async a -> Async b -> M (a, b)
waitAny         :: [Async a] -> M (Async a, a)

race            :: M a -> M b -> M (Either a b)
concurrently    :: M a -> M b -> M (a, b)
mapConcurrently :: (a -> M b) -> [a] -> M [b]
forConcurrently :: [a] -> (a -> M b) -> M [b]

-- The Event-algebra sibling of the package's `waitSTM`: fires when the thread
-- settles, so a select over threads, agents, timers, and mailboxes is one
-- ordinary `nextEvent`.
waitEvent       :: Async a -> Event (Async a)

data AsyncCancelled = AsyncCancelled deriving (Show, Eq)
```

`waitEvent` carries the HANDLE, not the value — the typed result is then one
immediate `wait` away. That keeps results on the heap (closures survive) and
keeps the event payload BARE like `Tick`, so `nextEvent` yields
`Observed (Async a)` with no double wrap. It needs a `WatchAsync Int` watch and
an `ObservedAsyncDone EventId Int` observation in `event_decl`, published
through the same `SubscriptionRegistry` the repository watches use. The `Int`
is raw rather than a newtype because `event_decl`'s type_defs must stand alone
in a row with `RepoEvent` but not `Green`.

`Tidepool.Async` is a stdlib module like `Tidepool.Fork`, auto-imported
whenever `Green` is in the row.

> Name collision to avoid: `Tidepool.Fork` is the ANSWERER's unrelated
> fanout-to-sub-answerers surface (`fork`/`forkAll`/`forkMap`). Nothing in the
> green-thread module is called `fork`.

### Wave 1 acceptance

The test that pins the representation: **two green threads blocked on two
DIFFERENT effects have BOTH holes pending in the registry simultaneously, and
resuming them in either order produces identical results.** Assert on the
registry's pending set, not on a completion count — a count passes under a
representation that serializes.

Then, in the `tests/outer_effects.rs` family bundle (one fixture, one compile,
many named assertions — a new suspension kind joins it rather than paying its
own extract compile): `wait` joins, `waitEither` races with the loser still
live afterward, `cancel` discards the thread's pending suspensions and
`waitCatch` reports `Left AsyncCancelled`, and `mapConcurrently` returns
results in the ORIGINAL list order with threads of differing lengths. Plus
`green_decl()` in the dev-tree typecheck row, and
`outer_row_suspends_everything` passing unedited.

## Wave 2 — capability handles and mailboxes

**Handles are capabilities: possession is permission.** No global registry, no
addressing scheme, no node ids in the authored surface. A handle is minted at
fork and handed to exactly the parties entitled to use it; the runtime's
mailbox table is an implementation detail of the effect, not an address space.

```haskell
data NodeHandle up down r     -- parent's end: send down, receive up, await the fold
data Uplink up                -- child's end: send up
data NodeCtx up down = NodeCtx { uplink :: Uplink up, inbox :: Event down }

forkNode :: (FromJSON up, FromJSON down) => (NodeCtx up down -> M r) -> M (NodeHandle up down r)
sendDown :: ToJSON down  => NodeHandle up down r -> down -> M ()
sendUp   :: ToJSON up    => Uplink up -> up -> M ()
inbox    :: FromJSON down => NodeCtx up down -> Event down
received :: FromJSON up   => NodeHandle up down r -> Event up
folded   :: NodeHandle up down r -> Event (Async r)  -- `waitEvent` on the underlying thread
```

`up`/`down` are ABSOLUTE tree directions, not relative to whoever holds the
value: a parent sends down and receives up, a child receives down and sends
up, and both ends name the two type parameters the same way.

**Corrected 2026-08-17, in flight** (the child implementing this wave found
both gaps by trying to write the acceptance below): the sketch above
originally read `NodeHandle down r` with no typed way for the PARENT to
observe an up message, even though `sendUp`'s whole point (escalation, per
this PRD's `Up = Escalate Failure | Progress Text`) is for the parent to read
it — a hole in the sketch, not a one-directional design. `NodeHandle` widens
to carry `up` too, with `received` as `inbox`'s parent-side sibling. `folded`
likewise was typed `Event r` while described as "`waitEvent` on the
underlying thread" — `waitEvent` carries the HANDLE, never the value (by
design: the projection is pure and cannot itself perform the effectful
`wait`), so `folded`'s type now matches its own description instead of
disagreeing with it.

- **Sends never block.** `sendDown`/`sendUp` append and return.
- **Bursts coalesce.** The Haskell helper derives a coalesce KEY from the
  message's own generic-JSON tag (`payload.tag` under the TaggedObject
  encoding) and passes it explicitly, so the Rust handler stays dumb: a send
  whose key already sits in the queue REPLACES that entry in place, preserving
  the earlier arrival position. Latest target wins; queue length is bounded by
  the number of distinct message tags.
- **Messages are reconciliation hints.** A dropped or coalesced message is
  never a correctness hole — every instruction is re-derivable from git plus
  the run journal. No durable mailbox machinery exists or is wanted.

**Receive is an Event source, not a second blocking primitive.** `inbox` and
`received` both plug into the existing `Tidepool.Event` algebra alongside
`WatchAsync`, so `nextEvent (fmap Left (received h) <|> fmap Right deadline)`
is an ordinary select. **Do not add a blocking mailbox receive.** Payloads
stay BARE (like `Tick`).

Mailbox verbs join `RepoEvent`, not `Green` — a mailbox IS an event source,
and `RepoEvent` is already handler-dispatched and already wired into the
driver, so these three verbs needed nothing else:

```
MailboxNew  :: RepoEvent Int
MailboxSend :: Int -> Text -> Value -> RepoEvent ()   -- (mailbox, coalesce key, payload)
MailboxDrop :: Int -> RepoEvent ()
```

### Wave 2 acceptance

A parent select-loops over `{message, deadline}`: it forks a child, the child
`sendUp`s, the parent's `nextEvent (fmap Left (received h) <|> fmap Right (after ms))`
observes the message before the deadline, and a second iteration with a
silent child observes the `Tick`. A burst of same-tag sends is observed once,
carrying the LAST payload — proven deterministically (`folded`+`wait` on the
sending child first, so the whole burst has already coalesced before the
select runs, rather than racing the scheduler's own interleaving).

## Out of scope for this lane

The wake journal (per-wake source/continuation/payload digest) and the seeded
order-insensitivity permutation property test over whole outcome TREES belong
with `Tidepool.Swarm`'s folds, which consume child outcomes in plan order.
Wave 1's either-order acceptance covers the substrate's half of that contract.

`agentDone` joining the Event algebra is deliberately NOT taken here (declined
to root, 2026-08-17): it is the twin of `waitEvent` and should reuse the
`WatchAsync`/`ObservedAsyncDone` shape this lane introduces, but it belongs to
whoever owns the agent-cycle surface.
