{-# LANGUAGE FlexibleContexts #-}

-- | This checkout's agent spec: the tools its agent is offered, and what runs
-- after each of its tool calls. Edit it, then call @reload_agent_spec@.
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Project.Tools as Tools
import qualified Project.Watchdog as Watchdog
import Tidepool.Effects.Core (ActorContext, Jev, Notifications, Reflect)

agentSpec
  :: ( Member Cmd.Commands effects
     , Member Jev effects
     , Member ActorContext effects
     , Member Notifications effects
     , Member Reflect effects
     )
  => AgentSpec Tools.WorkspaceTools effects
agentSpec =
  defaultSpec
    { specTools = Tools.tools
    , afterTool = Just (Watchdog.watchChildrenWith defaultHeuristics)
    }

-- Scope drift needs the child's assignment in its evidence packet, which the
-- generic after-tool input does not carry. Install it through an
-- assignment-aware 'watchBy' selector instead of guessing here.
defaultHeuristics :: [Watchdog.Heuristic]
defaultHeuristics =
  Watchdog.coreHeuristics
    <> [Watchdog.ignoringAFailure, Watchdog.guessingInsteadOfReading]
