{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module MinimalSpawn where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Authored (Console)

type MinimalSpec = AgentSpec NoTools '[Console]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

spawnMinimal :: Eff '[AgentLaunch] (Either SpawnError AgentRef)
spawnMinimal = spawnSubagent (FreshCtx "inspect the repository") SameDir
  (defaultSpawnOptions minimalSpec)
