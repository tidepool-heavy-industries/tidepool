{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | Green threads with the authored surface of @Control.Concurrent.Async@ —
-- PRD 20 S1-L4 (@plans\/self-iterating-harness\/20-s1l4-green-threads.md@).
--
-- __A green thread is an @M a@ value__ — its residual computation — plus a
-- runtime thread identity.  It is not a parked frame the driver holds and not
-- a second machine.  freer-simple's 'Eff' is inspectable, so one step of a
-- thread is one effect:
--
-- @
-- 'stepM' ('Val' a)  = pure ('Finished' a)
-- 'stepM' ('E' u q)  = 'E' u ('tsingleton' 'Val') >>= \\x -> pure ('Blocked' ('qApp' q x))
-- @
--
-- @E u (tsingleton Val)@ performs exactly that one effect — one ordinary
-- suspension, serviced by the one driver loop, resumed — and hands back the
-- rest of the computation.  (The same deconstruct-and-reconstruct is what
-- @withHandler@'s @pumpEff@ already does across real suspensions.)  A
-- scheduler is then a round-robin over residuals, and the locked semantics
-- fall out structurally rather than by discipline:
--
-- * __Cooperative at effect boundaries only.__  A thread advances exactly one
--   effect per turn.  No preemption, no time slicing.
-- * __Effects interleave through the one driver loop.__  Every step is an
--   ordinary outer suspension.
-- * __Data races unrepresentable.__  Threads are values; they share nothing.
--   Nothing here adds a mutable cell to the row.
--
-- == The one limitation, stated up front
--
-- There is no background execution.  A thread progresses only while a
-- scheduling point is driving it, and a scheduling point drives only the
-- threads it was handed.  So
--
-- @
-- a <- 'async' bodyA
-- b <- 'async' bodyB
-- x <- 'wait' a        -- bodyA runs to completion here…
-- y <- 'wait' b        -- …only then does bodyB start
-- @
--
-- is sequential.  Concurrency is expressed by the multi-thread scheduling
-- points — 'waitBoth', 'waitAny', 'waitEither', 'concurrently',
-- 'mapConcurrently', 'race' — each of which round-robins over every thread it
-- holds.  External concurrency is unaffected: @spawnAsync@ starts a backend
-- process and returns, so N agent cycles genuinely overlap while the scheduler
-- round-robins their awaits.
--
-- == Cancellation
--
-- 'cancel' marks the thread cancelled in the runtime table and the holder
-- drops its residual, so the suspensions that residual would have raised never
-- reach the driver.  A scheduling point reads the cancelled bit __once, at
-- entry__ — that is not an optimization but a theorem: while a scheduling
-- point is driving, no other Haskell code runs, so the bit cannot change
-- underneath it.  A cancel therefore takes effect at the next scheduling point
-- that observes the handle, which is also how real 'cancel' behaves.
--
-- Cancelling IN-FLIGHT EXTERNAL work (a running agent turn) is the Subagent
-- cycle's business: dropping the residual stops the thread, and whatever it was
-- awaiting settles through that cycle's own typed terminal states.
--
-- == Deviations from @Control.Concurrent.Async@
--
-- Names and semantics track the package; each deviation below is one the
-- substrate forces, and is documented again at its definition.
--
-- * 'waitEither' \/ 'waitAny' DISCARD the loser's residual (there is no
--   detached execution to leave it running in) — i.e. @waitEitherCancel@
--   semantics under @waitEither@'s signature.  Use 'waitEitherResume' \/
--   'waitAnyResume' to keep the losers.
-- * 'poll' reports @Just@ only for an already-settled thread; it cannot
--   reflect background progress, because there is none.
-- * 'waitCatch' ranges over 'AsyncCancelled' only.  A thread's own failure is
--   an ordinary 'Either' in its result type — the row's discipline — not an
--   exception.
--
-- This module is reachable only in rows containing @Green@ (it builds on that
-- effect's substrate verbs), and is auto-imported whenever @Green@ is in the
-- row.  It has nothing to do with "Tidepool.Fork", which is the answerer's
-- unrelated fanout-to-sub-answerers surface.
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

    -- * Scheduling points
  , waitEither
  , waitBoth
  , waitAny

    -- * Residual-preserving variants
  , waitEitherResume
  , waitAnyResume
  , waitAll

    -- * Derived combinators
  , race
  , concurrently
  , mapConcurrently
  , forConcurrently

    -- * The stepping mechanism
  , Step (..)
  , stepM
  ) where

import Prelude
import Control.Monad.Freer.Internal (Eff (..), qApp, tsingleton)

import Tidepool.Effects (M, greenCancel, greenCancelled, greenNew, greenSettle)

-- | A green thread: its runtime identity, and the computation it has left to
-- run.  Opaque — the representation is free to move (see the module header on
-- the parked-frame alternative).
data Async a = Async Int (M a)

-- | The thread's runtime identity.  Stable across steps; useful for tracing.
asyncThreadId :: Async a -> Int
asyncThreadId (Async t _) = t

-- | The outcome of cancelling a thread — what 'waitCatch' reports for one.
data AsyncCancelled = AsyncCancelled
  deriving (Show, Eq)

-- | One step of a thread: either it is done, or it performed one effect and
-- has this much left.
data Step a = Finished a | Blocked (M a)

-- | Perform __exactly one__ effect of @m@ and hand back the residual.
--
-- @E u (tsingleton Val)@ re-sends the very effect @m@ was about to perform, as
-- a computation of its own: one suspension, serviced by the driver, resumed
-- with the answer.  @qApp q x@ is then the rest of @m@.  This is the whole
-- scheduler mechanism.
stepM :: M a -> M (Step a)
stepM m = case m of
  Val a -> pure (Finished a)
  E u q -> do
    x <- E u (tsingleton Val)
    pure (Blocked (qApp q x))

-- | Create a green thread.  Performs no effect of the body: a thread suspends
-- at its very first effect, and every outer effect suspends, so "run it until
-- it blocks" is "do nothing yet".  Progress happens at scheduling points.
async :: M a -> M (Async a)
async body = do
  tid <- greenNew
  pure (Async tid body)

-- | Drive a thread to completion and return its value.  Fails loudly if the
-- thread was cancelled — 'waitCatch' is the total form.
wait :: Async a -> M a
wait h = do
  r <- waitCatch h
  case r of
    Right a -> pure a
    Left AsyncCancelled ->
      error "Tidepool.Async.wait: thread was cancelled (use waitCatch)"

-- | Drive a thread to completion, reporting cancellation as data.  The
-- cancelled bit is read once, at entry (see the module header).
waitCatch :: Async a -> M (Either AsyncCancelled a)
waitCatch (Async tid m) = do
  cancelled <- greenCancelled tid
  if cancelled
    then pure (Left AsyncCancelled)
    else do
      a <- drive tid m
      pure (Right a)

-- | Run a residual to completion, settling the thread when it lands.
drive :: Int -> M a -> M a
drive tid m = do
  s <- stepM m
  case s of
    Finished a -> do
      greenSettle tid
      pure a
    Blocked m' -> drive tid m'

-- | Has this thread already settled?  Performs no effect and never advances
-- the thread: with no background execution there is nothing to poll for
-- beyond a value that is already there.
poll :: Async a -> M (Maybe a)
poll (Async _ m) = case m of
  Val a -> pure (Just a)
  E _ _ -> pure Nothing

-- | Mark a thread cancelled.  Idempotent.  A holder that later 'wait's it gets
-- 'AsyncCancelled'; the residual is dropped, so its pending suspensions never
-- reach the driver.
cancel :: Async a -> M ()
cancel (Async tid _) = greenCancel tid

-- | Race two threads, stepping them alternately.  __The loser's residual is
-- discarded__ (and its thread marked cancelled) — there is no detached
-- execution to leave it running in.  'waitEitherResume' keeps it.
waitEither :: Async a -> Async b -> M (Either a b)
waitEither ha hb = do
  r <- waitEitherResume ha hb
  case r of
    Left (a, loser) -> do
      cancel loser
      pure (Left a)
    Right (b, loser) -> do
      cancel loser
      pure (Right b)

-- | 'waitEither', handing back the loser's residual so the caller can keep
-- driving it.
waitEitherResume :: Async a -> Async b -> M (Either (a, Async b) (b, Async a))
waitEitherResume (Async ta ma) (Async tb mb) = go ma mb
  where
    go x y = do
      sx <- stepM x
      case sx of
        Finished a -> do
          greenSettle ta
          pure (Left (a, Async tb y))
        Blocked x' -> do
          sy <- stepM y
          case sy of
            Finished b -> do
              greenSettle tb
              pure (Right (b, Async ta x'))
            Blocked y' -> go x' y'

-- | Drive both threads to completion, stepping them alternately.
waitBoth :: Async a -> Async b -> M (a, b)
waitBoth ha hb = do
  r <- waitEitherResume ha hb
  case r of
    Left (a, hb') -> do
      b <- wait hb'
      pure (a, b)
    Right (b, ha') -> do
      a <- wait ha'
      pure (a, b)

-- | The first of these threads to finish, stepping them in list order.
-- __The others' residuals are discarded__ (see 'waitEither');
-- 'waitAnyResume' keeps them.
waitAny :: [Async a] -> M (Async a, a)
waitAny hs = do
  (winner, a, rest) <- waitAnyResume hs
  mapM_ cancel rest
  pure (winner, a)

-- | 'waitAny', handing back the losers' residuals in their original order.
waitAnyResume :: [Async a] -> M (Async a, a, [Async a])
waitAnyResume [] = error "Tidepool.Async.waitAny: empty thread list"
waitAnyResume hs0 = pass hs0 []
  where
    -- One step of each thread, in order; the first to finish wins.  An
    -- exhausted pass starts the next round with the stepped residuals, order
    -- preserved.
    pass [] stepped = pass (reverse stepped) []
    pass (Async t m : rest) stepped = do
      s <- stepM m
      case s of
        Finished a -> do
          greenSettle t
          pure (Async t (Val a), a, reverse stepped ++ rest)
        Blocked m' -> pass rest (Async t m' : stepped)

-- | Drive every thread to completion, one step each per round, and collect
-- their values __in the original order__ — completion order is never an input
-- (PRD 20's order-insensitivity contract).
waitAll :: [Async a] -> M [a]
waitAll hs = loop (map (\(Async t m) -> Live t m) hs)
  where
    loop slots = do
      slots' <- mapM stepSlot slots
      if any isLive slots'
        then loop slots'
        else mapM valueOf slots'

    stepSlot s@(Settled _ _) = pure s
    stepSlot (Live t m) = do
      st <- stepM m
      case st of
        Finished a -> do
          greenSettle t
          pure (Settled t a)
        Blocked m' -> pure (Live t m')

    isLive (Live _ _) = True
    isLive (Settled _ _) = False

    valueOf (Settled _ a) = pure a
    valueOf (Live _ _) = error "Tidepool.Async.waitAll: unreachable live slot"

-- | One thread's state inside 'waitAll'.
data Slot a = Live Int (M a) | Settled Int a

-- | Run two computations concurrently, returning the first to finish; the
-- other is cancelled.
race :: M a -> M b -> M (Either a b)
race l r = do
  ha <- async l
  hb <- async r
  waitEither ha hb

-- | Run two computations concurrently, returning both values.
concurrently :: M a -> M b -> M (a, b)
concurrently l r = do
  ha <- async l
  hb <- async r
  waitBoth ha hb

-- | Map concurrently: one thread per element, stepped round-robin, results in
-- the original order.
mapConcurrently :: (a -> M b) -> [a] -> M [b]
mapConcurrently f xs = do
  hs <- mapM (async . f) xs
  waitAll hs

-- | 'mapConcurrently' with the arguments flipped — @forConcurrently xs f@.
forConcurrently :: [a] -> (a -> M b) -> M [b]
forConcurrently xs f = mapConcurrently f xs
