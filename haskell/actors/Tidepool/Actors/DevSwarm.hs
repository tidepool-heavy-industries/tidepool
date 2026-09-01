{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | The first self-hosting actor policy.
--
-- Rust owns panes, processes, scheduling, and delivery. This module owns the
-- small typed interaction surface presented to the root and worker agents.
module Tidepool.Actors.DevSwarm
  ( RootEffects
  , rootPolicy
  ) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Prelude

import Tidepool.Actor
import Tidepool.Agent.Contract
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Effects.Core (Actor, ActorMcp)

type RootEffects = '[ActorMcp, Actor]

data StatusInput = StatusInput
  deriving (Generic, FromJSON, JsonSchema)

data ActorStatus = ActorStatus
  { role :: Text
  , detail :: Text
  }
  deriving (Generic, ToJSON)

data SpawnWorker = SpawnWorker
  { assignment :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

data WorkerStarted = WorkerStarted
  { accepted :: Bool
  }
  deriving (Generic, ToJSON)

data AssignmentInput = AssignmentInput
  deriving (Generic, FromJSON, JsonSchema)

data WorkerAssignment = WorkerAssignment
  { assignment :: Text
  }
  deriving (Generic, ToJSON)

data RootTools mode = RootTools
  { actorStatus :: mode :- Call StatusInput ActorStatus
  , spawnWorker :: mode :- Call SpawnWorker WorkerStarted
  }
  deriving (Generic)

data WorkerTools mode = WorkerTools
  { actorStatus :: mode :- Call StatusInput ActorStatus
  , currentAssignment :: mode :- Call AssignmentInput WorkerAssignment
  }
  deriving (Generic)

workerDefinition :: ActorDefinition Text Maybe ()
workerDefinition =
  ActorDefinition
    { label = "devswarm-worker"
    , effectProfile = ReadOnly
    , initialization = pure
    , behavior = \_ assignmentText ->
        serveTools
          WorkerTools
            { actorStatus =
                tool "Describe this worker actor." $ \_ ->
                  pure (ActorStatus "worker" "ready")
            , currentAssignment =
                tool "Return this worker's typed startup assignment." $ \_ ->
                  pure (WorkerAssignment assignmentText)
            }
    , visibleToChild = []
    , onShutdown = const (pure ())
    }

rootPolicy :: Eff RootEffects a
rootPolicy =
  serveTools
    RootTools
      { actorStatus =
          tool "Describe the root actor and its current role." $ \_ ->
            pure (ActorStatus "root" "ready to unfold work into typed workers")
      , spawnWorker =
          tool "Start one supervised worker actor with a typed assignment." $ \request -> do
            _ <- startActor workerDefinition request.assignment
            pure (WorkerStarted True)
      }
