{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Focused contracts for the production candidate workflow compositions.
module Project.WorkflowExamplesChecks (workflowContracts) where

import Control.Monad (unless)
import Control.Monad.Freer (Eff, Member, interpret, run)
import qualified Control.Monad.Freer.State as State
import Data.Text (Text)
import qualified Data.Text as Text
import Exomonad.Contrib.Types (Candidate (..), Outcome (..), Task (..))
import Project.WorkflowExamples
import Tidepool.Agent.Ref.Internal (internalAgentRef, agentIdentity)
import Tidepool.Agent.Reply.Internal
  ( Replies (..), RequestId (..), requestIdNumber, responseRequestId
  , newRequestHandles, fillResponse, Progress (..)
  , ResponseResult (..), ExecutionReceipt (..), WorktreeEvidence (..)
  , RequestError (..), ReplyError (..), ResponseFailure (..)
  )
import Tidepool.Agent.Watch.Internal
  ( AwaitError (..), AwaitPlan (..), AwaitNode (..), AwaitDependency (..), AwaitDecision (..)
  , RawWatchObservation (..), Watches (..), ForgetWatchOutcome (..)
  )
import Tidepool.Effects.Core
  ( AgentLaunch (..), CheckpointRefusal (..)
  , SpawnErrorWire, WorktreeHandle, GitOid (..)
  , SpawnContextWire (..), SpawnWorkspaceWire (..)
  )
import Tidepool.Agent.Launch (Workspace (..))
import qualified Tidepool.Effects.Core as Core
import Tidepool.Actors.Spawn (SpawnError (SpawnRefused))

data Event
  = Checkpoint Text
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

type HarnessEffects = '[AgentLaunch, Replies, Watches, State.State Script]

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
        (interpret launchHandler action))))
  where
    record :: Member (State.State Script) effects => Event -> Eff effects ()
    record event = State.modify (\current -> current { events = events current <> [event] })

    launchHandler :: AgentLaunch value -> Eff '[Replies, Watches, State.State Script] value
    launchHandler request = case request of
      AgentLaunchCheckpointWith name -> do
        record (Checkpoint name)
        checkpointResult <$> State.get
      AgentLaunchSpawnWith context _ workspace _ _ _ _ _ _ _ -> do
        record (Spawned context workspace)
        current <- State.get
        case spawnResults current of
          [] -> error "workflow fixture: unscripted spawn"
          answer : rest -> do
            State.put current { spawnResults = rest }
            pure answer
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
      SubmitRequestWith identity _ _ _ -> do
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
        (admitCandidates [(SameDir, first)])
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
      (admitted, admissionTrace) = runHarness admissionsScript (admitCandidates work)
      expectedAdmissionTrace =
        [ Checkpoint "candidate coordination"
        , Spawned (CapturedSpawn "checkpoint-1") SameDirectory
        , Spawned (CapturedSpawn "checkpoint-1") SameDirectory
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
      producedTask = taskFixture "produced"
      blockedTask = taskFixture "blocked"
      (failedRequest, _) = newRequestHandles () (RequestId 101) actor
      (producedRequest, _) = newRequestHandles () (RequestId 102) actor
      (blockedRequest, _) = newRequestHandles () (RequestId 103) actor
      producedCandidate = Candidate
        { candidateCommit = GitOid "1123456789abcdef0123456789abcdef01234567"
        , reportedChecks = ["compiled"]
        , remainingGates = ["review"]
        }
      blockedReason = "required check missing"
      blockedEvidence = ["cargo test was not run"]
      producedReceipt = ResponseResult (Produced producedCandidate)
        (ExecutionReceipt (RequestId 102) 41 3) NoBoundWorktree
      blockedReceipt = ResponseResult (Blocked blockedReason blockedEvidence)
        (ExecutionReceipt (RequestId 103) 41 3) NoBoundWorktree
      -- Filling is intentionally sequenced before running the lazy projection.
      readyRequests = fillResponse producedRequest producedReceipt `seq`
        fillResponse blockedRequest blockedReceipt `seq` ()
      mixedAdmissions =
        [ CandidateSpawnRefused first (SpawnRefused "fixture spawn refusal")
        , CandidateRequestRefused second actor requestRefusal
        , CandidateRequested failedTask actor failedRequest (Progress (RequestId 101))
        , CandidateRequested producedTask actor producedRequest (Progress (RequestId 102))
        , CandidateRequested blockedTask actor blockedRequest (Progress (RequestId 103))
        ]
      observationScript = initialScript
        { awaitRegistration = Right 9
        , awaitFailures = [(RequestId 101, ResponseTargetFailed "terminal failure")]
        }
      (mixedObservation, observationTrace) = readyRequests `seq`
        runHarness observationScript (awaitCandidates mixedAdmissions)
  assert "settlement observation keeps mixed refusals and response outcomes in input order"
    (case mixedObservation of
      CandidateObservationReady
        [ CandidateSpawnNotAdmitted spawnTask (SpawnRefused "fixture spawn refusal")
        , CandidateRequestNotAdmitted requestTask retainedActor (RequestReservationRejected ReplyUnauthorized)
        , CandidateResponseUnavailable failedIdentity failedActor (ResponseTargetFailed "terminal failure")
        , CandidateResponseReceived producedIdentity producedActor actualProduced
        , CandidateResponseReceived blockedIdentity blockedActor actualBlocked
        ] -> spawnTask == first && requestTask == second
          && failedIdentity == failedTask && producedIdentity == producedTask && blockedIdentity == blockedTask
          && agentIdentity retainedActor == (41, 3)
          && agentIdentity failedActor == (41, 3)
          && agentIdentity producedActor == (41, 3)
          && agentIdentity blockedActor == (41, 3)
          && responseValue actualProduced == Produced producedCandidate
          && responseExecution actualProduced == ExecutionReceipt (RequestId 102) 41 3
          && responseWorktree actualProduced == NoBoundWorktree
          && responseValue actualBlocked == Blocked blockedReason blockedEvidence
          && responseExecution actualBlocked == ExecutionReceipt (RequestId 103) 41 3
          && responseWorktree actualBlocked == NoBoundWorktree
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
        outcome <- admitCandidates [(SameDir, rejectedTask)]
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
      , Spawned (CapturedSpawn "checkpoint-1") SameDirectory
      , RequestReserved Nothing (61, 5)
      , RequestSubmitted 91
      , CheckpointReleased "checkpoint-1"
      , AwaitRegistered
      ])
