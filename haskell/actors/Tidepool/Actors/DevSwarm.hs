{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DerivingStrategies #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE NoFieldSelectors #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | Typed orchestration policy for Shoal.
--
-- Rust owns the mutable worker ledger, exact actor correlation, lifecycle
-- history, and receipt custody. Haskell describes worker programs and invokes
-- typed operations; no copied registry value is threaded through model turns.
module Tidepool.Actors.DevSwarm
  ( RootEffects
  , ActorEffects
  , WorkerEffects
  , rootPolicy
  , RootActivation (..)
  , WorkerActivation (..)
  , AgentAction
  , ActionFailure (..)
  , waitOn
  , continueWith
  , nextTurn
  , WorkerSpec (..)
  , WorkerHandle (..)
  , AcceptedWorker (..)
  , WorkerStartResult (..)
  , WorkerPhase (..)
  , WorkerSummary (..)
  , WorkerWake (..)
  , LifecycleEventId
  , SessionContext (..)
  , WorkerCollection (..)
  , AcknowledgementDisposition (..)
  , WorkerAcknowledgementRequest (..)
  , WorkerAcknowledgement (..)
  , WorkerReport (..)
  , WorkerOutcome (..)
  , CandidateReceipt (..)
  , worker
  , workerHandleOf
  , collectionHandle
  , actionableWorkers
  , acknowledgedWorkers
  , startWorker
  , startWorkers
  , listWorkers
  , collectWorkers
  , collectWorkerWakes
  , acknowledgeWorkers
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import GHC.Generics (Generic)
import Prelude

import Tidepool.Actor
import Tidepool.Agent.Action
import Tidepool.Agent.Session (agentSession)
import Tidepool.Aeson (FromJSON, Result (..), ToJSON, Value, fromJSON, toJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import qualified Tidepool.Data.Text as T
import Tidepool.Effects.Core (Actor, AgentSession, WorkerKernel (..), Worktree)
import Tidepool.Internal.ActorRef (ExitRef (..), actorAddress)
import Tidepool.Worktree

type RootEffects = '[AgentSession, Actor, Worktree, WorkerKernel CandidateReceipt]

-- | Canonical workbench alias for the root's actor-local compilation view.
type ActorEffects = RootEffects

type WorkerEffects = ReadOnlyEffects WorkerProtocol

-- | Stable facts mounted for one root activation. Runtime-owned worker wakes
-- are a snapshot for this activation; an interrupted action is the typed
-- reason a previously returned Haskell continuation could not finish.
data RootActivation = RootActivation
  { sessionContext :: SessionContext
  , rootInterruption :: Maybe ActionFailure
  }

-- | Stable facts mounted for one worker activation. The assignment remains
-- available after an action failure without being copied into a Developer
-- message or serialized through the tool transport.
data WorkerActivation = WorkerActivation
  { workerAssignment :: Text
  , workerInterruption :: Maybe ActionFailure
  }

data WorkerSpec = WorkerSpec
  { key :: Text
  , assignment :: Text
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

worker :: Text -> Text -> WorkerSpec
worker = WorkerSpec

newtype WorkerHandle = WorkerHandle { workerId :: Text }
  deriving stock (Eq, Generic)
  deriving anyclass (FromJSON, JsonSchema, ToJSON)

data AcceptedWorker = AcceptedWorker
  { key :: Text
  , handle :: WorkerHandle
  , fingerprint :: Text
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerStartResult
  = WorkerAccepted { accepted :: AcceptedWorker }
  | WorkerAlreadyRunning { accepted :: AcceptedWorker }
  | WorkerStartConflict
      { key :: Text
      , existingFingerprint :: Text
      , requestedFingerprint :: Text
      }
  | WorkerStartFailed
      { accepted :: AcceptedWorker
      , detail :: Text
      }
  | WorkerStartAcknowledged { accepted :: AcceptedWorker }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerPhase
  = WorkerProvisioning
  | WorkerRunning
  | WorkerTerminal
  | WorkerCollectedPhase
  | WorkerAcknowledgedPhase
  | WorkerStartFailedPhase
  deriving stock (Eq, Generic)
  deriving anyclass (FromJSON, JsonSchema, ToJSON)

data WorkerSummary = WorkerSummary
  { accepted :: AcceptedWorker
  , phase :: WorkerPhase
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerWake = WorkerWake
  { wakeEvent :: LifecycleEventId
  , wakeHandle :: WorkerHandle
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

newtype LifecycleEventId = LifecycleEventId { lifecycleEventId :: Int }
  deriving stock (Eq, Generic)
  deriving anyclass (FromJSON, JsonSchema, ToJSON)

newtype SessionContext = SessionContext
  { workerWakes :: [WorkerWake]
  }
  deriving stock (Generic)
  deriving anyclass (FromJSON, JsonSchema, ToJSON)

data WorkerReport = WorkerReport
  { summary :: Text
  , evidence :: [Text]
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data CandidateReceipt = CandidateReceipt
  { authoredReport :: WorkerReport
  , repository :: SubmissionObservation
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerOutcome
  = WorkCompleted { receipt :: CandidateReceipt }
  | WorkFailed { detail :: Text }
  | WorkCancelled { detail :: Text }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerCollection
  = WorkerPending { worker :: WorkerHandle }
  | WorkerCollected
      { worker :: WorkerHandle
      , outcome :: WorkerOutcome
      }
  | WorkerCollectionAcknowledged
      { worker :: WorkerHandle }
  | WorkerCollectionUnknown { worker :: WorkerHandle }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data AcknowledgementDisposition
  = IntegratedAs { oid :: Text }
  | Reviewed
  | Rejected { reason :: Text }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerAcknowledgementRequest = WorkerAcknowledgementRequest
  { worker :: WorkerHandle
  , disposition :: AcknowledgementDisposition
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerAcknowledgement
  = WorkerAcknowledged
      { worker :: WorkerHandle
      , disposition :: AcknowledgementDisposition
      }
  | WorkerAlreadyAcknowledged
      { worker :: WorkerHandle
      , disposition :: AcknowledgementDisposition
      }
  | WorkerNotCollected { worker :: WorkerHandle }
  | WorkerAcknowledgementUnknown { worker :: WorkerHandle }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerProtocol result

rootPolicy :: Eff RootEffects a
rootPolicy = loop Nothing Nothing
  where
    loop initialUser interruption = do
      context <- takeSessionContext
      action <-
        ( agentSession initialUser (RootActivation context interruption)
            :: Eff RootEffects (AgentAction RootEffects ())
        )
      outcome <- runAgentAction action
      case outcome of
        Right () -> loop Nothing Nothing
        Left failure ->
          loop
            (Just "Your returned Haskell action stopped at an actor lifecycle failure. The typed failure is mounted in `sessionInput`; decide the next program explicitly.")
            (Just failure)

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
          report <- workerSession (Just assignmentText) (WorkerActivation assignmentText Nothing)
          observed <- observeSubmission (worktreeId tree)
          repository <- case observed of
            Left failure -> error (T.unpack (renderWorktreeError failure))
            Right value -> pure value
          let receipt = CandidateReceipt report repository
          pure receipt
      , onShutdown = const (pure ())
      }

workerSession :: Maybe Text -> WorkerActivation -> Eff WorkerEffects WorkerReport
workerSession initialUser activation = do
  action <-
    ( agentSession initialUser activation
        :: Eff WorkerEffects (AgentAction WorkerEffects WorkerReport)
    )
  outcome <- runAgentAction action
  case outcome of
    Right report -> pure report
    Left failure ->
      workerSession
        (Just "Your returned Haskell action stopped at an actor lifecycle failure. The typed failure is mounted in `sessionInput`; decide the next program explicitly.")
        activation { workerInterruption = Just failure }

startWorker
  :: (Member Actor effs, Member Worktree effs, Member (WorkerKernel CandidateReceipt) effs)
  => Text
  -> Text
  -> Eff effs WorkerStartResult
startWorker key assignment = do
  results <- startWorkers [worker key assignment]
  case results of
    [result] -> pure result
    _ -> error "worker kernel violated singleton start cardinality"

startWorkers
  :: (Member Actor effs, Member Worktree effs, Member (WorkerKernel CandidateReceipt) effs)
  => [WorkerSpec]
  -> Eff effs [WorkerStartResult]
startWorkers specs = do
  reserved <- kernel (WorkerReserveBatchWith (toJSON specs))
  mapM provision reserved
  where
    provision result@WorkerAccepted { accepted = acceptedWorker } = do
      created <- createWorktree (fromCurrentRepository ("shoal/" <> acceptedWorker.key))
      case created of
        Left failure ->
          kernel
            ( WorkerFailStartWith
                acceptedWorker.handle.workerId
                (renderWorktreeError failure)
            )
        Right tree -> do
          ref <- startActor
            (workerDefinition tree acceptedWorker.key)
            (assignmentFor acceptedWorker.key specs)
          let exitRef = ExitRef ref
          exitRef `seq`
            kernel
              ( WorkerAttachWith
                  acceptedWorker.handle.workerId
                  exitRef
                  (actorAddress ref)
              )
    provision result = pure result

assignmentFor :: Text -> [WorkerSpec] -> Text
assignmentFor wanted specs = case [spec.assignment | spec <- specs, spec.key == wanted] of
  assignment : _ -> assignment
  [] -> error "worker kernel accepted a key absent from its request batch"

-- | Recover the exact worker handle from every successful start result.
-- Conflicts and provisioning failures remain explicit rather than throwing.
workerHandleOf :: WorkerStartResult -> Maybe WorkerHandle
workerHandleOf WorkerAccepted { accepted = worker } = Just worker.handle
workerHandleOf WorkerAlreadyRunning { accepted = worker } = Just worker.handle
workerHandleOf WorkerStartAcknowledged { accepted = worker } = Just worker.handle
workerHandleOf WorkerStartConflict {} = Nothing
workerHandleOf WorkerStartFailed {} = Nothing

-- | Correlation carried by a collection result, independent of its phase.
collectionHandle :: WorkerCollection -> WorkerHandle
collectionHandle WorkerPending { worker = handle } = handle
collectionHandle WorkerCollected { worker = handle } = handle
collectionHandle WorkerCollectionAcknowledged { worker = handle } = handle
collectionHandle WorkerCollectionUnknown { worker = handle } = handle

-- | Workers which may still require orchestration or a custody decision.
actionableWorkers :: [WorkerSummary] -> [WorkerSummary]
actionableWorkers = filter (\summary -> summary.phase /= WorkerAcknowledgedPhase)

-- | Stable audit projection for workers whose custody was acknowledged.
acknowledgedWorkers :: [WorkerSummary] -> [WorkerSummary]
acknowledgedWorkers = filter (\summary -> summary.phase == WorkerAcknowledgedPhase)

takeSessionContext
  :: Member (WorkerKernel CandidateReceipt) effs
  => Eff effs SessionContext
takeSessionContext = kernel WorkerSessionContextWith

listWorkers :: Member (WorkerKernel CandidateReceipt) effs => Eff effs [WorkerSummary]
listWorkers = kernel WorkerListWith

data WorkerInspection
  = WorkerInspectionPending { worker :: WorkerHandle }
  | WorkerInspectionExitReady { worker :: WorkerHandle }
  | WorkerInspectionStartFailed { worker :: WorkerHandle, detail :: Text }
  | WorkerInspectionAcknowledged { worker :: WorkerHandle }
  | WorkerInspectionUnknown { worker :: WorkerHandle }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

collectWorkers
  :: (Member Actor effs, Member (WorkerKernel CandidateReceipt) effs)
  => [WorkerHandle]
  -> Eff effs [WorkerCollection]
collectWorkers handles = do
  inspections <- kernel (WorkerInspectWith (toJSON handles))
  mapM collect inspections
  where
    collect WorkerInspectionPending { worker = handle } = pure (WorkerPending handle)
    collect WorkerInspectionStartFailed { worker = handle, detail = failure } =
      pure (WorkerCollected handle (WorkFailed failure))
    collect WorkerInspectionAcknowledged { worker = handle } =
      pure (WorkerCollectionAcknowledged handle)
    collect WorkerInspectionUnknown { worker = handle } =
      pure (WorkerCollectionUnknown handle)
    collect WorkerInspectionExitReady { worker = handle } = do
      exitRef <- send (WorkerBorrowExitWith handle.workerId)
      outcome <- awaitExitRef exitRef
      pure (WorkerCollected handle (workerOutcome outcome))

    awaitExitRef (ExitRef ref) = awaitExit ref

    workerOutcome (Completed receipt) = WorkCompleted receipt
    workerOutcome (Failed failure) = WorkFailed failure.actorFailureSummary
    workerOutcome (Cancelled reason) = WorkCancelled reason.cancelReasonSummary

collectWorkerWakes
  :: (Member Actor effs, Member (WorkerKernel CandidateReceipt) effs)
  => [WorkerWake]
  -> Eff effs [WorkerCollection]
collectWorkerWakes = collectWorkers . map (\wake -> wake.wakeHandle)

acknowledgeWorkers
  :: Member (WorkerKernel CandidateReceipt) effs
  => [WorkerAcknowledgementRequest]
  -> Eff effs [WorkerAcknowledgement]
acknowledgeWorkers requests = kernel (WorkerAcknowledgeWith (toJSON requests))

kernel
  :: (Member (WorkerKernel CandidateReceipt) effs, FromJSON a)
  => WorkerKernel CandidateReceipt Value
  -> Eff effs a
kernel request = do
  encoded <- send request
  case fromJSON encoded of
    Success value -> pure value
    Error detail -> error ("worker kernel returned an invalid typed value: " <> detail)
