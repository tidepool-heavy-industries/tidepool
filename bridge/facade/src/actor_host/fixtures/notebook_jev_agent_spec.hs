{-# LANGUAGE FlexibleContexts #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev)

agentSpec ::
  (KnownToolEffects effects, AsyncEffects effects, Member Jev effects) =>
  AgentSpec (HaskellTools effects) effects
agentSpec = defaultWorkbenchSpec
