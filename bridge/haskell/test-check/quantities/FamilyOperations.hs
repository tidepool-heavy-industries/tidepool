{-# LANGUAGE DataKinds, GADTs, OverloadedStrings #-}
module Main where

import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, run)
import Data.Text (Text)
import Tidepool.Aeson (Result (..), fromJSON, toJSON)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import qualified Tidepool.Actors.Spawn as Spawn
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Internal.ModelControl as Control
import Tidepool.Model

primary :: Model OpenAI
primary = openAI "catalog" "primary" (OpenAIOptions OpenAILow)

alternate :: Model OpenAI
alternate = openAI "catalog" "alternate" (OpenAIOptions OpenAIHigh)

bounded :: Eff '[ModelCall OpenAI] (ModelResult Text)
bounded = invokeModel
  (withModel alternate (textTurn primary (defaultSpec :: AgentSpec NoTools '[ModelCall OpenAI]) "bounded")) "input"

boundedHost :: Core.ModelCall OpenAI a -> Eff '[] a
boundedHost (Core.ModelStartWith Core.OpenAIWitness request) = case fromJSON request of
  Success (Control.ModelRequestEnvelope "bounded" "input" _ _ _ _ selected)
    | selected == modelSelection alternate -> pure (Right (toJSON
        (Control.ModelFinished (Control.ModelReceiptEnvelope "invocation" "cell" [] [] 0
          (Control.ModelUsageEnvelope 1 0 0 0) (Control.ModelUsageEnvelope 1 0 0 0)
          (Control.ModelTextOutcome "answer")))))
  _ -> error "bounded family/model/effort selection changed"
boundedHost _ = error "NoTools bounded turn requested an unexpected operation"

captured :: Eff '[Core.ForkAgent OpenAI, Core.AgentLaunch] Bool
captured = do
  saved <- Spawn.checkpoint primary "capture"
  case saved of
    Left _ -> pure False
    Right capture -> do
      launched <- Spawn.spawnSubagent (Spawn.ForkCtx capture) Spawn.SameDir
        (Spawn.defaultSpawnOptions alternate (defaultSpec :: AgentSpec NoTools '[]))
      released <- Spawn.releaseCheckpoint capture
      pure (case (launched, released) of
        (Left (Spawn.SpawnRefused "observed captured choice"), Right ()) -> True
        _ -> False)

forkHost :: Core.ForkAgent OpenAI a -> Eff '[Core.AgentLaunch] a
forkHost (Core.ForkAgentCheckpointWith Core.OpenAIWitness "capture") = pure (Right "capture-token")
forkHost (Core.ForkAgentSpawnWith Core.OpenAIWitness _ (Core.CapturedSpawn token) _ keys grants _ selection _ _ _ _)
  | token == "capture-token", null keys
  , grants == toJSON (modelPermissionRequests noModelPermissions)
  , selection == toJSON (modelSelection alternate) =
      pure (Left (Core.SpawnRefused "observed captured choice"))
forkHost _ = error "captured launch changed family, checkpoint, permissions or exact selection"

adminHost :: Core.AgentLaunch a -> Eff '[] a
adminHost (Core.AgentLaunchValidateSpecWith _ _) = pure (Right
  (Core.SpecValidationReceipt [] [] (Core.SpecProvenanceUnavailable "fixture") Core.SpecProviderNotChecked))
adminHost (Core.AgentLaunchReleaseCheckpointWith "capture-token") = pure (Right ())
adminHost _ = error "unexpected administrative operation"

main :: IO ()
main = do
  unless (case modelOutcome (run (interpret boundedHost bounded)) of
    Right "answer" -> True
    _ -> False) (error "same-family bounded model selection failed")
  unless (run (interpret adminHost (interpret forkHost captured)))
    (error "same-family captured launch or cleanup failed")
