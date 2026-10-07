{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Remixable compositions of the existing owners. Readiness is project code:
-- it can verify the source and assets and return the check appropriate to them.
module Project.WorkflowExamples
  ( PreparedCheckIssue (..), prepareFocused, awaitFocused
  , CandidateAdmission (..), CandidateSettlement (..)
  , admitCandidates, awaitCandidates, nextCandidateEvent, scopedCandidates
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
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

-- Capture once, prepare every idle actor, then admit independent typed inputs.
-- Workspace choices can share SameDir or choose exact ForkWorktree sources.
-- These actor-owned handles remain available in subsequent notebook cells.
admitCandidates
  :: (Member AgentLaunch effects, Member Replies effects)
  => [(Workspace, Task)]
  -> Eff effects (Either CheckpointRefusal ([CandidateAdmission], Either CheckpointRefusal ()))
admitCandidates = admitCandidatesWithLifetime ActorOwned

admitCandidatesWithLifetime
  :: (Member AgentLaunch effects, Member Replies effects)
  => WorkerLifetime -> [(Workspace, Task)]
  -> Eff effects (Either CheckpointRefusal ([CandidateAdmission], Either CheckpointRefusal ()))
admitCandidatesWithLifetime lifetime work = do
  captured <- checkpoint "candidate coordination"
  case captured of
    Left issue -> pure (Left issue)
    Right context -> do
      idle <- mapM (spawn context) work
      admitted <- mapM activate idle
      released <- releaseCheckpoint context
      pure (Right (admitted, released))
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

-- Admission refusals retain the original SpawnError or idle AgentRef. Terminal
-- request failures and authored Blocked values occupy different Either layers.
data CandidateSettlement
  = CandidateNotRequested CandidateAdmission
  | CandidateSettled Task AgentRef (Either ResponseFailure (Either (Text, [Text]) Candidate))
  deriving (Show)

-- Traversal retains every assignment, including a refusal between successes.
-- A rejected observation is an AwaitError; it does not erase the input handles.
awaitCandidates
  :: Member Watches effects
  => [CandidateAdmission]
  -> Eff effects (Either AwaitError [CandidateSettlement])
awaitCandidates admissions = await (traverse observe admissions)
  where
    observe admission@(CandidateSpawnRefused _ _) = pure (CandidateNotRequested admission)
    observe admission@(CandidateRequestRefused _ _ _) = pure (CandidateNotRequested admission)
    observe (CandidateRequested task actor reply _) =
      CandidateSettled task actor . fmap authoredResult <$> settlement reply
    authoredResult (Produced candidate) = Right candidate
    authoredResult (Blocked reason evidence) = Left (reason, evidence)

-- This optional composition opts only its actors and requests into the scope.
-- The original admissions survive an AwaitError so cleanup can be assessed
-- against the issued handles. scopeBody and scopeCleanup remain independent.
scopedCandidates
  :: ( Member ResourceScopes effects, Member AgentLaunch effects
     , Member Replies effects, Member Watches effects )
  => [(Workspace, Task)]
  -> Eff effects (ScopeOutcome
       (Either CheckpointRefusal
         ([CandidateAdmission], Either CheckpointRefusal (), Either AwaitError [CandidateSettlement])))
scopedCandidates work = withScope $ \scope -> do
  admitted <- admitCandidatesWithLifetime (InScope scope) work
  case admitted of
    Left refusal -> pure (Left refusal)
    Right (admissions, released) -> do
      collected <- awaitCandidates admissions
      pure (Right (admissions, released, collected))

-- The first terminal response or next progress observation resumes the owner.
-- A result failure remains an AwaitError; authored Blocked remains a value.
nextCandidateEvent
  :: Member Watches effects
  => Request (Outcome Candidate) -> Progress WorkProgress -> ProgressCursor
  -> Eff effects (Either AwaitError (Either (Outcome Candidate) (ProgressState WorkProgress)))
nextCandidateEvent reply updates cursor = await (eitherOf (result reply) (after updates cursor))
