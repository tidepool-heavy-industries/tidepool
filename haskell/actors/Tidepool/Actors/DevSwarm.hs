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
  { workKey :: Text
  , assignment :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

data WorkerStart
  = WorkerStarted { workKey :: Text }
  | WorkerKeyInUse { workKey :: Text }
  deriving (Generic, ToJSON)

data ListWorkers = ListWorkers
  deriving (Generic, FromJSON, JsonSchema)

data PendingWorkers = PendingWorkers
  { workKeys :: [Text]
  }
  deriving (Generic, ToJSON)

data CollectWorker = CollectWorker
  { workKey :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

data WorkerResult = WorkerResult
  { summary :: Text
  , evidence :: [Text]
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerOutcome
  = WorkCompleted { result :: WorkerResult }
  | WorkFailed { summary :: Text }
  | WorkCancelled { summary :: Text }
  deriving (Generic, ToJSON)

data WorkerCollection
  = WorkerCollected
      { workKey :: Text
      , outcome :: WorkerOutcome
      }
  | WorkerNotFound { workKey :: Text }
  deriving (Generic, ToJSON)

data AssignmentInput = AssignmentInput
  deriving (Generic, FromJSON, JsonSchema)

data WorkerAssignment = WorkerAssignment
  { assignment :: Text
  }
  deriving (Generic, ToJSON)

data FinishAccepted = FinishAccepted
  { accepted :: Bool
  }
  deriving (Generic, ToJSON)

data RootTools mode = RootTools
  { actorStatus :: mode :- Call StatusInput ActorStatus
  , spawnWorker :: mode :- Update SpawnWorker WorkerStart
  , listWorkers :: mode :- Call ListWorkers PendingWorkers
  , collectWorker :: mode :- Update CollectWorker WorkerCollection
  }
  deriving (Generic)

data WorkerTools mode = WorkerTools
  { actorStatus :: mode :- Call StatusInput ActorStatus
  , currentAssignment :: mode :- Call AssignmentInput WorkerAssignment
  , finishWork :: mode :- Finish WorkerResult FinishAccepted
  }
  deriving (Generic)

workerDefinition :: Text -> ActorDefinition Text Maybe WorkerResult
workerDefinition key =
  ActorDefinition
    { label = "devswarm-worker/" <> key
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
            , finishWork =
                finishTool "Return the completed typed work product and exit this worker." $ \result ->
                  pure (FinishAccepted True, result)
            }
    , visibleToChild = []
    , onShutdown = const (pure ())
    }

rootPolicy :: Eff RootEffects a
rootPolicy = serveToolsWith [] tools
  where
    tools workers =
      RootTools
        { actorStatus =
            tool "Describe the root actor and its current role." $ \_ ->
              pure (ActorStatus "root" "ready to unfold and fold typed workers")
        , spawnWorker =
            updateTool "Start one supervised worker actor under a unique work key." $ \request ->
              if hasWorker request.workKey workers
                then pure (WorkerKeyInUse request.workKey, workers)
                else do
                  ref <- startActor (workerDefinition request.workKey) request.assignment
                  pure
                    ( WorkerStarted request.workKey
                    , RunningWorker request.workKey ref : workers
                    )
        , listWorkers =
            tool "List work keys whose exact actor exits have not been collected." $ \_ ->
              pure (PendingWorkers [key | RunningWorker key _ <- workers])
        , collectWorker =
            updateTool "Collect one worker's retained exact typed exit." $ \request ->
              case takeWorker request.workKey workers of
                Nothing -> pure (WorkerNotFound request.workKey, workers)
                Just (ref, remaining) -> do
                  terminal <- awaitExit ref
                  pure
                    ( WorkerCollected request.workKey (workerOutcome terminal)
                    , remaining
                    )
        }

data RunningWorker = RunningWorker Text (ActorRef Maybe WorkerResult)

hasWorker :: Text -> [RunningWorker] -> Bool
hasWorker key = any (\(RunningWorker candidate _) -> candidate == key)

takeWorker
  :: Text
  -> [RunningWorker]
  -> Maybe (ActorRef Maybe WorkerResult, [RunningWorker])
takeWorker _ [] = Nothing
takeWorker key (worker@(RunningWorker candidate ref) : rest)
  | key == candidate = Just (ref, rest)
  | otherwise = do
      (found, remaining) <- takeWorker key rest
      pure (found, worker : remaining)

workerOutcome :: ActorExit WorkerResult -> WorkerOutcome
workerOutcome terminal = case terminal of
  Completed result -> WorkCompleted result
  Failed (ActorFailure summary) -> WorkFailed summary
  Cancelled (CancelReason summary) -> WorkCancelled summary
