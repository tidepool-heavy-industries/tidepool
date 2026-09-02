{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DerivingStrategies #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | The first self-hosting actor policy.
--
-- Rust owns panes, processes, scheduling, worktree truth, and delivery. This
-- module owns worker intent, idempotency, typed candidate composition, and
-- result custody. Collection observes an exact exit and never consumes it;
-- only explicit acknowledgement releases the root's reference.
module Tidepool.Actors.DevSwarm
  ( RootEffects
  , ActorEffects
  , rootPolicy
  , WorkerRecord
  , WorkerHandle (..)
  , WorkerStart (..)
  , WorkerPhase (..)
  , WorkerSummary (..)
  , WorkerCollection (..)
  , WorkerAcknowledgement (..)
  , WorkerReport (..)
  , WorkerOutcome (..)
  , CandidateReceipt (..)
  , startWorker
  , listWorkerState
  , collectWorkerResult
  , acknowledgeWorker
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Prelude

import Tidepool.Actor
import Tidepool.Agent.Session (agentSession)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import qualified Tidepool.Data.Text as T
import Tidepool.Effects.Core (Actor, AgentSession, Worktree)
import Tidepool.Worktree

type RootEffects = '[AgentSession, Actor, Worktree]

-- | Canonical workbench alias for the root's actor-local compilation view.
-- Child facades provide the same name for their attenuated row.
type ActorEffects = RootEffects

-- | Stable model-visible correlation for one worker in this root
-- incarnation. The constructor carries no authority; the root retains the
-- exact 'ActorRef' privately.
newtype WorkerHandle = WorkerHandle { workerId :: Text }
  deriving stock (Eq, Generic)
  deriving anyclass (FromJSON, JsonSchema, ToJSON)

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

data WorkerAcknowledgement
  = WorkerAcknowledged { worker :: WorkerHandle }
  | WorkerNotCollected { worker :: WorkerHandle }
  | WorkerAcknowledgementUnknown { worker :: WorkerHandle }
  deriving (Generic, JsonSchema, ToJSON)

data WorkerProtocol result

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
      , behavior = \_ assignmentText -> do
          report <-
            ( agentSession (Just assignmentText) assignmentText
                :: Eff (ReadOnlyEffects WorkerProtocol) WorkerReport
            )
          observed <- observeSubmission (worktreeId tree)
          case observed of
            Left failure -> error (T.unpack (renderWorktreeError failure))
            Right repository -> pure (CandidateReceipt report repository)
      , visibleToChild = []
      , onShutdown = const (pure ())
      }

-- | The fixed root policy owns only the durable-in-incarnation state loop.
-- The attached agent evolves orchestration policy inside each typed session
-- and returns the next exact state with 'complete'.
rootPolicy :: Eff RootEffects a
rootPolicy = loop []
  where
    loop :: [WorkerRecord] -> Eff RootEffects a
    loop records = do
      next <-
        ( agentSession Nothing records
            :: Eff RootEffects [WorkerRecord]
        )
      loop next

startWorker
  :: (Member Actor effs, Member Worktree effs)
  => Text
  -> Text
  -> [WorkerRecord]
  -> Eff effs (WorkerStart, [WorkerRecord])
startWorker key assignmentText records =
  case findByKey key records of
    Just record
      | recordAssignment record == assignmentText ->
          pure (existingStart record, records)
      | otherwise -> pure (WorkerKeyConflict key, records)
    Nothing -> do
      created <- createWorktree (fromCurrentRepository ("shoal/" <> key))
      case created of
        Left failure ->
          pure (WorkerStartFailed key (renderWorktreeError failure), records)
        Right tree -> do
          let handle = WorkerHandle (renderWorktreeId (worktreeId tree))
          ref <- startActor (workerDefinition tree key) assignmentText
          pure
            ( WorkerAccepted key handle
            , RunningWorker key assignmentText handle ref : records
            )

listWorkerState :: [WorkerRecord] -> [WorkerSummary]
listWorkerState = map summarize

collectWorkerResult
  :: Member Actor effs
  => WorkerHandle
  -> [WorkerRecord]
  -> Eff effs (WorkerCollection, [WorkerRecord])
collectWorkerResult handle records =
  case findByHandle handle records of
    Nothing -> pure (WorkerNotFound handle, records)
    Just record -> collectRecord record records

acknowledgeWorker
  :: WorkerHandle
  -> [WorkerRecord]
  -> (WorkerAcknowledgement, [WorkerRecord])
acknowledgeWorker handle records = case acknowledge handle records of
  AckUnknown -> (WorkerAcknowledgementUnknown handle, records)
  AckPending -> (WorkerNotCollected handle, records)
  AckDone updated -> (WorkerAcknowledged handle, updated)

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
  :: Member Actor effs
  => WorkerRecord
  -> [WorkerRecord]
  -> Eff effs (WorkerCollection, [WorkerRecord])
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
