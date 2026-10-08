{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module SpawnContextWorkspaceChoices where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

freshFork :: Text -> Eff ActorEffects (Either SpawnError AgentRef)
freshFork prompt = spawnSubagent (FreshCtx prompt) (ForkWorktree projectHead)
  (defaultSpawnOptions minimalSpec)

forkFromCheckpoint
  :: ContextCheckpoint
  -> Workspace
  -> Eff ActorEffects (Either SpawnError AgentRef)
forkFromCheckpoint captured workspace =
  spawnSubagent (ForkCtx captured) workspace (defaultSpawnOptions minimalSpec)

sharedDirectory :: Eff ActorEffects (Either SpawnError AgentRef)
sharedDirectory = spawnSubagent (FreshCtx "inspect current files") SameDir
  (defaultSpawnOptions minimalSpec)

retainedWorkspace
  :: WorkspaceHandle
  -> Eff ActorEffects (Either SpawnError AgentRef)
retainedWorkspace handle = spawnSubagent (FreshCtx "continue in this workspace")
  (ExistingWorkspace handle) (defaultSpawnOptions minimalSpec)
