{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Actors.Exomonad
  ( AgentLaunch, WorkerLifetime (..), readonlyAgent, startAgent, withAgentLifetime )
import qualified Tidepool.Command as Cmd

newtype LifetimeTools mode = LifetimeTools
  { lifetimeProbe :: mode :- Call () Text }
  deriving Generic

agentSpec :: (Member AgentLaunch effects, Member Cmd.Commands effects)
          => AgentSpec LifetimeTools effects
agentSpec = defaultSpec
  { specTools = LifetimeTools
      { lifetimeProbe = tool "Exercise the invocation lifetime boundary." (\() -> pure "original-result") }
  , afterTool = Just afterProbe
  }

afterProbe :: (Member AgentLaunch effects, Member Cmd.Commands effects)
           => ToolCall -> ToolResult -> Eff effects Annotation
afterProbe call _
  | toolCallName call /= "lifetimeProbe" = pure NoAnnotation
  | otherwise = do
      _ <- startAgent (withAgentLifetime InvocationOwned (readonlyAgent "invocation-deadline-child"))
      _ <- Cmd.quiet (Cmd.run (Cmd.argv ["printf", "unreachable-slot-result"]))
      pure (Annotated "slot-finished")
