{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- Authored project requests and exact-source review. Context, workspace and
-- installed tools remain explicit choices at each spawn.
module Project.Work
  ( task, projectPrompt, taskContext, reviewContext, decisionContext
  , withDecision, updateDecision, designQuestion, raiseQuestion, resolveQuestion
  , WorkspaceEffects, workspaceAgentSpec, WorkAdmissionError (..)
  , implement, reviewCandidate, reviewCommit, requestReview, repair
  , candidateAtSubmission, reviewCandidateAtSubmission, admitReviewedCheckpoint
  , requestIncorporation, consultDesign, unownedPaths
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core
  ( Actor, ActorContext, AgentLaunch, AgentInspection, AgentControl, Commands
  , BoundWorktree, WorktreeRegistry, WorktreeAllocation, WorktreeIntegration
  , Notifications, Jev, ModelCall, Console, Reflect, Lookup, Source, Journal, RepoEvent, GitRef (..))
import Tidepool.Agent.Contract (AgentSpec)
import qualified AgentSpec as Installed
import qualified Project.Tools as Tools
import Tidepool.Worktree (renderGitOid)
import qualified Tidepool.Command as Cmd
import Exomonad.Contrib.Types
import Project.Evidence (numstatFiles)
import Exomonad.Workspace (workspacePrompt)

-- This workspace chooses its installed API directly. The runtime does not
-- infer tools or authority from the worker's task, prompt, model or label.
type WorkspaceEffects =
  '[Replies, Watches, ActorContext, AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation, WorktreeIntegration
   , Notifications, Jev, ModelCall, Commands, Console, Actor, Reflect, Lookup
   , Source, Journal, RepoEvent]

workspaceAgentSpec :: AgentSpec (Tools.WorkspaceTools WorkspaceEffects) WorkspaceEffects
workspaceAgentSpec = Installed.agentSpec

-- Admission failure retains a successfully spawned idle actor when only its
-- request was refused. Execution and authored Outcome errors remain separate.
data WorkAdmissionError
  = WorkSpawnRefused SpawnError
  | WorkRequestRefused AgentRef RequestError
  deriving (Show)

task :: Text -> Text -> [Text] -> Text -> GitOid -> Task
task name objective owned accept source = Task
  { taskName = name
  , planPath = ".exomonad/WORKBENCH.md"
  , taskSource = source
  , obligation = objective
  , rationale = ""
  , ownedPaths = owned
  , acceptance = accept
  , acceptedDecisions = []
  }

-- Keys are the workspace's authored resource names; selecting prose grants no
-- permission and does not create a runtime role.
projectPrompt :: Text -> Text
projectPrompt name = case workspacePrompt name of
  Just body -> body
  Nothing -> error ("Missing configured project prompt: " <> Text.unpack name)

taskContext :: Task -> Text
taskContext task = Text.unlines $
  [ "Plan: " <> planPath task
  , "Source: " <> renderGitOid (taskSource task)
  , "Obligation: " <> obligation task
  , "Why: " <> rationale task
  , "Owned source: " <> Text.intercalate ", " (ownedPaths task)
  , "Acceptance: " <> acceptance task
  , "Use this task's source, ownership and acceptance. Relevant operations live in Project.Work; use their supplied examples and the lookup tool (names, modules, or a Hoogle-like type) when you need to check one."
  ] ++ map decisionContext (acceptedDecisions task)

-- Only call after the owning decision and source incorporation have been checked.
-- Replace this question's old decision; keep unrelated accepted choices intact.
withDecision :: AcceptedDecision -> Task -> Task
withDecision decision task = task
  { taskSource = decisionSource decision
  , acceptedDecisions = filter (not . sameQuestion (decisionQuestion decision) . decisionQuestion)
      (acceptedDecisions task) ++ [decision]
  }

raiseQuestion :: Question -> Attention -> Attention
raiseQuestion question current = filter (not . sameQuestion question) current ++ [question]

-- An answer for an older revision of a question cannot clear its newer finding.
resolveQuestion :: AcceptedDecision -> Attention -> Attention
resolveQuestion decision = filter (/= decisionQuestion decision)

-- Render only the actionable answer and its exact correlation. Detailed question
-- evidence remains recoverable from the retained question and named source.
decisionContext :: AcceptedDecision -> Text
decisionContext decision = Text.unlines
  [ questionKey question <> " @" <> renderGitOid (questionSource details) <> " " <> questionPlan details
  , questionFinding details
  , decisionSummary decision
  , "Decision source: " <> renderGitOid (decisionSource decision)
  , Text.intercalate "; " (decisionEvidence decision)
  ]
  where
    question = decisionQuestion decision
    details = questionDetails question

updateDecision
  :: Member Replies effects
  => Request result -> AcceptedDecision -> Eff effects (Either ReplyError RequestUpdate)
updateDecision response = updateRequest response . decisionContext

implement
  :: (Member AgentLaunch effects, Member Replies effects)
  => Task -> Eff effects (Either WorkAdmissionError (Request (Outcome Candidate), Progress WorkProgress))
implement work = do
  spawned <- spawnSubagent (FreshCtx (taskContext work)) (ForkWorktree currentCheckout)
    ((defaultSpawnOptions workspaceAgentSpec)
      { spawnModel = Just "luna", spawnEffort = Just Medium
      , spawnInstructions = Just (projectPrompt "task"), spawnLabel = Just (taskName work) })
  case spawned of
    Left issue -> pure (Left (WorkSpawnRefused issue))
    Right worker -> do
      admitted <- requestWithProgress @WorkProgress @(Outcome Candidate) worker work defaultRequestOptions
      pure (either (Left . WorkRequestRefused worker) Right admitted)

-- Exact-scope reviews return findings to their requester because there is no
-- owning Task with which to address a retained implementer.
repairOwnerContext :: RepairOwner -> Text
repairOwnerContext owner = case owner of
  OwnerRepairs -> "Repair owner: your requester. Return Repair findings; it will repair and commission the exact revised source. Do not queue work behind its pending delivery."
  RetainedImplementer actor -> "Repair owner: retained implementer " <> Text.pack (show actor)
    <> ". Use repair for direct follow-up; keep your review pending while its separate request runs."

reviewContext :: ReviewRequest -> Text
reviewContext request = Text.unlines
  [ basisContext
  , "Candidate: " <> renderGitOid (candidateCommit (reviewInput request))
  , "Claimed checks: " <> Text.intercalate "; " (reportedChecks (reviewInput request))
  , "Remaining product gates: " <> Text.intercalate "; " (remainingGates (reviewInput request))
  , ownerContext
  ]
  where
    (basisContext, ownerContext) = case reviewBasis request of
      AssignedTask task -> (taskContext task, repairOwnerContext (repairOwner request))
      ExactScope base owned accept ->
        (Text.unlines
          [ "Base: " <> renderGitOid base
          , "Owned source: " <> Text.intercalate ", " owned
          , "Acceptance: " <> accept
          ], repairOwnerContext OwnerRepairs)

reviewCandidate
  :: (Member AgentLaunch effects, Member Replies effects)
  => Task -> RepairOwner -> Candidate
  -> Eff effects (Either WorkAdmissionError (Request (Outcome ReviewDecision), Progress WorkProgress))
reviewCandidate work owner candidate =
  requestReview (taskName work <> " review") (ReviewRequest (AssignedTask work) candidate owner)

reviewCommit
  :: (Member AgentLaunch effects, Member Replies effects)
  => Text -> GitOid -> GitOid -> Text -> [Text]
  -> Eff effects (Either WorkAdmissionError (Request (Outcome ReviewDecision), Progress WorkProgress))
reviewCommit name base commit accept owned = requestReview name $
  ReviewRequest (ExactScope base owned accept) (Candidate commit [] []) OwnerRepairs

-- After the original review response settles, a producer can call
-- 'admitReviewedCheckpoint' with its ReviewRequest and Request handle, then
-- publish the returned checkpoint through 'withReviewedCheckpoint'. The router
-- retains it with ordinary progress and decides whether to notify its owner.

-- Ownership gate: which paths a candidate range actually touched outside its
-- declared ownership. A command failure is unavailable evidence, never an
-- empty passing diff.
unownedPaths
  :: Member Commands effects
  => GitOid -> GitOid -> [Text] -> Eff effects [Text]
unownedPaths base candidate owned = do
  let range = renderGitOid base <> ".." <> renderGitOid candidate
  result <- Cmd.run (Cmd.argv ["git", "diff", "--numstat", range])
  case (Cmd.failure result, Cmd.stdout result, Cmd.commandCleanup (Cmd.commandResult result)) of
    (Nothing, Right stat, Cmd.CommandClean) -> pure [path | (_, _, path) <- numstatFiles stat, path `notElem` owned]
    unavailable -> error ("unownedPaths: git diff --numstat " <> Text.unpack range <> " failed: " <> show unavailable)

-- Each exact-source review chooses its own workspace and installed API.
requestReview
  :: (Member AgentLaunch effects, Member Replies effects)
  => Text -> ReviewRequest
  -> Eff effects (Either WorkAdmissionError (Request (Outcome ReviewDecision), Progress WorkProgress))
requestReview name input = do
  spawned <- spawnSubagent (FreshCtx (reviewContext input))
    (ForkWorktree (atRef (GitRef (renderGitOid (candidateCommit (reviewInput input))))))
    ((defaultSpawnOptions workspaceAgentSpec)
      { spawnModel = Just "luna", spawnEffort = Just Medium
      , spawnInstructions = Just (projectPrompt "review"), spawnLabel = Just name })
  case spawned of
    Left issue -> pure (Left (WorkSpawnRefused issue))
    Right reviewer -> do
      admitted <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) reviewer input defaultRequestOptions
      pure (either (Left . WorkRequestRefused reviewer) Right admitted)

-- Exact-scope findings return to the owner. A retained implementer receives an
-- independent caller-owned request; no implicit lifetime transfer is needed.
repair
  :: Member Replies effects
  => Text -> ReviewRequest -> Candidate -> [Text]
  -> Eff effects (Either ReviewDecision (Either RequestError (Request (Outcome Candidate))))
repair name reviewRequest candidate findings = case reviewBasis reviewRequest of
  ExactScope _ _ _ -> pure (Left (Repair candidate findings))
  AssignedTask work -> case repairOwner reviewRequest of
    OwnerRepairs -> pure (Left (Repair candidate findings))
    RetainedImplementer worker -> Right <$> request @(Outcome Candidate) worker
      (RepairTask work candidate findings)
      (defaultRequestOptions { requestLabel = Just name, requestGuidance = Just (projectPrompt "repair") })

requestIncorporation
  :: Member Replies effects
  => AgentRef -> Text -> Task -> PlanAmendment
  -> Eff effects (Either RequestError (Request Incorporation))
requestIncorporation recipient name work amendment = request @Incorporation recipient
  (IncorporationTask work amendment)
  (defaultRequestOptions { requestLabel = Just name, requestGuidance = Just (projectPrompt "incorporate") })

-- Build a complete packet from evidence already bound in the workbench. Record
-- updates add alternatives or narrow the unblocked obligation when needed.
designQuestion :: Task -> Candidate -> Text -> DesignQuestion
designQuestion task candidate finding = DesignQuestion
  { questionPlan = planPath task
  , questionSource = candidateCommit candidate
  , questionFinding = finding
  , questionEvidence = reportedChecks candidate
  , questionAlternatives = []
  , questionUnblocks = [obligation task]
  }

consultDesign
  :: (Member AgentLaunch effects, Member Replies effects, Member Watches effects)
  => DesignSlot -> DesignQuestion
  -> Eff effects (Either WorkAdmissionError (Request DesignAnswer, Watch (Either ResponseFailure DesignAnswer)))
consultDesign slot question = do
  spawned <- spawnSubagent (FreshCtx (designContext slot question))
    (ForkWorktree (atRef (GitRef (renderGitOid (questionSource question)))))
    ((defaultSpawnOptions workspaceAgentSpec)
      { spawnModel = Just (specialistModel slot), spawnEffort = Just (specialistEffort slot)
      , spawnInstructions = Just (projectPrompt "specialist"), spawnLabel = Just (specialistLabel slot) })
  case spawned of
    Left issue -> pure (Left (WorkSpawnRefused issue))
    Right expert -> do
      admitted <- request @DesignAnswer expert question defaultRequestOptions
      case admitted of
        Left issue -> pure (Left (WorkRequestRefused expert issue))
        Right reply -> do
          ready <- watch (Just (specialistWatch slot)) (settlement reply)
          pure (Right (reply, ready))

designContext :: DesignSlot -> DesignQuestion -> Text
designContext slot question = Text.unlines
  [ "Declared specialist plan: " <> specialistPlan slot
  , "Waiting component: " <> questionPlan question
  , "Source: " <> renderGitOid (questionSource question)
  , "Finding: " <> questionFinding question
  , "Evidence: " <> Text.intercalate "; " (questionEvidence question)
  , "Alternatives: " <> Text.intercalate "; " (questionAlternatives question)
  , "Unblocks: " <> Text.intercalate "; " (questionUnblocks question)
  ]
