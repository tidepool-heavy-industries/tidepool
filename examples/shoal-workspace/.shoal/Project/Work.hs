{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- Tools for resident sessions. Bind their handles and compose the next operation
-- when it is useful; importing this module prescribes no worker tree.
module Project.Work
  ( projectPrompt, taskContext, reviewContext, decisionContext
  , withDecision, updateDecision, designQuestion, sameQuestion, raiseQuestion, resolveQuestion
  , solTask, solTaskFrom, implement, reviewCandidate, reviewAgain, repair
  , candidateAtSubmission
  , requestIncorporation, consultDesign
  , settledValue
  ) where

import Control.Monad.Freer (Eff, Member)
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

-- Render only the actionable answer and its exact correlation. Detailed question
-- evidence remains recoverable from the retained question and named source.
decisionContext :: AcceptedDecision -> Text
decisionContext decision = Text.unlines
  [ questionKey question <> " @" <> questionSource details <> " " <> questionPlan details
  , questionFinding details
  , decisionSummary decision
  , "incorporated " <> decisionSource decision
  , Text.intercalate "; " (decisionEvidence decision)
  ]
  where
    question = decisionQuestion decision
    details = questionDetails question

updateDecision
  :: Member Replies effects
  => Response result -> AcceptedDecision -> Eff effects (Either ReplyError RequestUpdate)
updateDecision response = updateRequest response . decisionContext

solTask :: BranchLabel -> Task -> Branch CodingEffects Task result
solTask label = solTaskFrom label boundHead

-- Source and context are independent choices. Roots use projectHead; an exact
-- committed review seed uses atRef. Fresh context is an explicit withContext.
solTaskFrom :: BranchLabel -> WorktreeSeed -> Task -> Branch CodingEffects Task result
solTaskFrom label source task = withInstructions (projectPrompt "task") $
  withContext inherited $ withModel "gpt-5.6-sol" $ withEffort Medium $
  coding label source task

implement
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Task -> Eff effects (Forked (Outcome Candidate), Progress WorkProgress)
implement task = unfold (taskGroup task) $
  childWithProgress @WorkProgress @(Outcome Candidate) (solTask (named "implement") task)

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
  => Task -> RepairOwner -> Candidate -> Eff effects (Forked (Outcome ReviewDecision), Progress WorkProgress)
reviewCandidate task owner candidate = unfold (taskGroup task) $ childWithProgress @WorkProgress @(Outcome ReviewDecision) $
  withInstructions (projectPrompt "review") $ withContext (selected reviewContext) $
  withModel "gpt-5.6-sol" $ withEffort Medium $
  coding (named "review") (atRef (GitRef (candidateCommit candidate))) (ReviewTask task candidate owner)

-- This project's automatic review edge selects the committed submission head.
-- Other authored flows may deliberately select earlier artifacts instead.
candidateAtSubmission :: Candidate -> WorktreeEvidence -> Either Text Candidate
candidateAtSubmission candidate evidence = case evidence of
  WorktreeObserved _ _ observation
    | actual == candidateCommit candidate -> Right candidate
    | otherwise -> Left ("candidate " <> candidateCommit candidate <> "; submitted " <> actual)
    where actual = renderGitOid (headOid (submittedHead observation))
  NoBoundWorktree -> Left "candidate has no bound-source evidence"
  WorktreeObservationFailed failure -> Left (renderWorktreeError failure)

-- A completed review attempt leaves its actor available for the revised candidate.
reviewAgain
  :: Member Replies effects
  => AgentRef -> RequestLabel -> ReviewTask -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
reviewAgain actor label task = requestWithProgress @WorkProgress @(Outcome ReviewDecision) actor $
  withRequestGuidance (projectPrompt "review") $
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
    (withRequestGuidance (projectPrompt "repair") $
      requestOptions label (RepairTask (reviewAssignment task) candidate findings))

requestIncorporation
  :: Member Replies effects
  => AgentRef -> RequestLabel -> Task -> PlanAmendment -> Eff effects (Response Incorporation)
requestIncorporation recipient label assignment amendment = requestWith recipient $
  withRequestGuidance (projectPrompt "incorporate") $
  requestOptions label (IncorporationTask assignment amendment)

-- Build a complete packet from evidence already bound in the workbench. Record
-- updates add alternatives or narrow the unblocked obligation when needed.
designQuestion :: Task -> Candidate -> Text -> DesignQuestion
designQuestion task candidate finding = DesignQuestion
  { questionPlan = planPath task
  , questionSource = candidateCommit candidate
  , questionFinding = finding
  , questionEvidence = checkedCommands candidate
  , questionAlternatives = []
  , questionUnblocks = [obligation task]
  }

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
