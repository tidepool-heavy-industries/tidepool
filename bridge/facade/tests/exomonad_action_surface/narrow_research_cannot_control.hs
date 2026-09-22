{-# LANGUAGE DataKinds #-}

module NarrowResearchCannotControl where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

type NarrowResearch = '[Replies, ActorContext]

result :: AgentRef -> Eff NarrowResearch StopOutcome
result = stopAgent
