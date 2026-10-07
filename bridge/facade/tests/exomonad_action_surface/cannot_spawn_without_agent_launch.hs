{-# LANGUAGE DataKinds #-}

module CannotSpawnWithoutAgentLaunch where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

-- The empty child capability row does not grant the caller spawn authority.
branchOnSpawnAuthority :: Eff '[Replies, Watches] (Either SpawnError AgentRef)
branchOnSpawnAuthority = spawnSubagent (FreshCtx "inspect") SameDir
  (defaultSpawnOptions minimalSpec)
