{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools
import Tidepool.Agent.Reply (Replies)
import Tidepool.Effects.Core (BoundWorktree, Commands, Jev, Lookup, Reflect)

data ReloadTools effects mode = ReloadTools
  { workspace :: Tools.WorkspaceTools effects mode
  , reloadSpec :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  } deriving (Generic)

-- Default tool execution has no blanket semantic monitor. Install targeted
-- task/event monitors explicitly where their evidence and benefit are bounded.
agentSpec ::
  ( KnownToolEffects effects, AsyncEffects effects
  , Member Commands effects, Member Lookup effects, Member Jev effects
  , Member Reflect effects
  , Member Replies effects, Member BoundWorktree effects
  ) =>
  AgentSpec (ReloadTools effects) effects
agentSpec = defaultSpec
  { specTools = ReloadTools Tools.tools standardReload
  }
