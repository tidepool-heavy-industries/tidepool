# S1-L4 scaffold — green threads (`Tidepool.Async`) + capability mailboxes

**Status:** scaffold (2026-08-16) — the contract waves 1 and 2 implement.
**Parent:** [PRD 20](20-exomonad-v3-prd.md) §"Green threads (the scheduler)",
§"Node residency and messaging".

## What a green thread IS here

**A green thread is an `M a` value — the residual computation — plus a runtime
thread identity.** Not a parked frame the driver holds, not a second machine.

The mechanism already exists and is already exercised in production. Every
outer-row effect suspends (the row's handled prefix is empty — `ask_tag == 0`,
pinned by `outer_row_suspends_everything`), and freer-simple's `Eff` is
inspectable from authored Haskell: the eval preamble imports
`Control.Monad.Freer.Internal (Eff(..), qApp, tsingleton)`, and
`event_decl`'s own `pumpEff` already deconstructs `E u q` and reconstructs
`E u (tsingleton …)` across a real suspension — that is what `withHandler`
is built from, and `tests/outer_effects.rs` drives it end to end today.

So one step of a thread is:

```haskell
data Step a = Finished a | Blocked (M a)

stepM :: M a -> M (Step a)
stepM (Val a)  = pure (Finished a)
stepM (E u q)  = E u (tsingleton Val) >>= \x -> pure (Blocked (qApp q x))
```

`E u (tsingleton Val)` performs *exactly that one effect* — one suspension,
serviced by the one driver loop, resumed — and hands back the residual. A
scheduler is then an ordinary round-robin over residuals. Consequences, all of
them the locked semantics rather than approximations of them:

- **Cooperative at effect boundaries only.** A thread advances exactly one
  effect per turn of the scheduler. No preemption, no time slicing —
  structurally, not by discipline.
- **Effects interleave through the one driver loop.** Every step is an
  ordinary outer suspension the driver services exactly as it services a
  single-threaded loop's. Nothing about `service_outer_effect` changes.
- **Data races unrepresentable.** A thread is a value; threads share nothing.
  No `IORef`/`MVar`/shared-cell effect is added to the row (PRD 20, locked).
- **`forConcurrently` and friends are stdlib derivations,** not primitives
  (PRD 20, locked).

### The one honest limitation, stated up front

There is no background execution. A thread makes progress only while a
scheduling point is driving it, and a scheduling point can only drive the
threads it was handed. `async` therefore performs no effect of its own, and

```haskell
a <- async bodyA
b <- async bodyB
x <- wait a          -- bodyA runs to completion here…
y <- wait b          -- …only then does bodyB start
```

is sequential. Concurrency is expressed by the multi-thread scheduling points —
`waitBoth`, `waitAny`, `waitEither`, `concurrently`, `mapConcurrently`,
`race` — which round-robin over every thread they hold. This is faithful to
the substrate (a single machine, a single driver loop) and is the same
constraint the PRD already accepts under "a forked computation that never
performs a parking effect starves its siblings". Do not paper over it with a
`async`-steps-once hack: stepping at creation makes `async` perform an
arbitrary effect at an arbitrary point, which is worse.

External concurrency is unaffected and is where the real overlap comes from: a
`spawnAsync` starts a backend process and returns, so N cycles genuinely run
at once while the scheduler round-robins the awaits.

### Why not park each thread as its own registry hole

That design — `async` suspends carrying its body as a closure, the driver mints
a `ValueHandle` over it and starts a *new suspension-capable top-level run* on
the shared machine — is the shape PRD 20 sketches, and the multi-hole registry
would hold each thread's parks by identity. It needs a new
`ResidentSession` entry (`run_child`/`run_child_pure` both refuse a child that
suspends; `apply_finalized` is the pure, non-`Eff` sibling), i.e. a
**tidepool-runtime** change, which is outside this lane's boundary. It also
buys nothing the value representation does not already give: the cooperative
semantics, the interleaving, and the race-freedom are identical, and the value
representation is typed end to end with no `unsafeCoerce` and no existential
thread table. Revisit only if a genuine background-progress requirement
appears; `Async` is abstract, so the representation can move.

## The `Green` effect — the only Rust surface wave 1 needs

Thread identity and cancellation status cannot live in a Haskell value (`cancel
a` must be observable to a later `wait a`), so they live in a runtime table
behind a new opt-in effect, wired exactly the way the Journal lane wired
`Journal` (`journal_effect_def!` + `JournalHandler` + `set_journal_handler` +
`OuterEffectKind::Journal`) — that lane is the template for every wiring step.

```
GreenNew       :: Green Int          -- mint a fresh thread id
GreenCancel    :: Int -> Green ()    -- mark cancelled; idempotent
GreenCancelled :: Int -> Green Bool  -- has this id been cancelled?
GreenSettle    :: Int -> Green ()    -- mark settled (poll + observability)
```

Return types stay primitive (`Int`/`Bool`/`()`), so nothing is added to
`tidepool-bridge-effects`. The effect goes at the **END** of `outer_decls()` —
`RunLLMTurn` must stay at index 0 (`outer_row_suspends_everything`).

Cancellation is checked at scheduling-point ENTRY and at each round-robin
iteration of a multi-thread wait, not per step. A cancel therefore takes effect
at the next scheduling point that observes the handle — which is also how real
`cancel` behaves (asynchronous), and is what "cancel discards the thread's
pending suspensions" means here: the residual is dropped and never resumed, so
the suspensions it would have raised never reach the driver.

## Wave 1 — the authored surface (`haskell/lib/Tidepool/Async.hs`)

Names and semantics track `Control.Concurrent.Async`. The API is the prompt;
every deviation is a fluency tax, and the deviations below are the minimum the
substrate forces.

```haskell
data Async a                                   -- opaque: thread id + residual
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

data AsyncCancelled = AsyncCancelled deriving (Show, Eq)
```

Deviations, each documented at its definition:

- **`waitEither` discards the loser's residual** (there is no detached
  execution to leave it running in) — i.e. it has `waitEitherCancel`'s
  semantics under `waitEither`'s signature. `waitAny` likewise.
- **`poll`** answers `Nothing` for anything not already settled; it cannot
  reflect background progress, because there is none.
- **`waitCatch`** ranges over `AsyncCancelled` only. A thread's own failure is
  an ordinary `Either` in its result type (the row's discipline), not an
  exception.

`Tidepool.Async` is a stdlib module like `Tidepool.Fork` — it imports
`Tidepool.Effects (M)` and `Control.Monad.Freer.Internal (Eff(..), qApp,
tsingleton)`, needs no build-time registration, and is reachable only in rows
containing `Green`.

> Name collision to avoid: `Tidepool.Fork` is the ANSWERER's fanout-to-
> sub-answerers surface (`fork`/`forkAll`/`forkMap`), an unrelated effect. The
> green-thread module is `Tidepool.Async`, and nothing in it is called `fork`.

### Wave 1 acceptance

Joins `tests/outer_effects.rs` (family bundle — one fixture, one compile, every
assertion off the resulting `State`; a new suspension kind joins the bundle
rather than paying its own extract compile). The fixture loop must show:

1. N threads whose effects genuinely interleave through the one driver loop —
   assert on an ORDER trace (e.g. each thread `record`s or `say`s a tagged
   step; the durable trace must show A,B,A,B…, not A,A,B,B).
2. `wait` joins and returns the thread's value.
3. `waitEither` picks the winner and the loser never runs again.
4. `cancel` then `waitCatch` yields `Left AsyncCancelled`, and the cancelled
   thread's remaining effects never appear in the trace.

Plus `dogfood_harness_typecheck.rs`'s dev-tree row gains `green_decl()` (row
widening), and `outer_row_suspends_everything` must keep passing unchanged.

## Wave 2 — capability handles and mailboxes

**Handles are capabilities: possession is permission.** No global registry, no
addressing scheme, no node ids in the authored surface. A handle is minted at
fork and handed to exactly the parties entitled to use it; the runtime's
mailbox table is an implementation detail of the effect, not an address space.

```haskell
data NodeHandle down r        -- parent's end: send down, await the fold
data Uplink up                -- child's end: send up
data NodeCtx up down = NodeCtx { uplink :: Uplink up, inbox :: Event down }

forkNode :: (NodeCtx up down -> M r) -> M (NodeHandle down r)
sendDown :: ToJSON down => NodeHandle down r -> down -> M ()
sendUp   :: ToJSON up   => Uplink up -> up -> M ()
inbox    :: FromJSON down => NodeCtx up down -> Event down
folded   :: NodeHandle down r -> Event r         -- or `wait` on the underlying Async
```

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

**Receive is an Event source, not a second blocking primitive.** The inbox
plugs into the existing `Tidepool.Event` algebra: `event_decl` gains a
`WatchMailbox Int` watch and an `ObservedMessage EventId Int Value`
observation, published through the same `SubscriptionRegistry` the repository
watches use, so `nextEvent (fmap Left inbox <|> fmap Right deadline)` is an
ordinary select. **Do not add a blocking mailbox receive.**

Payloads stay BARE (like `Tick`, unlike `commit`/`headChanged`): `inbox ::
Event down`, so `nextEvent` yields `Observed down` with no double wrap.

The `Watch`/`RepositoryEvent` constructors carry a raw `Int` mailbox id rather
than a `MailboxId` newtype, because `event_decl`'s type_defs must stand alone
in a row that has `RepoEvent` but not `Green`. The newtype lives on the `Green`
side.

Mailbox verbs join the same `Green` effect (one row widening, one wiring site):

```
MailboxNew  :: Green Int
MailboxSend :: Int -> Text -> Value -> Green ()   -- (mailbox, coalesce key, payload)
MailboxDrop :: Int -> Green ()
```

### Wave 2 acceptance

A parent select-loops over `{message, deadline}`: it forks a child, the child
`sendUp`s, the parent's `nextEvent (inbox <|> after ms)` observes the message
before the deadline, and a second iteration with a silent child observes the
`Tick`. A burst of same-tag sends is observed once, carrying the LAST payload.

## Out of scope for this lane

The wake journal (per-wake source/continuation/payload digest) and the seeded
order-insensitivity permutation property test are S1-L4 items the PRD lists but
this lane's DONE criteria do not; they belong with `Tidepool.Swarm`'s folds,
which consume child outcomes in plan order. Noted, not built here.

Cancel of IN-FLIGHT EXTERNAL work (a running agent turn) settles through the
Subagent cycle's typed terminal states — the agent-cycles lane owns that
contract. This lane defines the seam only: `cancel` drops the residual, and
whatever the residual was awaiting settles by its own terminal states.
