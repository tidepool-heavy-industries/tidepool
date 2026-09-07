{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- Tools for resident sessions. Bind their handles and compose the next operation
-- when it is useful; importing this module prescribes no worker tree.
module Project.Work
  ( projectPrompt, taskContext, reviewContext, decisionContext
  , withDecision, raiseQuestion, resolveQuestion
  , solTask, implement, reviewCandidate, reviewAgain, repair
  , requestIncorporation, consultDesign, followAttention
  , settledValue
  ) where

import Control.Monad.Freer (Eff, Member)
import Control.Monad (void, when)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (AgentInspection, Forks, GitRef (..))
import Project.Types
import Shoal.Workspace (workspacePrompt)

-- Keys are the workspace's authored resource names; selecting prose grants no
-- permission and does not create a runtime role.
projectPrompt :: Text -> Text
projectPrompt name = case workspacePrompt name of
  Just body -> body
  Nothing -> error ("Missing configured project prompt: " <> Text.unpack name)

named :: Text -> BranchLabel
named = either (error . show) id . branchLabel

shown :: Show value => value -> Text
shown = Text.pack . show

taskContext :: Task -> Text
taskContext task = Text.unlines $
  [ "Plan: " <> planPath task
  , "Source: " <> taskSource task
  , "Obligation: " <> obligation task
  , "Why: " <> rationale task
  , "Owned source: " <> Text.intercalate ", " (ownedPaths task)
  , "Acceptance: " <> acceptance task
  , "Read this branch's contract and .shoal/plans/language.md. Relevant operations live in Project.Work; use their supplied examples and focused :type/:info when needed."
  ] ++ map decisionContext (acceptedDecisions task)

-- Only call after the owning decision and source incorporation have been checked.
-- Replace this question's old decision; keep unrelated accepted choices intact.
withDecision :: AcceptedDecision -> Task -> Task
withDecision decision task = task
  { taskSource = decisionSource decision
  , acceptedDecisions = filter (not . sameQuestion (decisionQuestion decision) . decisionQuestion)
      (acceptedDecisions task) ++ [decision]
  }

sameQuestion :: Question -> Question -> Bool
sameQuestion left right = questionKey left == questionKey right
  && questionPlan (questionDetails left) == questionPlan (questionDetails right)

raiseQuestion :: Question -> Attention -> Attention
raiseQuestion question current = filter (not . sameQuestion question) current ++ [question]

-- An answer for an older revision of a question cannot clear its newer finding.
resolveQuestion :: AcceptedDecision -> Attention -> Attention
resolveQuestion decision = filter (/= decisionQuestion decision)

decisionContext :: AcceptedDecision -> Text
decisionContext decision = Text.unlines
  [ "Accepted decision for " <> questionKey (decisionQuestion decision)
      <> " at " <> questionSource (questionDetails (decisionQuestion decision))
  , "Question: " <> shown (decisionQuestion decision)
  , decisionSummary decision
  , "Incorporated source: " <> decisionSource decision
  , "Evidence: " <> Text.intercalate "; " (decisionEvidence decision)
  ]

solTask :: BranchLabel -> Task -> Branch CodingEffects Task result
solTask label task = withInstructions (projectPrompt "task") $
  withContext (selected taskContext) $ withModel "gpt-5.6-sol" $ withEffort Low $
  coding label (atRef (GitRef (taskSource task))) task

implement
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Task -> Eff effects (Forked (Outcome Candidate), Progress Attention)
implement task = unfold (taskGroup task) $
  childWithProgress @Attention @(Outcome Candidate) (solTask (named "implement") task)

reviewContext :: ReviewTask -> Text
reviewContext task = Text.unlines
  [ taskContext (reviewAssignment task)
  , "Candidate: " <> candidateCommit (reviewInput task)
  , "Claimed checks: " <> Text.intercalate "; " (checkedCommands (reviewInput task))
  , "Remaining product gates: " <> Text.intercalate "; " (remainingGates (reviewInput task))
  , case repairOwner task of
      OwnerRepairs -> "Repair owner: your requester. Return Repair findings; it will repair and reuse you. Do not queue work behind its pending delivery."
      RetainedImplementer actor -> "Repair owner: retained implementer " <> shown (agentIdentity actor)
        <> ". Use repair for direct follow-up; keep your review pending while its separate request runs."
  ]

reviewCandidate
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Task -> RepairOwner -> Candidate -> Eff effects (Forked (Outcome ReviewDecision), Progress Attention)
reviewCandidate task owner candidate = unfold (taskGroup task) $ childWithProgress @Attention @(Outcome ReviewDecision) $
  withInstructions (projectPrompt "review") $ withContext (selected reviewContext) $
  withModel "gpt-5.6-sol" $ withEffort Low $
  coding (named "review") (atRef (GitRef (candidateCommit candidate))) (ReviewTask task candidate owner)

-- A completed review attempt leaves its actor available for the revised candidate.
reviewAgain
  :: Member Replies effects
  => AgentRef -> RequestLabel -> ReviewTask -> Eff effects (Response (Outcome ReviewDecision), Progress Attention)
reviewAgain actor label task = requestWithProgress @Attention @(Outcome ReviewDecision) actor $
  withRequestGuidance (projectPrompt "review" <> "\n" <> reviewContext task) $
  requestOptions label task

-- Left is the useful verdict to return to the implementing owner; Right is a
-- separate request to an available implementer. No queue is created for Left.
repair
  :: Member Replies effects
  => RequestLabel -> ReviewTask -> Candidate -> [Text]
  -> Eff effects (Either ReviewDecision (Response (Outcome Candidate)))
repair label task candidate findings = case repairOwner task of
  OwnerRepairs -> pure (Left (Repair candidate findings))
  RetainedImplementer actor -> Right <$> requestWith actor
    (withRequestGuidance (Text.unlines
      [ projectPrompt "repair", taskContext (reviewAssignment task)
      , "Repair candidate: " <> candidateCommit candidate
      , "Findings: " <> Text.intercalate "; " findings
      , "Preserved gates: " <> Text.intercalate "; " (remainingGates candidate)
      ]) $
      requestOptions label (RepairTask (reviewAssignment task) candidate findings))

requestIncorporation
  :: Member Replies effects
  => AgentRef -> RequestLabel -> Task -> PlanAmendment -> Eff effects (Response Incorporation)
requestIncorporation recipient label assignment amendment = requestWith recipient $
  withRequestGuidance (projectPrompt "incorporate" <> "\n" <> taskContext assignment) $
  requestOptions label (IncorporationTask assignment amendment)

-- Observe one cumulative question source, forwarding meaningful changes only.
-- The sink owns its scope: combining several sources needs their cumulative union,
-- not publication of each source as if it were the entire component's attention.
followAttention
  :: Member Watches effects
  => Progress Attention -> ProgressCursor -> (Attention -> Eff effects ()) -> Eff effects Route
followAttention updates cursor sink = follow cursor []
  where
    follow after previous = route (awaitProgressAfter updates after) $ \state -> case state of
      ProgressUpdate next current -> do
        when (current /= previous) (sink current)
        void (follow next current)
      ProgressClosed -> pure ()
      ProgressRejected failure -> error (show failure)
      ProgressPending -> error "attention dependency became ready without an observation"

consultDesign
  :: (Member Forks effects, Member Replies effects, Member Watches effects, Member AgentInspection effects, Subset CodingEffects effects)
  => DesignSlot -> DesignQuestion -> Eff effects (Forked DesignAnswer, Watch (Settlement DesignAnswer))
consultDesign slot question = do
  expert <- unfold (specialistGroup slot) $ child $
    withInstructions (projectPrompt "specialist") $ withContext (selected (designContext slot)) $
    withModel (specialistModel slot) $ withEffort (specialistEffort slot) $
    coding (specialistLabel slot) (atRef (GitRef (questionSource question))) question
  ready <- watch (specialistWatch slot) (awaitSettledFork expert)
  pure (expert, ready)

designContext :: DesignSlot -> DesignQuestion -> Text
designContext slot question = Text.unlines
  [ "Declared specialist plan: " <> specialistPlan slot
  , "Waiting component: " <> questionPlan question
  , "Source: " <> questionSource question
  , "Finding: " <> questionFinding question
  , "Evidence: " <> Text.intercalate "; " (questionEvidence question)
  , "Alternatives: " <> Text.intercalate "; " (questionAlternatives question)
  , "Unblocks: " <> Text.intercalate "; " (questionUnblocks question)
  ]

settledValue :: Settlement result -> Either ResponseFailure result
settledValue (ReplyAvailable answer) = Right (responseValue answer)
settledValue (ReplyUnavailable failure) = Left failure
