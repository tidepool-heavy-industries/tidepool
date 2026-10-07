{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Remixable compositions of the existing owners. Readiness is project code:
-- it can verify the source and assets and return the check appropriate to them.
module Project.WorkflowExamples
  ( PreparedCheckIssue (..), prepareFocused, awaitFocused
  , CandidateAdmission (..), CandidateAdmissions (..), CandidateSettlement (..)
  , CandidateObservation (..), CandidateRunReport (..)
  , admitCandidates, awaitCandidates, nextCandidateEvent, scopedCandidates
  ) where

import Control.Monad.Freer (Eff, Member)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)
import Tidepool.Actors.Exomonad
import Exomonad.Contrib.Types
import Project.Work (workspaceAgentSpec, taskContext)
import Exomonad.Contrib.PrepareContinue
import Project.TestEvidence

data PreparedCheckIssue issue
  = PrerequisiteIssue (PreparationFailure issue)
  | CheckStartIssue FocusedSetupIssue
  deriving (Show)

-- | Invoke in the preparation job's completion handler. The readiness callback
-- runs with that handler's authority; a path alone grants no checkout access.
-- Starting the check returns its original handle for CheckResults.watchChecks.
prepareFocused
  :: Member Commands effects
  => Cmd.Memory
  -> Cmd.Job
  -> Cmd.CommandResult
  -> (Cmd.RunResult -> Eff effects (Either issue FocusedSpec))
  -> Eff effects (Either (PreparedCheckIssue issue) FocusedRun)
prepareFocused memory preparation receipt readiness = do
  ready <- verifyPrepared preparation receipt readiness
  case ready of
    Left issue -> pure (Left (PrerequisiteIssue issue))
    Right spec -> fmap (either (Left . CheckStartIssue) Right) (startFocused memory spec)

-- | Compose a pending prerequisite and its focused check in one continuation.
-- The runtime keeps the original preparation alive until its terminal receipt.
awaitFocused
  :: Member Commands effects
  => Cmd.Memory
  -> Cmd.Job
  -> (Cmd.RunResult -> Eff effects (Either issue FocusedSpec))
  -> Eff effects (Either (PreparedCheckIssue issue) FocusedRun)
awaitFocused memory preparation readiness = do
  ready <- awaitPrepared preparation readiness
  case ready of
    Left issue -> pure (Left (PrerequisiteIssue issue))
    Right spec -> fmap (either (Left . CheckStartIssue) Right) (startFocused memory spec)

-- Preserve each admission against its typed Task. A request refusal retains
-- the already-ready idle actor; the owner can retry or retire that actor.
data CandidateAdmission
  = CandidateSpawnRefused Task SpawnError
  | CandidateRequestRefused Task AgentRef RequestError
  | CandidateRequested Task AgentRef (Request (Outcome Candidate)) (Progress WorkProgress)
  deriving (Show)

-- The original admissions and the release of their shared captured context are
-- independent evidence. A release refusal does not discard issued handles.
data CandidateAdmissions = CandidateAdmissions
  { candidateAdmissions :: [CandidateAdmission]
  , candidateCheckpointRelease :: Either CheckpointRefusal ()
  } deriving (Show)

-- Capture once, prepare every idle actor, then admit independent typed inputs.
-- Workspace choices can share SameDir or choose exact ForkWorktree sources.
-- These actor-owned handles remain available in subsequent notebook cells.
admitCandidates
  :: (Member AgentLaunch effects, Member Replies effects)
  => [(Workspace, Task)]
  -> Eff effects (Either CheckpointRefusal CandidateAdmissions)
admitCandidates = admitCandidatesWithLifetime ActorOwned

admitCandidatesWithLifetime
  :: (Member AgentLaunch effects, Member Replies effects)
  => WorkerLifetime -> [(Workspace, Task)]
  -> Eff effects (Either CheckpointRefusal CandidateAdmissions)
admitCandidatesWithLifetime lifetime work = do
  captured <- checkpoint "candidate coordination"
  case captured of
    Left issue -> pure (Left issue)
    Right context -> do
      idle <- mapM (spawn context) work
      admitted <- mapM activate idle
      released <- releaseCheckpoint context
      pure (Right (CandidateAdmissions admitted released))
  where
    spawn context (workspace, task) = do
      ready <- spawnSubagent (ForkCtx context) workspace
        ((defaultSpawnOptions workspaceAgentSpec)
          { spawnModel = Just "luna", spawnEffort = Just Medium
          , spawnInstructions = Just (taskContext task), spawnLabel = Just (taskName task)
          , spawnLifetime = lifetime })
      pure (task, ready)
    activate (task, Left issue) = pure (CandidateSpawnRefused task issue)
    activate (task, Right actor) = do
      admitted <- requestWithProgress @WorkProgress @(Outcome Candidate) actor task
        (defaultRequestOptions { requestLifetime = lifetime })
      pure $ case admitted of
        Left issue -> CandidateRequestRefused task actor issue
        Right (reply, updates) -> CandidateRequested task actor reply updates

-- Admission, terminal failure, and the authored Outcome remain distinct.
-- Successful replies retain their original execution and worktree evidence.
data CandidateSettlement
  = CandidateSpawnNotAdmitted Task SpawnError
  | CandidateRequestNotAdmitted Task AgentRef RequestError
  | CandidateResponseUnavailable Task AgentRef ResponseFailure
  | CandidateResponseReceived Task AgentRef (ResponseResult (Outcome Candidate))
  deriving (Show)

data CandidateObservation
  = CandidateObservationFailed AwaitError
  | CandidateObservationReady [CandidateSettlement]
  deriving (Show)

data CandidateRunReport
  = CandidateCheckpointUnavailable CheckpointRefusal
  | CandidateRunReported
      { runAdmissions :: CandidateAdmissions
      , runObservation :: CandidateObservation
      }
  deriving (Show)

-- Traversal retains every assignment, including a refusal between successes.
-- A rejected observation is an AwaitError; it does not erase the input handles.
awaitCandidates
  :: Member Watches effects
  => [CandidateAdmission]
  -> Eff effects CandidateObservation
awaitCandidates admissions =
  either CandidateObservationFailed CandidateObservationReady <$> await (traverse observe admissions)
  where
    observe (CandidateSpawnRefused task issue) = pure (CandidateSpawnNotAdmitted task issue)
    observe (CandidateRequestRefused task actor issue) = pure (CandidateRequestNotAdmitted task actor issue)
    observe (CandidateRequested task actor reply _) =
      either (CandidateResponseUnavailable task actor) (CandidateResponseReceived task actor)
        <$> settledResponse reply

-- This optional composition opts only its actors and requests into the scope.
-- The original admissions survive an AwaitError so cleanup can be assessed
-- against the issued handles. scopeBody and scopeCleanup remain independent.
scopedCandidates
  :: ( Member ResourceScopes effects, Member AgentLaunch effects
     , Member Replies effects, Member Watches effects )
  => [(Workspace, Task)]
  -> Eff effects (ScopeOutcome CandidateRunReport)
scopedCandidates work = withScope $ \scope -> do
  admitted <- admitCandidatesWithLifetime (InScope scope) work
  case admitted of
    Left refusal -> pure (CandidateCheckpointUnavailable refusal)
    Right admissions -> do
      collected <- awaitCandidates (candidateAdmissions admissions)
      pure (CandidateRunReported admissions collected)

-- The first terminal response or next progress observation resumes the owner.
-- A result failure remains an AwaitError; authored Blocked remains a value.
nextCandidateEvent
  :: Member Watches effects
  => Request (Outcome Candidate) -> Progress WorkProgress -> ProgressCursor
  -> Eff effects (Either AwaitError (Either (Outcome Candidate) (ProgressState WorkProgress)))
nextCandidateEvent reply updates cursor = await (eitherOf (result reply) (after updates cursor))
