{-# LANGUAGE DataKinds #-}

module ShoalPublicAgents where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Prelude
import Tidepool.Actors.Shoal

startCoding :: WorktreeHandle -> Eff ActorEffects AgentRef
startCoding = startAgent . codingAgent

startReview :: Text -> Eff ActorEffects AgentRef
startReview = startAgent . readonlyAgent

submit
  :: AgentRef
  -> Text
  -> input
  -> Eff ActorEffects (Response result)
submit = request

submitWithOnlyReplies
  :: AgentRef
  -> Text
  -> input
  -> Eff '[Replies] (Response result)
submitWithOnlyReplies = request

compose
  :: Response left
  -> Response right
  -> Await (left, right)
compose left right = (,) <$> awaitResponse left <*> awaitResponse right

watchBoth
  :: Response left
  -> Response right
  -> Eff ActorEffects (Watch (left, right))
watchBoth left right = watch (compose left right)

stop :: AgentRef -> Eff ActorEffects ()
stop = stopAgent

result :: Int
result = 42
