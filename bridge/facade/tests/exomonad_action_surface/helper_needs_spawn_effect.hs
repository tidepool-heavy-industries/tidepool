{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module HelperNeedsSpawnEffect where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

spawnHelper :: Text -> Eff '[Replies, Watches] (Either SpawnError AgentRef)
spawnHelper prompt = spawnSubagent (FreshCtx prompt) SameDir
  (defaultSpawnOptions minimalSpec)
