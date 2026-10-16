{-# LANGUAGE DataKinds #-}

module ReusableHelperRows where

import Prelude
import Control.Monad.Freer (Eff)
import Tidepool.Effects.Authored (Green, RepoEvent, EventError, Tick)
import qualified Tidepool.Async as Async
import qualified Tidepool.Event as Event
import qualified Data.Map.Strict as Map

greenOnly :: Eff '[Green] (Either Async.AsyncCancelled Int)
greenOnly = Async.async (pure 41) >>= Async.waitCatch

greenState :: Eff '[Green] (Async.AsyncState Int)
greenState = Async.async (pure 41) >>= Async.poll

greenMap :: Eff '[Green] (Map.Map Int Int)
greenMap = Async.mapConcurrently (pure . (+ 1)) (Map.fromList [(7, 41)])

eventOnly :: Eff '[RepoEvent] (Either EventError (Event.Observed Tick))
eventOnly = Event.after 0 >>= Event.nextEventTry

eventHandler :: Eff '[RepoEvent] (Either EventError Int)
eventHandler = do
  deadline <- Event.after 0
  Event.withHandlerTry deadline (\_ -> pure ()) (pure 42)

-- Importing a helper never grants an effect to a caller.
result :: Eff '[] Int
result = pure 43
