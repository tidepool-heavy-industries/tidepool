{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- Tools for resident sessions. Bind their handles and compose the next operation
-- when it is useful; importing this module prescribes no worker tree.
module Project.Work
  ( projectPrompt, taskContext, reviewContext, decisionContext
  , withDecision, updateDecision, designQuestion, sameQuestion, raiseQuestion, resolveQuestion
  , lunaTask, lunaTaskFrom, solTask, solTaskFrom, implement, reviewCandidate, reviewCommit, reviewAgain, repair
  , candidateAtSubmission
  , requestIncorporation, consultDesign
  , settledValue
  , unownedPaths
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (AgentInspection, Commands, Forks, GitRef (..))
import Tidepool.Worktree (renderGitOid, renderWorktreeError)
import qualified Tidepool.Command as Cmd
import Project.Types
import Project.Evidence (numstatFiles)
import Exomonad.Workspace (workspacePrompt)

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
  , "Read this branch's contract. Relevant operations live in Project.Work; use their supplied examples and the lookup tool (names, modules, or a Hoogle-like type) when you need to check one."
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
  [ questionKey question <> " @" <> renderGitOid (questionSource details) <> " " <> questionPlan details
  , questionFinding details
  , decisionSummary decision
  , "incorporated " <> renderGitOid (decisionSource decision)
  , Text.intercalate "; " (decisionEvidence decision)
  ]
  where
    question = decisionQuestion decision
    details = questionDetails question

updateDecision
  :: Member Replies effects
  => Response result -> AcceptedDecision -> Eff effects (Either ReplyError RequestUpdate)
updateDecision response = updateRequest response . decisionContext

-- Model placement: the "luna" alias is the cheap, fast tier and the default
-- for bounded implementation and review children; fork many of them. A Luna
-- child starts from fresh context selected from its Task, since a different
-- model cannot reuse this conversation. The "executor" alias (Sol) inherits
-- context and is for children that own design judgment or an integration
-- loop of their own. Effort is always the caller's explicit choice -- there
-- is no default tier (the user's wave-3 decision).
lunaTask :: Label -> ForkEffort -> Task -> Branch CodingEffects Task result
lunaTask label effort = lunaTaskFrom label effort currentCheckout

solTask :: Label -> ForkEffort -> Task -> Branch CodingEffects Task result
solTask label effort = solTaskFrom label effort currentCheckout

-- Source and context are independent choices. currentCheckout selects the
-- executing actor's checkout; an exact
-- committed review seed uses atRef. Fresh context is an explicit withContext.
--
-- Reporting defaults to the Assignment default (NotifyOwner): the requester
-- gets the ordinary settlement notice. A record-actor router that consumes
-- settlement itself (Project.Routing's followWork, or a purpose-built
-- collector like checks/review-continuation.hs's ReviewFlow) builds its own
-- assignment directly with `{ report = Silent }` rather than going through
-- this sugar -- see Project.Review's startReviewer and
-- checks/review-continuation.hs for that shape.
lunaTaskFrom :: Label -> ForkEffort -> WorktreeSeed -> Task -> Branch CodingEffects Task result
lunaTaskFrom label effort source task = withInstructions (projectPrompt "task") $
  withContext (selected taskContext) $ withModel "luna" $ withEffort effort $
  coding source (assignment label task)

solTaskFrom :: Label -> ForkEffort -> WorktreeSeed -> Task -> Branch CodingEffects Task result
solTaskFrom label effort source task = withInstructions (projectPrompt "task") $
  withContext inherited $ withModel "executor" $ withEffort effort $
  coding source (assignment label task)

-- implement exposes no effort parameter of its own; Medium is chosen here
-- because a caller with a bounded, ordinary implementation obligation has
-- nowhere else to pass one through this entry point. A caller that needs a
-- different tier forks directly with lunaTask/lunaTaskFrom instead of going
-- through implement.
implement
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Task -> Eff effects (Response (Outcome Candidate), Progress WorkProgress)
implement task = unfold (taskGroup task) $
  childWithProgress @WorkProgress @(Outcome Candidate) (lunaTask [label|implement|] Medium task)

-- Exact-scope reviews return findings to their requester because there is no
-- owning Task with which to address a retained implementer.
repairOwnerContext :: RepairOwner -> Text
repairOwnerContext owner = case owner of
  OwnerRepairs -> "Repair owner: your requester. Return Repair findings; it will repair and reuse you. Do not queue work behind its pending delivery."
  RetainedImplementer actor -> "Repair owner: retained implementer " <> Text.pack (show actor)
    <> ". Use repair for direct follow-up; keep your review pending while its separate request runs."

reviewContext :: ReviewRequest -> Text
reviewContext request = Text.unlines
  [ basisContext
  , "Candidate: " <> renderGitOid (candidateCommit (reviewInput request))
  , "Claimed checks: " <> Text.intercalate "; " (checkedCommands (reviewInput request))
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

-- Both admission branches of a review fork at Medium: the reviewer's job is
-- to read a bounded diff and judge it, not to carry a design loop, so there
-- is no caller-facing effort parameter here (unlike lunaTask/lunaTaskFrom,
-- where the caller always chooses). Reporting is the Assignment default
-- (NotifyOwner), same reasoning as lunaTaskFrom/solTaskFrom above.
reviewCandidate
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Task -> RepairOwner -> Candidate -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
reviewCandidate task owner candidate = unfold (taskGroup task) $ childWithProgress @WorkProgress @(Outcome ReviewDecision) $
  withInstructions (projectPrompt "review") $ withContext (selected reviewContext) $
  withModel "luna" $ withEffort Medium $
  coding (atRef (GitRef (renderGitOid (candidateCommit candidate))))
    (assignment [label|review|] (ReviewRequest (AssignedTask task) candidate owner))

-- A root review of one exact commit, with no owning Task: useful when the
-- root itself produced or selected the commit (an incorporation, a direct
-- edit) and only needs a judgment against a stated acceptance, not the full
-- fork-group/plan/rationale bookkeeping a Task carries. Same admission shape
-- as reviewCandidate (fixed Medium, same effect constraints). The exact
-- scope stays distinct from a Task in the accepted result.
--
-- Takes its own campaign label rather than nesting under the caller's path
-- (subgroup): the root itself has no allocated actor path to nest under
-- (Task's batch/"work" split has the same shape, one level up -- see
-- Project.Types's task constructor), and a caller-supplied label keeps
-- concurrent reviewCommit calls from the same actor in separate groups.
reviewCommit
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Label -> GitOid -> GitOid -> Text -> [Text] -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
reviewCommit reviewLabel base commit accept owned = unfold (batch (labelCampaign reviewLabel) "review") $ childWithProgress @WorkProgress @(Outcome ReviewDecision) $
  withInstructions (projectPrompt "review") $ withContext (selected reviewContext) $
  withModel "luna" $ withEffort Medium $
  coding (atRef (GitRef (renderGitOid commit)))
    (assignment [label|review|] (ReviewRequest (ExactScope base owned accept) (Candidate commit [] []) OwnerRepairs))

-- This project's automatic review edge selects the committed submission head.
-- Other authored flows may deliberately select earlier artifacts instead.
candidateAtSubmission :: Candidate -> WorktreeEvidence -> Either Text Candidate
candidateAtSubmission candidate evidence = case evidence of
  WorktreeObserved _ _ observation
    | actual == candidateCommit candidate -> Right candidate
    | otherwise -> Left ("candidate " <> renderGitOid (candidateCommit candidate) <> "; submitted " <> renderGitOid actual)
    where actual = headOid (submittedHead observation)
  NoBoundWorktree -> Left "candidate has no bound-source evidence"
  WorktreeObservationFailed failure -> Left (renderWorktreeError failure)

-- Ownership gate: which paths a candidate range actually touched outside its
-- declared ownership. Same numstat parsing as Project.Evidence's pure
-- ownershipCheck, but this runs the diff itself and returns the exact stray
-- paths, for a caller that wants to act on the list rather than read a
-- CheckResult's rendered detail string. A git failure here is a defect in
-- the evidence, the same stance Project.Review's own gitText takes -- never
-- read as an empty, passing diff.
unownedPaths
  :: Member Commands effects
  => GitOid -> GitOid -> [Text] -> Eff effects [Text]
unownedPaths base candidate owned = do
  let range = renderGitOid base <> ".." <> renderGitOid candidate
  result <- Cmd.run (Cmd.argv ["git", "diff", "--numstat", range])
  case Cmd.stdout result of
    Right stat -> pure [path | (_, _, path) <- numstatFiles stat, path `notElem` owned]
    Left issue -> error ("unownedPaths: git diff --numstat " <> Text.unpack range <> " failed: " <> show issue)

-- A completed review attempt leaves its actor available for the revised
-- candidate. Reporting is the Assignment default (NotifyOwner); see
-- lunaTaskFrom's note above.
reviewAgain
  :: Member Replies effects
  => AgentRef -> Label -> ReviewRequest -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
reviewAgain actor label request = requestWithProgress @WorkProgress @(Outcome ReviewDecision) actor $
  (assignment label request) { guidance = Just (projectPrompt "review") }

-- Left is the useful verdict to return to the implementing owner; Right is a
-- separate request to an available implementer. No queue is created for Left.
-- Reporting is the Assignment default (NotifyOwner); see lunaTaskFrom's note
-- above.
repair
  :: Member Replies effects
  => Label -> ReviewRequest -> Candidate -> [Text]
  -> Eff effects (Either ReviewDecision (Response (Outcome Candidate)))
repair label request candidate findings = case reviewBasis request of
  ExactScope _ _ _ -> pure (Left (Repair candidate findings))
  AssignedTask task -> case repairOwner request of
    OwnerRepairs -> pure (Left (Repair candidate findings))
    RetainedImplementer actor -> Right <$> requestWith actor
      ((assignment label (RepairTask task candidate findings))
        { guidance = Just (projectPrompt "repair") })

-- Reporting is the Assignment default (NotifyOwner); see lunaTaskFrom's note
-- above.
requestIncorporation
  :: Member Replies effects
  => AgentRef -> Label -> Task -> PlanAmendment -> Eff effects (Response Incorporation)
requestIncorporation recipient label task amendment = requestWith recipient $
  (assignment label (IncorporationTask task amendment))
    { guidance = Just (projectPrompt "incorporate") }

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
  => DesignSlot -> DesignQuestion -> Eff effects (Response DesignAnswer, Watch (Settlement DesignAnswer))
consultDesign slot question = do
  expert <- unfold (specialistGroup slot) $ child $
    withInstructions (projectPrompt "specialist") $ withContext (selected (designContext slot)) $
    withModel (specialistModel slot) $ withEffort (specialistEffort slot) $
    coding (atRef (GitRef (renderGitOid (questionSource question))))
      (assignment (specialistLabel slot) question)
  ready <- watch (specialistWatch slot) (awaitSettled expert)
  pure (expert, ready)

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
