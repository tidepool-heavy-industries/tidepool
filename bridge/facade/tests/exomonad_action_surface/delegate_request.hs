{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

module DelegateRequest where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Prelude
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad hiding (result)
import qualified Tidepool.Actors.Exomonad as Exomonad

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

data DelegationError
  = SpawnFailed SpawnError
  | RequestFailed AgentRef RequestError
  | AwaitFailed AgentRef AwaitError

delegateAndAwait :: Text -> Eff ActorEffects (Either DelegationError Text)
delegateAndAwait input = do
  spawned <- spawnSubagent (FreshCtx "answer the next typed request") SameDir
    (defaultSpawnOptions minimalSpec)
  case spawned of
    Left issue -> pure (Left (SpawnFailed issue))
    Right agent -> do
      admitted <- request @Text agent input defaultRequestOptions
      case admitted of
        Left issue -> pure (Left (RequestFailed agent issue))
        Right pending -> do
          observed <- await (Exomonad.result pending)
          pure (either (Left . AwaitFailed agent) Right observed)

delegateAndWatch
  :: Text
  -> Eff ActorEffects (Either DelegationError (AgentRef, Watch Text))
delegateAndWatch input = do
  spawned <- spawnSubagent (FreshCtx "answer the next typed request") SameDir
    (defaultSpawnOptions minimalSpec)
  case spawned of
    Left issue -> pure (Left (SpawnFailed issue))
    Right agent -> do
      admitted <- request @Text agent input defaultRequestOptions
      case admitted of
        Left issue -> pure (Left (RequestFailed agent issue))
        Right pending -> do
          observed <- watch (Just "answer ready") (Exomonad.result pending)
          pure (Right (agent, observed))
