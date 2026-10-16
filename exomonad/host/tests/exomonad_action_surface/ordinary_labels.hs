{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module OrdinaryLabels where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

spawnWithOrdinaryLabel :: Text -> Eff ActorEffects (Either SpawnError AgentRef)
spawnWithOrdinaryLabel label = spawnSubagent (FreshCtx "inspect") SameDir
  ((defaultSpawnOptions minimalSpec) { spawnLabel = Just label })

requestWithOrdinaryLabel
  :: AgentRef
  -> Text
  -> Eff ActorEffects (Either RequestError (Request Text))
requestWithOrdinaryLabel agent label = request agent ("check this" :: Text)
  (defaultRequestOptions { requestLabel = Just label })

watchWithOrdinaryLabel
  :: Text
  -> Await Text
  -> Eff ActorEffects (Watch Text)
watchWithOrdinaryLabel label = watch (Just label)
