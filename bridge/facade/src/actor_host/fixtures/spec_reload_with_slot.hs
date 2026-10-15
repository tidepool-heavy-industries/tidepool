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
import Tidepool.Effects.Core (ActorContext, BoundWorktree, Commands, Jev, Lookup, Notifications, Reflect)
import qualified Project.Watchdog as Watchdog

data ReloadTools effects mode = ReloadTools
  { workspace :: Tools.WorkspaceTools effects mode
  , reloadSpec :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  } deriving (Generic)

-- Children do not have Journal in their effect row, so the project's
-- journal-backed nudge hook cannot be installed there. Keep the shared
-- watchdog baseline until the runtime can supply that effect.
agentSpec ::
  ( KnownToolEffects effects, AsyncEffects effects
  , Member Commands effects, Member Lookup effects, Member Jev effects
  , Member ActorContext effects, Member Notifications effects, Member Reflect effects
  , Member Replies effects, Member BoundWorktree effects
  ) =>
  AgentSpec (ReloadTools effects) effects
agentSpec = defaultSpec
  { specTools = ReloadTools Tools.tools standardReload
  , afterTool = Just (Watchdog.watchBy (const Watchdog.coreHeuristics))
  }
