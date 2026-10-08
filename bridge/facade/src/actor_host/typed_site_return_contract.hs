{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeApplications #-}

module TypedSiteReturnContract where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Tidepool.Actor as Actor
import Tidepool.Agent.Reply (Replies, Request, RequestError, RequestOptions, Progress)
import Tidepool.Actors.Exomonad (AgentRef)
import qualified Tidepool.Actors.Internal.Agent as Agent
import Tidepool.Effects.Core (ActorLocal)

data Protocol result where
  Ping :: Protocol Int

receiveProbe :: Eff '[ActorLocal Protocol] Bool
receiveProbe = Actor.receive handler
  where
    handler :: Protocol result -> Eff '[ActorLocal Protocol] (result, Bool)
    handler Ping = pure (7, True)

requestProbe :: AgentRef -> Text -> RequestOptions -> Eff '[Replies] (Either RequestError (Request Int))
requestProbe target input options = Agent.request @Int target input options

progressProbe :: AgentRef -> Text -> RequestOptions -> Eff '[Replies] (Either RequestError (Request Int, Progress Text))
progressProbe target input options = Agent.requestWithProgress @Text @Int target input options

retainedProgressProbe :: AgentRef -> Text -> RequestOptions -> Eff '[Replies] (Either RequestError (Request Int, Progress Text))
retainedProgressProbe target input options = Agent.requestWithProgress @Text @Int target input options
