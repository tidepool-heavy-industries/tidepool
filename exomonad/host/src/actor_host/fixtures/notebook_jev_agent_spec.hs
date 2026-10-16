{-# LANGUAGE FlexibleContexts #-}

module AgentSpec (agentSpec) where

import Tidepool.Agent.Contract

agentSpec ::
  ( KnownToolEffects effects
  , AsyncEffects effects
  ) =>
  AgentSpec (WorkbenchTools effects) effects
agentSpec = defaultWorkbenchSpec
