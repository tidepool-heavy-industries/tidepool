{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Actors.Exomonad hiding (Call, (:-))
import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Command as Cmd

data LifetimeProbe = LifetimeProbe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

data LifetimeTools effects mode = LifetimeTools
  { lifetimeProbe :: mode :- Call LifetimeProbe Text
  , notebook :: HaskellTools effects mode
  }
  deriving Generic

agentSpec :: (KnownToolEffects effects, AsyncEffects effects, Member AgentLaunch effects, Member Cmd.Commands effects)
          => AgentSpec (LifetimeTools effects) effects
agentSpec = defaultSpec
  { specTools = LifetimeTools
      { lifetimeProbe = presentWith id $ tool "Exercise the invocation lifetime boundary." (\_ -> pure "original-result")
      , notebook = haskellTools
      }
  , afterTool = Just afterProbe
  }

afterProbe :: (Member AgentLaunch effects, Member Cmd.Commands effects)
           => ToolCall -> ToolResult -> Eff effects Annotation
afterProbe call _
  | toolCallName call /= "lifetime_probe" = pure NoAnnotation
  | otherwise = do
      _ <- spawnSubagent (FreshCtx "invocation deadline child") SameDir
        ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
          { spawnLabel = Just "invocation-deadline-child", spawnLifetime = InvocationOwned })
      _ <- Cmd.quiet (Cmd.run (Cmd.argv ["printf", "unreachable-slot-result"]))
      pure (Annotated "slot-finished")
