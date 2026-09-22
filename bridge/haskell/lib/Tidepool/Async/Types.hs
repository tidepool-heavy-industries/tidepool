-- | Opaque green-thread handles shared by "Tidepool.Async" and
-- "Tidepool.Event" without coupling their independent effect rows.
module Tidepool.Async.Types
  ( Async
  , asyncThreadId
  , AsyncCancelled (..)
  ) where

import Prelude

import Tidepool.Async.Internal (Async, asyncThreadId)

-- | The outcome of cancelling a thread — what
-- 'Tidepool.Async.waitCatch' reports for one.
data AsyncCancelled = AsyncCancelled
  deriving (Show, Eq)
