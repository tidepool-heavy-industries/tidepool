{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | The first self-hosting actor policy.
--
-- Rust owns panes, processes, scheduling, worktree truth, and delivery. This
-- module owns worker intent, idempotency, typed candidate composition, and
-- result custody. Collection observes an exact exit and never consumes it;
-- only explicit acknowledgement releases the root's reference.
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
import qualified Tidepool.Data.Text as T
import Tidepool.Effects.Core (Actor, ActorMcp, Worktree)
import Tidepool.Worktree

type RootEffects = '[ActorMcp, Actor, Worktree]

data StatusInput = StatusInput
  deriving (Generic, FromJSON, JsonSchema)

data ActorStatus = ActorStatus
  { role :: Text
  , detail :: Text
  }
  deriving (Generic, JsonSchema, ToJSON)

data SpawnWorker = SpawnWorker
  { workKey :: Text
  , assignment :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

-- | Stable model-visible correlation for one worker in this root
-- incarnation. The constructor carries no authority; the root retains the
-- exact 'ActorRef' privately.
newtype WorkerHandle = WorkerHandle { workerId :: Text }
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data WorkerStart
  = WorkerAccepted
      { workKey :: Text
      , worker :: WorkerHandle
      }
  | WorkerKeyConflict { workKey :: Text }
  | WorkerAlreadyAcknowledged
      { workKey :: Text
      , worker :: WorkerHandle
      }
  | WorkerStartFailed
      { workKey :: Text
      , summary :: Text
      }
  deriving (Generic, JsonSchema, ToJSON)

data ListWorkers = ListWorkers
  deriving (Generic, FromJSON, JsonSchema)

data WorkerPhase
  = WorkerPendingPhase
  | WorkerCollectedPhase
  | WorkerAcknowledgedPhase
  deriving (Generic, JsonSchema, ToJSON)

data WorkerSummary = WorkerSummary
  { workKey :: Text
  , worker :: WorkerHandle
  , phase :: WorkerPhase
  }
  deriving (Generic, JsonSchema, ToJSON)

newtype Workers = Workers { workers :: [WorkerSummary] }
  deriving (Generic, JsonSchema, ToJSON)

newtype CollectWorker = CollectWorker { worker :: WorkerHandle }
  deriving (Generic, FromJSON, JsonSchema)

data WorkerReport = WorkerReport
  { summary :: Text
  , evidence :: [Text]
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data CandidateReceipt = CandidateReceipt
  { authoredReport :: WorkerReport
  , repository :: SubmissionObservation
  }
  deriving (Generic, JsonSchema, ToJSON)

data WorkerOutcome
  = WorkCompleted { receipt :: CandidateReceipt }
  | WorkFailed { summary :: Text }
  | WorkCancelled { summary :: Text }
  deriving (Generic, JsonSchema, ToJSON)

data WorkerCollection
  = WorkerPending { worker :: WorkerHandle }
  | WorkerCollected
      { worker :: WorkerHandle
      , outcome :: WorkerOutcome
      }
  | WorkerCollectionAcknowledged { worker :: WorkerHandle }
  | WorkerNotFound { worker :: WorkerHandle }
  deriving (Generic, JsonSchema, ToJSON)

newtype AcknowledgeWorker = AcknowledgeWorker { worker :: WorkerHandle }
  deriving (Generic, FromJSON, JsonSchema)

data WorkerAcknowledgement
  = WorkerAcknowledged { worker :: WorkerHandle }
  | WorkerNotCollected { worker :: WorkerHandle }
  | WorkerAcknowledgementUnknown { worker :: WorkerHandle }
  deriving (Generic, JsonSchema, ToJSON)

data AssignmentInput = AssignmentInput
  deriving (Generic, FromJSON, JsonSchema)

newtype WorkerAssignment = WorkerAssignment { assignment :: Text }
  deriving (Generic, JsonSchema, ToJSON)

newtype FinishAccepted = FinishAccepted { accepted :: Bool }
  deriving (Generic, JsonSchema, ToJSON)

data WorkerProtocol result

data RootTools mode = RootTools
  { actorStatus :: mode :- Call StatusInput ActorStatus
  , spawnWorker :: mode :- Update SpawnWorker WorkerStart
  , listWorkers :: mode :- Call ListWorkers Workers
  , collectWorker :: mode :- Update CollectWorker WorkerCollection
  , ackWorker :: mode :- Update AcknowledgeWorker WorkerAcknowledgement
  }
  deriving (Generic)

data WorkerTools mode = WorkerTools
  { actorStatus :: mode :- Call StatusInput ActorStatus
  , currentAssignment :: mode :- Call AssignmentInput WorkerAssignment
  , finishWork :: mode :- Finish WorkerReport FinishAccepted
  }
  deriving (Generic)

workerDefinition
  :: WorktreeHandle
  -> Text
  -> ActorDefinition Text WorkerProtocol CandidateReceipt
workerDefinition tree key =
  withWorktree tree
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
                  finishTool "Submit the authored report. Tidepool observes repository facts itself and exits with a trusted candidate receipt." $ \report -> do
                    observed <- observeSubmission (worktreeId tree)
                    case observed of
                      Left failure -> error (T.unpack (renderWorktreeError failure))
                      Right repository ->
                        pure
                          ( FinishAccepted True
                          , CandidateReceipt report repository
                          )
              }
      , visibleToChild = []
      , onShutdown = const (pure ())
      }

rootPolicy :: Eff RootEffects a
rootPolicy = serveToolsWith [] tools
  where
    tools records =
      RootTools
        { actorStatus =
            tool "Describe the root actor and its current role." $ \_ ->
              pure (ActorStatus "root" "ready to unfold and fold typed workers")
        , spawnWorker =
            updateTool "Idempotently accept a supervised worker intent. Acceptance means the resident actor exists and external deployment is underway; it does not claim Codex is online." $ \request ->
              case findByKey request.workKey records of
                Just record
                  | recordAssignment record == request.assignment ->
                      pure (existingStart record, records)
                  | otherwise ->
                      pure (WorkerKeyConflict request.workKey, records)
                Nothing -> do
                  created <- createWorktree (fromCurrentRepository ("shoal/" <> request.workKey))
                  case created of
                    Left failure ->
                      pure
                        ( WorkerStartFailed request.workKey (renderWorktreeError failure)
                        , records
                        )
                    Right tree -> do
                      let handle = WorkerHandle (renderWorktreeId (worktreeId tree))
                      ref <- startActor
                        (workerDefinition tree request.workKey)
                        request.assignment
                      pure
                        ( WorkerAccepted request.workKey handle
                        , RunningWorker request.workKey request.assignment handle ref : records
                        )
        , listWorkers =
            tool "List every worker key reserved in this root incarnation and its collection phase." $ \_ ->
              pure (Workers (map summarize records))
        , collectWorker =
            updateTool "Nonblocking, non-consuming collection. A terminal candidate or failure is replayed identically until explicit acknowledgement." $ \request ->
              case findByHandle request.worker records of
                Nothing -> pure (WorkerNotFound request.worker, records)
                Just record -> collectRecord record records
        , ackWorker =
            updateTool "Acknowledge an already-collected worker and release its exact actor reference. The key remains reserved for this root incarnation." $ \request ->
              case acknowledge request.worker records of
                AckUnknown ->
                  pure (WorkerAcknowledgementUnknown request.worker, records)
                AckPending ->
                  pure (WorkerNotCollected request.worker, records)
                AckDone updated ->
                  pure (WorkerAcknowledged request.worker, updated)
        }

data WorkerRecord
  = RunningWorker Text Text WorkerHandle (ActorRef WorkerProtocol CandidateReceipt)
  | CollectedWorker Text Text WorkerHandle WorkerOutcome
  | AcknowledgedWorker Text Text WorkerHandle

recordKey :: WorkerRecord -> Text
recordKey record = case record of
  RunningWorker key _ _ _ -> key
  CollectedWorker key _ _ _ -> key
  AcknowledgedWorker key _ _ -> key

recordAssignment :: WorkerRecord -> Text
recordAssignment record = case record of
  RunningWorker _ assignment _ _ -> assignment
  CollectedWorker _ assignment _ _ -> assignment
  AcknowledgedWorker _ assignment _ -> assignment

recordHandle :: WorkerRecord -> WorkerHandle
recordHandle record = case record of
  RunningWorker _ _ handle _ -> handle
  CollectedWorker _ _ handle _ -> handle
  AcknowledgedWorker _ _ handle -> handle

findByKey :: Text -> [WorkerRecord] -> Maybe WorkerRecord
findByKey key = findRecord ((== key) . recordKey)

findByHandle :: WorkerHandle -> [WorkerRecord] -> Maybe WorkerRecord
findByHandle handle = findRecord ((== handle) . recordHandle)

findRecord :: (WorkerRecord -> Bool) -> [WorkerRecord] -> Maybe WorkerRecord
findRecord _ [] = Nothing
findRecord predicate (record : rest)
  | predicate record = Just record
  | otherwise = findRecord predicate rest

existingStart :: WorkerRecord -> WorkerStart
existingStart record = case record of
  AcknowledgedWorker key _ handle -> WorkerAlreadyAcknowledged key handle
  _ -> WorkerAccepted (recordKey record) (recordHandle record)

summarize :: WorkerRecord -> WorkerSummary
summarize record = WorkerSummary
  { workKey = recordKey record
  , worker = recordHandle record
  , phase = case record of
      RunningWorker {} -> WorkerPendingPhase
      CollectedWorker {} -> WorkerCollectedPhase
      AcknowledgedWorker {} -> WorkerAcknowledgedPhase
  }

collectRecord
  :: WorkerRecord
  -> [WorkerRecord]
  -> Eff RootEffects (WorkerCollection, [WorkerRecord])
collectRecord record records = case record of
  RunningWorker key assignment handle ref -> do
    terminal <- pollExit ref
    case terminal of
      Nothing -> pure (WorkerPending handle, records)
      Just exit ->
        let outcome = workerOutcome exit
            updated = replaceRecord handle (CollectedWorker key assignment handle outcome) records
         in pure (WorkerCollected handle outcome, updated)
  CollectedWorker _ _ handle outcome ->
    pure (WorkerCollected handle outcome, records)
  AcknowledgedWorker _ _ handle ->
    pure (WorkerCollectionAcknowledged handle, records)

data AckResult
  = AckUnknown
  | AckPending
  | AckDone [WorkerRecord]

acknowledge :: WorkerHandle -> [WorkerRecord] -> AckResult
acknowledge handle records = case findByHandle handle records of
  Nothing -> AckUnknown
  Just (RunningWorker {}) -> AckPending
  Just (CollectedWorker key assignment _ _) ->
    AckDone (replaceRecord handle (AcknowledgedWorker key assignment handle) records)
  Just (AcknowledgedWorker {}) -> AckDone records

replaceRecord :: WorkerHandle -> WorkerRecord -> [WorkerRecord] -> [WorkerRecord]
replaceRecord _ _ [] = []
replaceRecord handle replacement (record : rest)
  | recordHandle record == handle = replacement : rest
  | otherwise = record : replaceRecord handle replacement rest

workerOutcome :: ActorExit CandidateReceipt -> WorkerOutcome
workerOutcome terminal = case terminal of
  Completed receipt -> WorkCompleted receipt
  Failed (ActorFailure summary) -> WorkFailed summary
  Cancelled (CancelReason summary) -> WorkCancelled summary
