{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module IndependentChildren where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

-- Both admissions are ordinary calls; labels may repeat and carry no identity.
spawnPair :: Text -> Eff ActorEffects (Either SpawnError AgentRef, Either SpawnError AgentRef)
spawnPair label = do
  let options = (defaultSpawnOptions minimalSpec) { spawnLabel = Just label }
  left <- spawnSubagent (FreshCtx "review the interface") SameDir options
  right <- spawnSubagent (FreshCtx "review the tests") SameDir options
  pure (left, right)

askBoth
  :: AgentRef
  -> AgentRef
  -> Eff ActorEffects (Either RequestError (Request Text), Either RequestError (Request Text))
askBoth first second = do
  firstRequest <- request first ("inspect the API" :: Text) defaultRequestOptions
  secondRequest <- request second ("inspect the tests" :: Text) defaultRequestOptions
  pure (firstRequest, secondRequest)
