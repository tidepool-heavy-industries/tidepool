{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeApplications #-}

module TypedSiteReturnContract where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Tidepool.Actor as Actor
import Tidepool.Agent.Reply (Replies, Response, Progress)
import Tidepool.Actors.Internal.Agent (AgentRef, Assignment)
import qualified Tidepool.Actors.Internal.Agent as Agent
import qualified Tidepool.Actors.Unfold as Unfold
import Tidepool.Effects.Core (ActorLocal)

data Protocol result where
  Ping :: Protocol Int

receiveProbe :: Eff '[ActorLocal Protocol] Bool
receiveProbe = Actor.receive handler
  where
    handler :: Protocol result -> Eff '[ActorLocal Protocol] (result, Bool)
    handler Ping = pure (7, True)

requestProbe :: AgentRef -> Assignment Text -> Eff '[Replies] (Response Int)
requestProbe target input = Agent.request @Int target input

requestWithProbe :: AgentRef -> Assignment Text -> Eff '[Replies] (Response Int)
requestWithProbe target input = Agent.requestWith @Int target input

progressProbe :: AgentRef -> Assignment Text -> Eff '[Replies] (Response Int, Progress Text)
progressProbe target input = Agent.requestWithProgress @Text @Int target input

retainedProgressProbe :: AgentRef -> Assignment Text -> Eff '[Replies] (Response Int, Progress Text)
retainedProgressProbe target input =
  Agent.requestWithProgressInto @Text @Int target input (const (pure ()))

childProbe :: Unfold.Branch '[] Text Int -> Unfold.Unfold '[] (Response Int)
childProbe branch = Unfold.child @Int branch

childProgressProbe :: Unfold.Branch '[] Text Int -> Unfold.Unfold '[] (Response Int, Progress Text)
childProgressProbe branch = Unfold.childWithProgress @Text @Int branch
