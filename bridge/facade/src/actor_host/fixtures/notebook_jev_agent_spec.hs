{-# LANGUAGE FlexibleContexts #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev, ModelCall, Reflect)

agentSpec ::
  ( KnownToolEffects effects
  , AsyncEffects effects
  , Member Jev effects
  , Member ModelCall effects
  , Member Reflect effects
  ) =>
  AgentSpec (HaskellTools effects) effects
agentSpec = defaultWorkbenchSpec
