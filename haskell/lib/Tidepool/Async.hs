{-# LANGUAGE ScopedTypeVariables #-}

-- | Green threads with the authored surface of @Control.Concurrent.Async@ —
-- PRD 20 S1-L4 (@plans\/self-iterating-harness\/20-s1l4-green-threads.md@).
--
-- __A green thread is a continuation parked in the session's multi-hole
-- registry, under its own realm.__  'async' starts a NEW suspension-capable
-- top-level run on the shared machine, so a forked computation parks
-- independently of its spawner: two threads blocked on two different effects
-- have BOTH holes pending at once, resumable in either order.
--
-- What that buys, and why it is the representation:
--
-- * __Threads blocked on effects progress independently.__  @'race' blockingX
--   blockingY@ genuinely overlaps.  Mirroring this package's names is only
--   honest if the semantics travel with them.
-- * __Cooperative, no preemption.__  A thread runs until it performs an
--   effect, then parks; the driver services whichever pending hole is ready
--   and resumes that thread until it parks again.  A thread that never
--   performs an effect starves its siblings — acceptable, it is authored
--   code.
-- * __Data races unrepresentable.__  Threads communicate by return value,
--   events, and typed messages.  No @IORef@\/@MVar@\/shared-cell effect is in
--   the row, and that is a decision rather than an omission.
-- * __Order-insensitivity is the correctness contract.__  Two pending holes
--   resumed in either order produce identical results.
--
-- == Cancellation
--
-- 'cancel' closes the thread's realm, which drops its parked frames and
-- releases its handles — its pending suspensions are discarded as a
-- mechanism, not as bookkeeping.  Cancelling a terminal thread is a no-op,
-- not an error.  IN-FLIGHT EXTERNAL work (a running agent turn) is not
-- cancelled here; it settles through the Subagent cycle's own typed terminal
-- states, and 'cancel' drops the thread that was awaiting it.
--
-- == Results cross in-heap
--
-- A thread's value reaches its waiter by handle, never through JSON, so a
-- thread may return a closure or a record of functions.
--
-- __A non-closure result should be forced (WHNF is enough) before it
-- settles.__  'asyncSpawn' hands a lazy thunk to @AsyncDoneWith@ by
-- construction; the driver reads that field once, at settle time, and holds
-- onto it until a waiter's 'asyncResult' delivers it — an unforced thunk
-- left to cross that gap (rather than being forced at settle time, in the
-- thread's own realm) has been observed to surface as a GC-forwarding
-- corruption on delivery, not a clean error.  @pure $! expensiveResult@ (or
-- an already-strict computation, as most are) sidesteps it entirely; this is
-- a property of the settle boundary, not of any particular value's shape.
--
-- == The ONE divergence from @Control.Concurrent.Async@
--
-- __A thread's own failure is not an exception.__  The row has none: failure
-- is data (PRD 20's failure-as-data lock), so a thread that can fail says so
-- in its result type — @'Async' ('Either' MyError r)@ — and you get that
-- 'Either' back from 'wait' like any other value.
--
-- Consequently 'waitCatch' ranges over 'AsyncCancelled' ALONE, rather than
-- over @SomeException@: cancellation is the only thing that can stop a thread
-- from behind the author's back.  'wait' on a cancelled thread fails loudly
-- instead of rethrowing.
--
-- That is the whole list.  Every other name here means exactly what the
-- package means it to mean.
--
-- This module is reachable only in rows containing @Green@, and is
-- auto-imported whenever @Green@ is in the row.  It has nothing to do with
-- "Tidepool.Fork", which is the answerer's unrelated fanout-to-sub-answerers
-- surface.
module Tidepool.Async
  ( -- * Threads
    Async
  , asyncThreadId
  , AsyncCancelled (..)

    -- * Creating and joining
  , async
  , wait
  , waitCatch
  , poll
  , cancel

    -- * Racing
  , waitEither
  , waitBoth
  , waitAny

    -- * Event-algebra select
  , waitEvent

    -- * Derived combinators
  , race
  , concurrently
  , mapConcurrently
  , forConcurrently
  ) where

import Prelude

import Tidepool.Effects
  ( AsyncStatus (..)
  , M
  , asyncCancel
  , asyncJoinAny
  , asyncResult
  , asyncSpawn
  , asyncStatus
  )
-- `Event`/`asyncDone` are DEFINITIONS in `Tidepool.Event` (PRD 22 lane 4), not
-- the generated `Tidepool.Effects` module — see 'waitEvent' below for why this
-- is the one place this module reaches into `Tidepool.Event`'s algebra.
import Tidepool.Event (Event, asyncDone)

-- | A handle on a green thread.  Opaque, and phantom-typed by the thread's
-- result — the same posture as @AgentHandle@.
newtype Async a = Async Int

-- | The thread's runtime identity.  Stable for the thread's life; useful for
-- tracing.
asyncThreadId :: Async a -> Int
asyncThreadId (Async t) = t

-- | The outcome of cancelling a thread — what 'waitCatch' reports for one.
data AsyncCancelled = AsyncCancelled
  deriving (Show, Eq)

-- | Fork a green thread.  Returns as soon as the thread is registered; the
-- thread runs until it performs an effect and then parks, independently of
-- this caller.
async :: M a -> M (Async a)
async body = fmap Async (asyncSpawn body)

-- | Wait for a thread and return its value.  Fails loudly if the thread was
-- cancelled — 'waitCatch' is the total form.
wait :: Async a -> M a
wait h = do
  r <- waitCatch h
  case r of
    Right a -> pure a
    Left AsyncCancelled ->
      error "Tidepool.Async.wait: thread was cancelled (use waitCatch)"

-- | Wait for a thread, reporting cancellation as data.
--
-- Two suspensions, and the split is what makes this race-free: the first
-- parks until the thread reaches a TERMINAL state, and only then is its state
-- read.  A cancel landing while this caller is parked is therefore observed,
-- not missed.
waitCatch :: Async a -> M (Either AsyncCancelled a)
waitCatch (Async t) = do
  _ <- asyncJoinAny [t]
  st <- asyncStatus t
  case st of
    AsyncWasCancelled -> pure (Left AsyncCancelled)
    _ -> fmap Right (asyncResult t)

-- | Has this thread finished?  Never parks and never advances anything.
poll :: Async a -> M (Maybe a)
poll (Async t) = do
  st <- asyncStatus t
  case st of
    AsyncSettled -> fmap Just (asyncResult t)
    _ -> pure Nothing

-- | Cancel a thread, discarding its pending suspensions.  Idempotent; a no-op
-- on a thread that already reached a terminal state.
cancel :: Async a -> M ()
cancel (Async t) = asyncCancel t

-- | Wait for the first of two threads to finish.  The loser keeps running —
-- 'cancel' it yourself if you want it stopped ('race' does).
waitEither :: Async a -> Async b -> M (Either a b)
waitEither ha hb = do
  w <- asyncJoinAny [asyncThreadId ha, asyncThreadId hb]
  if w == asyncThreadId ha
    then fmap Left (wait ha)
    else fmap Right (wait hb)

-- | Wait for both threads.  They run concurrently regardless of the order
-- these two waits are written in — each is already parked on its own
-- continuation.
waitBoth :: Async a -> Async b -> M (a, b)
waitBoth ha hb = do
  a <- wait ha
  b <- wait hb
  pure (a, b)

-- | Wait for the first of these threads to finish, returning it and its
-- value.  The others keep running.
waitAny :: [Async a] -> M (Async a, a)
waitAny [] = error "Tidepool.Async.waitAny: empty thread list"
waitAny hs = do
  w <- asyncJoinAny (map asyncThreadId hs)
  case filter (\h -> asyncThreadId h == w) hs of
    (h : _) -> do
      a <- wait h
      pure (h, a)
    [] -> error "Tidepool.Async.waitAny: joined a thread that was not waited on"

-- | The Event-algebra sibling of the package's @waitSTM@: fires once when
-- the thread reaches a terminal state (settled OR cancelled), so a select
-- over threads, timers, and mailboxes composes as one ordinary 'nextEvent'
-- (@Tidepool.Event@) instead of needing a separate blocking primitive.
--
-- Carries the HANDLE back, never the value: the typed result stays on the
-- heap and is one immediate 'wait' (or 'waitCatch', to observe a cancel) away
-- — the same reason 'poll'/'waitCatch' never take the value off this thread's
-- own settle path directly.
--
-- Built on 'asyncDone' — the raw @Int@-carrying watch this module's own
-- 'async'/'wait' machinery does not otherwise need — so this is the ONE
-- place @Tidepool.Async@ reaches into @Tidepool.Event@'s algebra. That
-- makes @RepoEvent@ a REQUIRED row member alongside @Green@ wherever
-- 'waitEvent' is actually called (a row with @Green@ but no @RepoEvent@
-- fails to resolve 'asyncDone'/'Event' — both are declared under
-- @RepoEvent@, never @Green@, so their watch vocabulary stands alone in a
-- row that has @RepoEvent@ without @Green@; the coupling runs the other way
-- only where @waitEvent@ itself is used).
waitEvent :: Async a -> Event (Async a)
waitEvent h = fmap (const h) (asyncDone (asyncThreadId h))

-- | Run two computations concurrently and return the first to finish,
-- cancelling the loser.
race :: M a -> M b -> M (Either a b)
race l r = do
  ha <- async l
  hb <- async r
  out <- waitEither ha hb
  cancel ha
  cancel hb
  pure out

-- | Run two computations concurrently and return both values.
concurrently :: M a -> M b -> M (a, b)
concurrently l r = do
  ha <- async l
  hb <- async r
  waitBoth ha hb

-- | One thread per element, all running concurrently; results come back in
-- the ORIGINAL list order.  Completion order is never an input — that is PRD
-- 20's order-insensitivity contract, and it is why this collects by walking
-- the handles rather than by whoever finishes first.
mapConcurrently :: (a -> M b) -> [a] -> M [b]
mapConcurrently f xs = do
  hs <- mapM (async . f) xs
  mapM wait hs

-- | 'mapConcurrently' with the arguments flipped — @forConcurrently xs f@.
forConcurrently :: [a] -> (a -> M b) -> M [b]
forConcurrently xs f = mapConcurrently f xs
