{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module RecordModelFixture where

import Prelude
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON)
import Tidepool.Agent.Contract (JsonSchema, defaultSpec, specTools, presentWith, tool)
import ModelFixture (EchoArgs (..), EchoTools (..))
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Effects.Row (KnownEffect, knownEffects)
import Tidepool.Model

data Answer = Answer { answer :: Int }
  deriving (Generic, FromJSON, JsonSchema)

data Probe mode = Probe
  { calls :: mode R.:- R.State Int
  , runModel :: mode R.:- R.Call Text (R.Reply (ModelResult Answer, ModelResult Answer))
  , readCalls :: mode R.:- R.Call () (R.Reply Int)
  } deriving Generic

probe :: forall modelFamily. (KnownFamily modelFamily, KnownEffect (ModelCall modelFamily))
      => Model modelFamily -> Bool -> R.ActorSpec Probe (R.LocalEffects Probe '[ModelCall modelFamily])
probe model granted = permissions $
  R.definition "record-model-probe" (Actor.Selected (knownEffects @'[ModelCall modelFamily])) Probe
    { calls = 0
    , runModel = \input -> do
        let bounded = withLimits (defaultLimits { requestLimit = Just 2, toolLimit = Just 2 })
              (typedTurn @Answer model
                (defaultSpec { specTools = EchoTools (presentWith id $ tool "Echo through the record actor state" (\args -> do
                  R.modify' (+10)
                  pure (message args))) }) "record-typed")
        result <- invokeModel bounded input
        exhausted <- invokeModel bounded "budget control"
        R.modify' (+1)
        pure (result, exhausted)
    , readCalls = \() -> R.get
    }
  where
    permissions = if granted
      then R.withModelPermissions (modelPermissions [allowModelCall model])
      else id
