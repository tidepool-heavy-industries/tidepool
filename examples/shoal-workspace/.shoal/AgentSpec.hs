{-# LANGUAGE FlexibleContexts #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools
import Tidepool.Effects.Core (Commands, Lookup, Jev, Reflect)

agentSpec ::
  (Member Commands effects, Member Lookup effects, Member Jev effects, Member Reflect effects) =>
  AgentSpec Tools.WorkspaceTools effects
agentSpec = defaultSpec {specTools = Tools.tools}
