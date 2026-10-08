{-# LANGUAGE FlexibleContexts #-}
module AgentSpec (agentSpec) where

import Tidepool.Agent.Contract

agentSpec :: (KnownToolEffects effects, AsyncEffects effects)
          => AgentSpec (HaskellTools effects) effects
agentSpec = defaultWorkbenchSpec
