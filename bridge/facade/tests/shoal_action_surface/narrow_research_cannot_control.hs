{-# LANGUAGE DataKinds #-}

module NarrowResearchCannotControl where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Shoal

type NarrowResearch = '[Replies, ActorContext]

result :: AgentRef -> Eff NarrowResearch StopOutcome
result = stopAgent
