{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE TypeApplications #-}

-- | Focused contracts for the production candidate workflow compositions.
module Fixture.WorkflowExamplesChecks
  ( workflowContracts, successfulSettlements, startReceiptWorker, requestBlocked
  , producedOutcome, blockedOutcome ) where

import Control.Monad (unless)
import Control.Monad.Freer (Eff, Member, interpret, run)
import qualified Control.Monad.Freer.State as State
import Data.Text (Text)
import qualified Data.Text as Text
import Exomonad.Contrib.Types (Candidate (..), Outcome (..), Task (..), WorkProgress)
import Project.WorkflowExamples
import Project.Work (workspaceAgentSpec)
import Tidepool.Actors.Exomonad
  ( spawnSubagent, SpawnContext (FreshCtx), defaultSpawnOptions
  , requestWithProgress, defaultRequestOptions )
import Tidepool.Agent.Ref.Internal (AgentRef, internalAgentRef, agentIdentity)
import Tidepool.Agent.Reply.Internal
  ( Replies (..), RequestId (..), requestIdNumber, responseRequestId
  , Request, newRequestHandles, Progress (..)
  , ResponseResult (..), ExecutionReceipt (..)
  , RequestError (..), ReplyError (..), ResponseFailure (..)
  )
import Tidepool.Agent.Watch.Internal
  ( AwaitError (..), AwaitPlan (..), AwaitNode (..), AwaitDependency (..), AwaitDecision (..)
  , RawWatchObservation (..), Watches (..), ForgetWatchOutcome (..), await, response
  )
import Tidepool.Effects.Core
  ( AgentLaunch (..), CheckpointRefusal (..)
  , SpawnErrorWire, WorktreeHandle, GitOid (..)
  , SpawnContextWire (..), SpawnWorkspaceWire (..)
  )
import Tidepool.Agent.Launch (Workspace (..))
import qualified Tidepool.Effects.Core as Core
import Tidepool.Actors.Spawn (SpawnError (SpawnRefused))
import Tidepool.Model (Model, KnownFamily, SpawnAgent, OpenAI, OpenAIOptions (..), OpenAIReasoning (..), openAI, modelSelection, modelPermissionRequests, noModelPermissions)
import Tidepool.Aeson (toJSON)

data Event
  = SpecValidated
  | Checkpoint Text
  | Spawned SpawnContextWire SpawnWorkspaceWire
  | CheckpointReleased Text
  | RequestReserved (Maybe Text) (Int, Int)
  | RequestSubmitted Int
  | AwaitRegistered
  | AwaitObserved Int
  | AwaitForgotten Int
  deriving (Eq, Show)

data Script = Script
  { checkpointResult :: Either CheckpointRefusal Text
  , spawnResults :: [Either SpawnErrorWire (Int, Int, Maybe WorktreeHandle)]
  , requestReservations :: [Either RequestError Int]
  , requestSubmissions :: [Either RequestError ()]
  , releaseResult :: Either CheckpointRefusal ()
  , awaitRegistration :: Either ReplyError Int
  , awaitFailures :: [(RequestId, ResponseFailure)]
  , awaitPlan :: Maybe AwaitPlan
  , events :: [Event]
  }

type HarnessEffects = '[Core.ForkAgent OpenAI, AgentLaunch, Replies, Watches, State.State Script]

fixtureModel :: Model OpenAI
fixtureModel = openAI "workflow-fixture" "primary" (OpenAIOptions OpenAILow)

initialScript :: Script
initialScript = Script
  { checkpointResult = Right "checkpoint-1"
  , spawnResults = []
  , requestReservations = []
  , requestSubmissions = []
  , releaseResult = Right ()
  , awaitRegistration = Right 1
  , awaitFailures = []
  , awaitPlan = Nothing
  , events = []
  }

runHarness :: Script -> Eff HarnessEffects a -> (a, Script)
runHarness script action =
  run (State.runState script
    (interpret watchHandler
      (interpret replyHandler
        (interpret launchHandler (interpret forkHandler action)))))
  where
    record :: Member (State.State Script) effects => Event -> Eff effects ()
    record event = State.modify (\current -> current { events = events current <> [event] })

    forkHandler :: Core.ForkAgent OpenAI value -> Eff '[AgentLaunch, Replies, Watches, State.State Script] value
    forkHandler request = case request of
      Core.ForkAgentCheckpointWith Core.OpenAIWitness name -> do
        record (Checkpoint name)
        checkpointResult <$> State.get
      Core.ForkAgentSpawnWith Core.OpenAIWitness _ context workspace _ grants _ selection _ _ _ _ -> do
        assert "the selected model and empty child permissions cross the family boundary"
          (selection == toJSON (modelSelection fixtureModel)
            && grants == toJSON (modelPermissionRequests noModelPermissions))
        record (Spawned context workspace)
        current <- State.get
        case spawnResults current of
          [] -> error "workflow fixture: unscripted spawn"
          answer : rest -> do
            State.put current { spawnResults = rest }
            pure answer

    launchHandler :: AgentLaunch value -> Eff '[Replies, Watches, State.State Script] value
    launchHandler request = case request of
      AgentLaunchValidateSpecWith _ _ -> do
        record SpecValidated
        pure (Right (Core.SpecValidationReceipt [] []
          (Core.SpecProvenanceUnavailable "workflow fixture") Core.SpecProviderNotChecked))
      AgentLaunchReleaseCheckpointWith token -> do
        record (CheckpointReleased token)
        releaseResult <$> State.get
      _ -> error "workflow fixture: unexpected AgentLaunch effect"

    replyHandler :: Replies value -> Eff '[Watches, State.State Script] value
    replyHandler request = case request of
      ReserveRequestWith label target _ _ -> do
        record (RequestReserved label target)
        current <- State.get
        case requestReservations current of
          [] -> error "workflow fixture: unscripted request reservation"
          answer : rest -> do
            State.put current { requestReservations = rest }
            pure answer
      SubmitRequestWith identity _ _ _ _ -> do
        record (RequestSubmitted identity)
        current <- State.get
        case requestSubmissions current of
          [] -> error "workflow fixture: unscripted request submission"
          answer : rest -> do
            State.put current { requestSubmissions = rest }
            pure answer
      _ -> error "workflow fixture: unexpected Replies effect"

    watchHandler :: Watches value -> Eff '[State.State Script] value
    watchHandler request = case request of
      RegisterAwaitWith plan -> do
        record AwaitRegistered
        State.modify (\current -> current { awaitPlan = Just plan })
        awaitRegistration <$> State.get
      AwaitWatchWith identity -> do
        record (AwaitObserved identity)
        current <- State.get
        case awaitPlan current of
          Nothing -> error "workflow fixture: await observed before registration"
          Just (AwaitPlan nodes _) ->
            let leaves =
                  [ (index, lookup requestId (awaitFailures current))
                  | (index, LeafNode (AwaitDependency requestId True)) <- zip [0..] nodes
                  ]
            in pure (RawWatchReady (AwaitDecision leaves []))
      ForgetWatchWith identity -> do
        record (AwaitForgotten identity)
        pure WatchForgotten
      _ -> error "workflow fixture: unexpected Watches effect"

assert :: Text -> Bool -> Eff effects ()
assert name passed = unless passed (error (Text.unpack ("workflow contract failed: " <> name)))

taskFixture :: Text -> Task
taskFixture name = Task
  { taskName = name
  , planPath = ".exomonad/WORKBENCH.md"
  , taskSource = GitOid "0123456789abcdef0123456789abcdef01234567"
  , obligation = "retain " <> name
  , rationale = "workflow contract fixture"
  , ownedPaths = ["src/" <> name]
  , acceptance = "inspect " <> name
  , acceptedDecisions = []
  }

-- The same interpreter drives the production admission/observation functions;
-- it only scripts the generated effect boundary.
workflowContracts :: Eff effects ()
workflowContracts = do
  let first = taskFixture "spawn-refused"
      second = taskFixture "request-refused"
      third = taskFixture "admitted"
      captureRefusal = initialScript { checkpointResult = Left CaptureFailed }
      (refused, refusedScript) = runHarness captureRefusal
        (admitCandidates fixtureModel [(SameDir, first)])
  assert "checkpoint refusal precedes every spawn, request and release"
    (case refused of
      Left CaptureFailed -> events refusedScript == [Checkpoint "candidate coordination"]
      _ -> False)

  let actorIdentityPair = (31, 2)
      admittedActor = Right (31, 2, Nothing)
      requestRefusal = RequestReservationRejected ReplyUnauthorized
      admissionsScript = initialScript
        { spawnResults =
            [ Left (Core.SpawnRefused "fixture spawn refusal")
            , admittedActor
            , Right (32, 1, Nothing)
            ]
        , requestReservations = [Left requestRefusal, Right 77]
        , requestSubmissions = [Right ()]
        , releaseResult = Left ReleasedCheckpoint
        }
      work = [(SameDir, first), (SameDir, second), (SameDir, third)]
      (admitted, admissionTrace) = runHarness admissionsScript (admitCandidates fixtureModel work)
      expectedAdmissionTrace =
        [ Checkpoint "candidate coordination"
        , SpecValidated
        , Spawned (CapturedSpawn "checkpoint-1") SameDirectory
        , SpecValidated
        , Spawned (CapturedSpawn "checkpoint-1") SameDirectory
        , SpecValidated
        , Spawned (CapturedSpawn "checkpoint-1") SameDirectory
        , RequestReserved Nothing actorIdentityPair
        , RequestReserved Nothing (32, 1)
        , RequestSubmitted 77
        , CheckpointReleased "checkpoint-1"
        ]
  assert "spawn, request and checkpoint-release refusals retain assignment order"
    (case admitted of
      Right report ->
        candidateCheckpointRelease report == Left ReleasedCheckpoint
          && case candidateAdmissions report of
            [ CandidateSpawnRefused spawnTask (SpawnRefused "fixture spawn refusal")
              , CandidateRequestRefused requestTask actor (RequestReservationRejected ReplyUnauthorized)
              , CandidateRequested requestTask' actor' request _
              ] -> spawnTask == first && requestTask == second && requestTask' == third
                && agentIdentity actor == actorIdentityPair && agentIdentity actor' == (32, 1)
                && requestIdNumber (responseRequestId request) == 77
            _ -> False
      _ -> False)
  assert "admission effects preserve the production sequencing"
    (events admissionTrace == expectedAdmissionTrace)

  let actor = internalAgentRef 41 3
      failedTask = taskFixture "terminal-failure"
      (failedRequest, _) = newRequestHandles (RequestId 101) actor
      mixedAdmissions =
        [ CandidateSpawnRefused first (SpawnRefused "fixture spawn refusal")
        , CandidateRequestRefused second actor requestRefusal
        , CandidateRequested failedTask actor failedRequest (Progress (RequestId 101))
        ]
      observationScript = initialScript
        { awaitRegistration = Right 9
        , awaitFailures = [(RequestId 101, ResponseTargetFailed "terminal failure")]
        }
      (mixedObservation, observationTrace) =
        runHarness observationScript (awaitCandidates mixedAdmissions)
  assert "settlement observation keeps mixed refusals and response outcomes in input order"
    (case mixedObservation of
      CandidateObservationReady
        [ CandidateSpawnNotAdmitted spawnTask (SpawnRefused "fixture spawn refusal")
        , CandidateRequestNotAdmitted requestTask retainedActor (RequestReservationRejected ReplyUnauthorized)
        , CandidateResponseUnavailable failedIdentity failedActor (ResponseTargetFailed "terminal failure")
        ] -> spawnTask == first && requestTask == second && failedIdentity == failedTask
          && agentIdentity retainedActor == (41, 3)
          && agentIdentity failedActor == (41, 3)
      _ -> False)
  assert "settlement await uses the generated watch admission and exact returned watch"
    (events observationTrace == [AwaitRegistered, AwaitObserved 9, AwaitForgotten 9])

  let rejectedTask = taskFixture "observation-rejected"
      rejectionScript = initialScript
        { spawnResults = [Right (61, 5, Nothing)]
        , requestReservations = [Right 91]
        , requestSubmissions = [Right ()]
        , awaitRegistration = Left ReplyInvalidReadiness
        }
      (rejectedReport, rejectedTrace) = runHarness rejectionScript $ do
        outcome <- admitCandidates fixtureModel [(SameDir, rejectedTask)]
        case outcome of
          Left refusal -> pure (CandidateCheckpointUnavailable refusal)
          Right report -> do
            observation <- awaitCandidates (candidateAdmissions report)
            pure (CandidateRunReported report observation)
  assert "run report keeps the original admissions and checkpoint release beside observation rejection"
    (case rejectedReport of
      CandidateRunReported report (CandidateObservationFailed (AwaitRejected ReplyInvalidReadiness)) ->
        candidateCheckpointRelease report == Right ()
          && case candidateAdmissions report of
            [CandidateRequested task actor request (Progress (RequestId progressId))] -> task == rejectedTask
              && agentIdentity actor == (61, 5)
              && requestIdNumber (responseRequestId request) == 91
              && progressId == 91
            _ -> False
      _ -> False)
  assert "rejected report follows admission, request and watch registration in order"
    (events rejectedTrace ==
      [ Checkpoint "candidate coordination"
      , SpecValidated
      , Spawned (CapturedSpawn "checkpoint-1") SameDirectory
      , RequestReserved Nothing (61, 5)
      , RequestSubmitted 91
      , CheckpointReleased "checkpoint-1"
      , AwaitRegistered
      ])

-- Successful replies are materialized by the hosted request owner. Compare its
-- original receipts with the production candidate wrapper's repeated projection.
successfulSettlements
  :: Member Watches effects
  => AgentRef -> Request (Outcome Candidate) -> Request (Outcome Candidate)
  -> Eff effects ()
successfulSettlements actor producedRequest blockedRequest = do
  produced <- await (response producedRequest)
  blocked <- await (response blockedRequest)
  let producedTask = taskFixture "produced"
      blockedTask = taskFixture "blocked"
      requestRefusal = RequestReservationRejected ReplyUnauthorized
      admissions =
        [ CandidateSpawnRefused (taskFixture "spawn-refused") (SpawnRefused "fixture spawn refusal")
        , CandidateRequestRefused (taskFixture "request-refused") actor requestRefusal
        , CandidateRequested producedTask actor producedRequest (Progress (responseRequestId producedRequest))
        , CandidateRequested blockedTask actor blockedRequest (Progress (responseRequestId blockedRequest))
        ]
  observed <- awaitCandidates admissions
  assert "candidate settlement preserves successful values and original runtime receipts in mixed input order"
    (case (produced, blocked, observed) of
      (Right originalProduced, Right originalBlocked, CandidateObservationReady
        [ CandidateSpawnNotAdmitted spawnTask (SpawnRefused "fixture spawn refusal")
        , CandidateRequestNotAdmitted requestTask retainedActor (RequestReservationRejected ReplyUnauthorized)
        , CandidateResponseReceived producedIdentity producedActor actualProduced
        , CandidateResponseReceived blockedIdentity blockedActor actualBlocked
        ]) -> spawnTask == taskFixture "spawn-refused" && requestTask == taskFixture "request-refused"
          && producedIdentity == producedTask && blockedIdentity == blockedTask
          && agentIdentity retainedActor == agentIdentity actor
          && agentIdentity producedActor == agentIdentity actor
          && agentIdentity blockedActor == agentIdentity actor
          && responseValue actualProduced == producedOutcome
          && responseValue actualBlocked == blockedOutcome
          && actualProduced == originalProduced && actualBlocked == originalBlocked
          && responseExecution actualProduced == receiptFor producedRequest
          && responseExecution actualBlocked == receiptFor blockedRequest
      _ -> False)
  where
    receiptFor request = let (identity, incarnation) = agentIdentity actor
      in ExecutionReceipt (responseRequestId request) identity incarnation

producedOutcome :: Outcome Candidate
producedOutcome = Produced
  (Candidate (GitOid "1123456789abcdef0123456789abcdef01234567") ["compiled"] ["review"])

blockedOutcome :: Outcome Candidate
blockedOutcome = Blocked "required check missing" ["cargo test was not run"]

startReceiptWorker
  :: (KnownFamily modelFamily, Member (SpawnAgent modelFamily) effects
     , Member AgentLaunch effects, Member Replies effects)
  => Model modelFamily -> Eff effects (AgentRef, Request (Outcome Candidate))
startReceiptWorker model = do
  admitted <- spawnSubagent (FreshCtx "Return the requested candidate outcome.") SameDir
    (defaultSpawnOptions model workspaceAgentSpec)
  let actor = require admitted
  requested <- requestWithProgress @WorkProgress @(Outcome Candidate) actor
    (taskFixture "produced") defaultRequestOptions
  pure (actor, fst (require requested))

requestBlocked :: Member Replies effects => AgentRef -> Eff effects (Request (Outcome Candidate))
requestBlocked actor = do
  requested <- requestWithProgress @WorkProgress @(Outcome Candidate) actor
    (taskFixture "blocked") defaultRequestOptions
  pure (fst (require requested))

require :: Show issue => Either issue value -> value
require = either (error . ("workflow fixture admission: " <>) . show) id
