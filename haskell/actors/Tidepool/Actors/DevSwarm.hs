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
  , rootPolicy
  , WorkerSpec (..)
  , WorkerHandle (..)
  , AcceptedWorker (..)
  , WorkerStartResult (..)
  , WorkerPhase (..)
  , WorkerSummary (..)
  , WorkerWake (..)
  , SessionContext (..)
  , WorkerCollection (..)
  , AcknowledgementDisposition (..)
  , WorkerAcknowledgementRequest (..)
  , WorkerCustody (..)
  , WorkerAcknowledgement (..)
  , WorkerReport (..)
  , WorkerOutcome (..)
  , CandidateReceipt (..)
  , worker
  , startWorker
  , startWorkers
  , currentSessionContext
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
import Tidepool.Actor.Internal (ActorRef (..))
import Tidepool.Agent.Session (agentSession)
import Tidepool.Aeson (FromJSON, Result (..), ToJSON, Value, fromJSON, toJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import qualified Tidepool.Data.Text as T
import Tidepool.Effects.Core (Actor, AgentSession, WorkerKernel (..), Worktree)
import Tidepool.Worktree

type RootEffects = '[AgentSession, Actor, Worktree, WorkerKernel]

-- | Canonical workbench alias for the root's actor-local compilation view.
type ActorEffects = RootEffects

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
  | WorkerTombstoned { accepted :: AcceptedWorker }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerPhase
  = WorkerProvisioning
  | WorkerRunning
  | WorkerTerminal
  | WorkerCollectedPhase
  | WorkerAcknowledgedPhase
  | WorkerStartFailedPhase
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerSummary = WorkerSummary
  { accepted :: AcceptedWorker
  , phase :: WorkerPhase
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerWake = WorkerWake
  { wakeEvent :: Int
  , wakeHandle :: WorkerHandle
  }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

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
      { worker :: WorkerHandle
      , outcome :: WorkerOutcome
      }
  | WorkerNotFound { worker :: WorkerHandle }
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

data WorkerCustody = WorktreeRetained
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerAcknowledgement
  = WorkerAcknowledged
      { worker :: WorkerHandle
      , disposition :: AcknowledgementDisposition
      , custody :: WorkerCustody
      }
  | WorkerAlreadyAcknowledged
      { worker :: WorkerHandle
      , disposition :: AcknowledgementDisposition
      , custody :: WorkerCustody
      }
  | WorkerNotCollected { worker :: WorkerHandle }
  | WorkerAcknowledgementUnknown { worker :: WorkerHandle }
  deriving (Generic, FromJSON, JsonSchema, ToJSON)

data WorkerProtocol result

rootPolicy :: Eff RootEffects a
rootPolicy = loop
  where
    loop = do
      _ <- (agentSession Nothing () :: Eff RootEffects ())
      loop

workerDefinition
  :: WorkerHandle
  -> WorktreeHandle
  -> Text
  -> ActorDefinition Text WorkerProtocol CandidateReceipt
workerDefinition workerHandle tree key =
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
          repository <- case observed of
            Left failure -> error (T.unpack (renderWorktreeError failure))
            Right value -> pure value
          let receipt = CandidateReceipt report repository
          submitWorker workerHandle receipt
          pure receipt
      , visibleToChild = []
      , onShutdown = const (pure ())
      }

startWorker
  :: (Member Actor effs, Member Worktree effs, Member WorkerKernel effs)
  => Text
  -> Text
  -> Eff effs WorkerStartResult
startWorker key assignment = do
  results <- startWorkers [worker key assignment]
  case results of
    [result] -> pure result
    _ -> error "worker kernel violated singleton start cardinality"

startWorkers
  :: (Member Actor effs, Member Worktree effs, Member WorkerKernel effs)
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
            (workerDefinition acceptedWorker.handle tree acceptedWorker.key)
            (assignmentFor acceptedWorker.key specs)
          let ActorRef actorId incarnation _ = ref
          kernel
            ( WorkerAttachWith
                acceptedWorker.handle.workerId
                (actorId, incarnation)
            )
    provision result = pure result

assignmentFor :: Text -> [WorkerSpec] -> Text
assignmentFor wanted specs = case [spec.assignment | spec <- specs, spec.key == wanted] of
  assignment : _ -> assignment
  [] -> error "worker kernel accepted a key absent from its request batch"

currentSessionContext :: Member WorkerKernel effs => Eff effs SessionContext
currentSessionContext = kernel WorkerSessionContextWith

listWorkers :: Member WorkerKernel effs => Eff effs [WorkerSummary]
listWorkers = kernel WorkerListWith

collectWorkers
  :: Member WorkerKernel effs
  => [WorkerHandle]
  -> Eff effs [WorkerCollection]
collectWorkers handles = kernel (WorkerCollectWith (toJSON handles))

collectWorkerWakes
  :: Member WorkerKernel effs
  => [WorkerWake]
  -> Eff effs [WorkerCollection]
collectWorkerWakes = collectWorkers . map (\wake -> wake.wakeHandle)

acknowledgeWorkers
  :: Member WorkerKernel effs
  => [WorkerAcknowledgementRequest]
  -> Eff effs [WorkerAcknowledgement]
acknowledgeWorkers requests = kernel (WorkerAcknowledgeWith (toJSON requests))

submitWorker
  :: Member WorkerKernel effs
  => WorkerHandle
  -> CandidateReceipt
  -> Eff effs ()
submitWorker handle receipt =
  send (WorkerSubmitWith handle.workerId (toJSON receipt))

kernel :: (Member WorkerKernel effs, FromJSON a) => WorkerKernel Value -> Eff effs a
kernel request = do
  encoded <- send request
  case fromJSON encoded of
    Success value -> pure value
    Error detail -> error ("worker kernel returned an invalid typed value: " <> detail)
