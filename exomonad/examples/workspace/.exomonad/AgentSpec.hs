{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools
import Tidepool.Effects.Core (ActorContext, Commands, Jev, Lookup, Notifications, Reflect)
import qualified Project.Watchdog as Watchdog

agentSpec ::
  ( Member Commands effects, Member Lookup effects, Member Jev effects
  , Member ActorContext effects, Member Notifications effects, Member Reflect effects
  ) =>
  AgentSpec Tools.WorkspaceTools effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just (Watchdog.watchBy monitorsFor)
  }

-- The parent chooses the label when it creates the child. These labels install
-- an escalation or a nudge; other children, including this workspace's root,
-- abstain before asking Jev.
monitorsFor :: Text -> [Watchdog.Heuristic]
monitorsFor path
  | "escalate-child" `T.isInfixOf` path = [Watchdog.outOfScope]
  | "nudge-child" `T.isInfixOf` path = [Watchdog.repeatingItself]
  | otherwise = []
