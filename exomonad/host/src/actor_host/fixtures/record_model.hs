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
import Tidepool.Aeson (FromJSON, JsonSchema)
import Tidepool.Agent.Contract (defaultSpec)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Effects.Row (knownEffects)
import Tidepool.Model

newtype Answer = Answer { answer :: Int }
  deriving (Generic, FromJSON, JsonSchema)

data Probe mode = Probe
  { calls :: mode R.:- R.State Int
  , runModel :: mode R.:- R.Call Text (R.Reply (ModelResult Answer))
  , readCalls :: mode R.:- R.Call () (R.Reply Int)
  } deriving Generic

probe :: forall modelFamily. KnownFamily modelFamily
      => Model modelFamily -> Bool -> R.ActorSpec Probe '[ModelCall modelFamily]
probe model granted = permissions $
  R.definition "record-model-probe" (Actor.Selected (knownEffects @'[ModelCall modelFamily])) Probe
    { calls = 0
    , runModel = \input -> do
        result <- invokeModel
          (withLimits (defaultLimits { requestLimit = Just 1, toolLimit = Just 1 })
            (typedTurn @Answer model defaultSpec "record-typed")) input
        R.modify' (+1)
        pure result
    , readCalls = \() -> R.get
    }
  where
    permissions = if granted
      then R.withModelPermissions (modelPermissions [allowModelCall model])
      else id
