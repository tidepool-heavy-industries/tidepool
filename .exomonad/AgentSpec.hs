{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | This checkout's agent spec: the tools its agent is offered, and what runs
-- after each of its tool calls. Edit it, then call @reload_agent_spec@.
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Project.Tools as Tools

agentSpec :: Member Cmd.Commands effects => AgentSpec Tools.WorkspaceTools effects
agentSpec =
  defaultSpec
    { specTools = Tools.tools
    , afterTool = Just afterEachTool
    }

-- | Shown every finished tool call and its result. It says nothing yet.
afterEachTool :: ToolCall -> ToolResult -> Eff effects Annotation
afterEachTool _ _ = pure (Abstained "no after-tool logic has been written yet")
