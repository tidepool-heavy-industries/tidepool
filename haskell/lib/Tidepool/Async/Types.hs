-- | The green-thread HANDLE vocabulary, split from "Tidepool.Async" so the
-- two modules built on it stay independently loadable:
--
-- * "Tidepool.Async" (the verbs: @async@\/@wait@\/…) rides @Green@'s
--   substrate and compiles only in rows containing @Green@.
-- * "Tidepool.Event"'s 'Tidepool.Event.waitEvent' (the completion watch)
--   rides @RepoEvent@'s substrate and compiles only in rows containing
--   @RepoEvent@.
--
-- Before this split, @waitEvent@ lived in "Tidepool.Async" and imported
-- "Tidepool.Event" — which made a @Green@-without-@RepoEvent@ row (the
-- answerer window's row) unable to load "Tidepool.Async" AT ALL, over one
-- function it could never call anyway. This module imports nothing from the
-- generated effects surface, so either side loads without the other.
module Tidepool.Async.Types
  ( Async (..)
  , asyncThreadId
  , AsyncCancelled (..)
  ) where

import Prelude

-- | A handle on a green thread.  Opaque, and phantom-typed by the thread's
-- result — the same posture as @AgentHandle@.
newtype Async a = Async Int

-- | The thread's runtime identity.  Stable for the thread's life; useful for
-- tracing.
asyncThreadId :: Async a -> Int
asyncThreadId (Async t) = t

-- | The outcome of cancelling a thread — what
-- 'Tidepool.Async.waitCatch' reports for one.
data AsyncCancelled = AsyncCancelled
  deriving (Show, Eq)
