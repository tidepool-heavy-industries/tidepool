{-# LANGUAGE OverloadedStrings #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools

agentSpec :: (KnownToolEffects effects, AsyncEffects effects) => AgentSpec (Tools.SpecTools effects) effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just noted
  }

noted :: ToolCall -> ToolResult -> Eff effects Annotation
noted call _
  | toolCallName call /= "probe" = pure NoAnnotation
  | otherwise = pure (Annotated "SLOT_GENERATION")
