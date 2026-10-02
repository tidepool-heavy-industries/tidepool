{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools
import Tidepool.Effects.Core (BoundWorktree, Commands, Jev, Lookup, Reflect)
import Tidepool.Agent.Reply (Replies)

agentSpec ::
  ( KnownToolEffects effects, AsyncEffects effects
  , Member Commands effects, Member Lookup effects, Member Jev effects
  , Member Reflect effects
  , Member Replies effects, Member BoundWorktree effects
  ) =>
  AgentSpec (Tools.WorkspaceTools effects) effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  }
