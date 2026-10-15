{-# LANGUAGE FlexibleContexts #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools
import Tidepool.Effects.Core (ActorContext, BoundWorktree, Commands, Jev, Lookup, Console, Notifications, Reflect)
import Tidepool.Agent.Reply (Replies)

-- The lookup campaign uses the shipped tools with the default no-hook policy.
agentSpec ::
  ( KnownToolEffects effects, AsyncEffects effects
  , Member Commands effects, Member Lookup effects, Member Console effects, Member Jev effects
  , Member ActorContext effects, Member Notifications effects, Member Reflect effects
  , Member Replies effects, Member BoundWorktree effects
  ) =>
  AgentSpec (Tools.WorkspaceTools effects) effects
agentSpec = defaultSpec {specTools = Tools.tools}
