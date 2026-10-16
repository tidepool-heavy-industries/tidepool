{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools
import Tidepool.Agent.Reply (Replies)
import Tidepool.Effects.Core (BoundWorktree, Commands, Console, Jev, Lookup, Reflect)

-- Default tool execution has no blanket semantic monitor. Install targeted
-- task/event monitors explicitly where their evidence and benefit are bounded.
agentSpec ::
  ( KnownToolEffects effects, AsyncEffects effects
  , Member Commands effects, Member Lookup effects, Member Console effects, Member Jev effects
  , Member Reflect effects
  , Member Replies effects, Member BoundWorktree effects
  ) =>
  AgentSpec (Tools.WorkspaceTools effects) effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  }
