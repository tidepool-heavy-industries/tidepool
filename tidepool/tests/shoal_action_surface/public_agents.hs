{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

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
  -> RequestLabel
  -> input
  -> Eff ActorEffects (Response result)
submit = request

submitWithOnlyReplies
  :: AgentRef
  -> RequestLabel
  -> input
  -> Eff '[Replies] (Response result)
submitWithOnlyReplies = request

compose
  :: Response left
  -> Response right
  -> Await (ResponseResult left, ResponseResult right)
compose left right = (,) <$> awaitResponse left <*> awaitResponse right

watchBoth
  :: WatchLabel
  -> Response left
  -> Response right
  -> Eff ActorEffects (Watch (ResponseResult left, ResponseResult right))
watchBoth label left right = watch label (compose left right)

stop :: AgentRef -> Eff ActorEffects StopOutcome
stop = stopAgent

inspect :: AgentRef -> Eff ActorEffects AgentObservation
inspect = observeAgent

heterogeneousUnfold
  :: ForkGroupPath
  -> BranchLabel
  -> BranchLabel
  -> Eff ActorEffects (Forked Text, Forked Int)
heterogeneousUnfold group textLeaf intLeaf =
  unfold group $
    (,)
      <$> child (researching @Text textLeaf projectHead ())
      <*> child (coding @Int intLeaf projectHead ())

homogeneousUnfold
  :: ForkGroupPath
  -> [BranchLabel]
  -> Eff ActorEffects [Forked Text]
homogeneousUnfold group leaves =
  unfold group $
    traverse
      (\leaf -> child (researching @Text leaf projectHead ()))
      leaves

result :: Int
result = 42
