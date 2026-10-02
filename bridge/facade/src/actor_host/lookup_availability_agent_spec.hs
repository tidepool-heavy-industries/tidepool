{-# LANGUAGE FlexibleContexts #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract (AgentSpec (..), defaultSpec)
import Tidepool.Effects.Core (Lookup)
import qualified Tidepool.Lookup.Tools as LookupTools

agentSpec :: Member Lookup effects => AgentSpec LookupTools.LookupTools effects
agentSpec = defaultSpec {specTools = LookupTools.tools}
