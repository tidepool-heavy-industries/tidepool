{-# LANGUAGE DataKinds #-}

module ReusableHelperRows where

import Prelude
import Control.Monad.Freer (Eff)
import Tidepool.Effects (M)
import Tidepool.Effects.Authored (Green, RepoEvent, EventError, Tick)
import qualified Tidepool.Async as Async
import qualified Tidepool.Event as Event

greenOnly :: Eff '[Green] (Either Async.AsyncCancelled Int)
greenOnly = Async.async (pure 41) >>= Async.waitCatch

eventOnly :: Eff '[RepoEvent] (Either EventError (Event.Observed Tick))
eventOnly = Event.after 0 >>= Event.nextEventTry

eventHandler :: Eff '[RepoEvent] (Either EventError Int)
eventHandler = do
  deadline <- Event.after 0
  Event.withHandlerTry deadline (\_ -> pure ()) (pure 42)

-- The selected shim is empty; importing the helpers and describing an event
-- requires no Green or RepoEvent grant.
result :: M Int
result = Event.after 0 >> pure 43
