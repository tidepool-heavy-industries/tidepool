{-# LANGUAGE DataKinds #-}

module CannotControlWithoutAgentControl where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

type NarrowEffects = '[Replies, ActorContext]

cancelNeedsControl :: AgentRef -> Eff NarrowEffects StopOutcome
cancelNeedsControl = stopAgent
