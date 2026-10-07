{-# LANGUAGE FlexibleContexts #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Effects.Core (Sleep)

agentSpec :: Member Sleep effects => AgentSpec NoTools effects
agentSpec = defaultSpec
