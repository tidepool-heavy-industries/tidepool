{-# LANGUAGE DataKinds #-}

module MinimalSpawn where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

spawnMinimal :: Eff ActorEffects (Either SpawnError AgentRef)
spawnMinimal = spawnSubagent (FreshCtx "inspect the repository") SameDir
  (defaultSpawnOptions minimalSpec)
