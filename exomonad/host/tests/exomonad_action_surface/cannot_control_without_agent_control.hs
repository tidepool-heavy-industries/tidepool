{-# LANGUAGE DataKinds #-}

module CannotControlWithoutAgentControl where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

type NarrowEffects = '[Replies, ActorContext]

stopNeedsControl :: AgentRef -> Eff NarrowEffects StopOutcome
stopNeedsControl = stopAgent
