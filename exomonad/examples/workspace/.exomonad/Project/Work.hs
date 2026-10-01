{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- Branches and exact-source review for recursive local work. Importing the
-- module admits nothing; the execution owner composes each ready frontier.
module Project.Work
  ( task, labelCampaign, projectPrompt, taskContext, reviewContext, decisionContext
  , withDecision, updateDecision, designQuestion, raiseQuestion, resolveQuestion
  , lunaTask, lunaTaskFrom, lunaTaskInputFrom, lunaLead, lunaLeadFrom
  , solTask, solTaskFrom, implement, reviewCandidate, reviewCommit, requestReview, repair
  , candidateAtSubmission, reviewCandidateAtSubmission, admitReviewedCheckpoint
  , RequestHandoff (..), requestIncorporation, consultDesign
  , settledValue
  , unownedPaths
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Exomonad
import Tidepool.Agent.Assignment (labelText)
import Tidepool.Effects.Core (AgentInspection, Commands, Forks, GitRef (..))
import Tidepool.Inspection (WorkbenchDisplay)
import Tidepool.Worktree (renderGitOid)
import qualified Tidepool.Command as Cmd
import Exomonad.Contrib.Types
import Project.Evidence (numstatFiles)
import Exomonad.Workspace (workspacePrompt)

-- Project task defaults keep the shared data types free of workspace paths.
labelCampaign :: Label -> CampaignLabel
labelCampaign label = either (error . show) id (campaignLabel (labelText label))

task :: Label -> Text -> [Text] -> Text -> GitOid -> Task
task label objective owned accept source = Task
  { taskGroup = batch (labelCampaign label) "work"
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
  => Response result -> AcceptedDecision -> Eff effects (Either ReplyError RequestUpdate)
updateDecision response = updateRequest response . decisionContext

-- Model placement: the "luna" alias is the cheap, fast tier and the default
-- for bounded implementation, recursive component ownership and review.
-- Selected Task context makes immediate admission explicit across model tiers.
-- Sol is available for consequential design uncertainty.
-- Effort remains an explicit choice at each branch.
lunaTask :: Label -> ForkEffort -> Task -> Branch CodingEffects Task result
lunaTask label effort = lunaTaskFrom label effort currentCheckout

-- Component ownership changes instructions and result contract, not runtime
-- authority. The caller can override context with the ordinary branch modifier.
lunaLead :: Label -> ForkEffort -> Task -> Branch CodingEffects Task Delivery
lunaLead label effort = lunaLeadFrom label effort currentCheckout

lunaLeadFrom :: Label -> ForkEffort -> WorktreeSeed -> Task -> Branch CodingEffects Task Delivery
lunaLeadFrom label effort source work =
  withInstructions (projectPrompt "lead") (lunaTaskFrom label effort source work)

solTask :: Label -> ForkEffort -> Task -> Branch CodingEffects Task result
solTask label effort = solTaskFrom label effort currentCheckout

-- Source and context are independent choices. currentCheckout selects the
-- executing actor's checkout; an exact
-- committed review seed uses atRef. Fresh context is an explicit withContext.
--
-- An ordinary assignment reports settlement to its requester. Exomonad.Contrib.Routing's
-- workChild selects Silent when the batch collector owns that notification.
-- Custom collectors should likewise select one notification owner explicitly.
lunaTaskFrom :: Label -> ForkEffort -> WorktreeSeed -> Task -> Branch CodingEffects Task result
lunaTaskFrom label effort source = lunaTaskInputFrom label effort source taskContext

-- | Selected-context task work can carry typed project data alongside its
-- rendered context, including an opaque actor handle allocated before fork.
-- Keep the task role prompt and Luna placement in this single owner.
lunaTaskInputFrom
  :: WorkbenchDisplay input
  => Label -> ForkEffort -> WorktreeSeed -> (input -> Text) -> input
  -> Branch CodingEffects input result
lunaTaskInputFrom label effort source context input = withInstructions (projectPrompt "task") $
  withContext (selected context) $ withModel "luna" $ withEffort effort $
  coding source (assignment label input)

solTaskFrom :: Label -> ForkEffort -> WorktreeSeed -> Task -> Branch CodingEffects Task result
solTaskFrom label effort source task = withInstructions (projectPrompt "task") $
  withContext (selected taskContext) $ withModel "executor" $ withEffort effort $
  coding source (assignment label task)

-- These response-returning project helpers hand unfinished work to later cells,
-- so their branches explicitly use ActorOwned. Lower-level branch constructors
-- leave lifetime selectable and scoped by default.
--
-- implement exposes no effort parameter of its own; Medium is chosen here
-- because a caller with a bounded, ordinary implementation obligation has
-- nowhere else to pass one through this entry point. A caller that needs a
-- different tier forks directly with lunaTask/lunaTaskFrom instead of going
-- through implement.
implement
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Task -> Eff effects (Response (Outcome Candidate), Progress WorkProgress)
implement task = unfold (taskGroup task) $
  childWithProgress @WorkProgress @(Outcome Candidate) (withLifetime ActorOwned $ lunaTask [label|implement|] Medium task)

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

-- Both admission branches of a review fork at Medium: the reviewer's job is
-- to read a bounded diff and judge it, not to carry a design loop, so there
-- is no caller-facing effort parameter here (unlike lunaTask/lunaTaskFrom,
-- where the caller always chooses). Reporting is the Assignment default
-- (NotifyOwner), same reasoning as lunaTaskFrom/solTaskFrom above.
reviewCandidate
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Task -> RepairOwner -> Candidate -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
reviewCandidate task owner candidate =
  admitReview (taskGroup task) (ReviewRequest (AssignedTask task) candidate owner)

-- A root review of one exact commit, with no owning Task: useful when the
-- root itself produced or selected the commit (an incorporation, a direct
-- edit) and only needs a judgment against a stated acceptance, not the full
-- fork-group/plan/rationale bookkeeping a Task carries. Same admission shape
-- as reviewCandidate (fixed Medium, same effect constraints). The exact
-- scope stays distinct from a Task in the accepted result.
--
-- The caller supplies an independent campaign label for each root review.
reviewCommit
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Label -> GitOid -> GitOid -> Text -> [Text] -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
reviewCommit reviewLabel base commit accept owned = requestReview reviewLabel $
  ReviewRequest (ExactScope base owned accept) (Candidate commit [] []) OwnerRepairs

-- After the original review response settles, a producer can call
-- 'admitReviewedCheckpoint' with its ReviewRequest and Response handle, then
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

-- A revised candidate gets its own exact checkout. A retained actor's previous
-- checkout is never implicitly treated as the source named by a new request.
requestReview
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => Label -> ReviewRequest -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
requestReview reviewLabel = admitReview (batch (labelCampaign reviewLabel) "review")

-- All review entry points share model placement, exact-source admission and
-- the original typed request. Scope construction belongs to their callers.
admitReview
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => ForkGroupPath -> ReviewRequest -> Eff effects (Response (Outcome ReviewDecision), Progress WorkProgress)
admitReview group request = unfold group $
  childWithProgress @WorkProgress @(Outcome ReviewDecision) $
    withLifetime ActorOwned $ withInstructions (projectPrompt "review") $ withContext (selected reviewContext) $
    withModel "luna" $ withEffort Medium $
    coding (atRef (GitRef (renderGitOid (candidateCommit (reviewInput request)))))
      (assignment [label|review|] request)

-- Left is the useful verdict to return to the implementing owner; Right is a
-- separate request to an available implementer. No queue is created for Left.
-- Reporting is the Assignment default (NotifyOwner); see lunaTaskFrom's note
-- above.
repair
  :: Member Replies effects
  => Label -> ReviewRequest -> Candidate -> [Text]
  -> Eff effects (Either ReviewDecision (RequestHandoff (Outcome Candidate)))
repair label reviewRequest candidate findings = case reviewBasis reviewRequest of
  ExactScope _ _ _ -> pure (Left (Repair candidate findings))
  AssignedTask task -> case repairOwner reviewRequest of
    OwnerRepairs -> pure (Left (Repair candidate findings))
    RetainedImplementer actor -> do
      response <- request actor
        ((assignment label (RepairTask task candidate findings))
          { guidance = Just (projectPrompt "repair") })
      Right <$> retainRequest response

-- Reporting is the Assignment default (NotifyOwner); see lunaTaskFrom's note
-- above.
requestIncorporation
  :: Member Replies effects
  => AgentRef -> Label -> Task -> PlanAmendment -> Eff effects (RequestHandoff Incorporation)
requestIncorporation recipient label task amendment = do
  response <- request recipient $
    (assignment label (IncorporationTask task amendment))
      { guidance = Just (projectPrompt "incorporate") }
  retainRequest response

-- A retention receipt proves only transfer of this request to actor ownership.
-- A refusal leaves invocation ownership and its ordinary scope cleanup intact;
-- the original response remains available to inspect or cancel.
data RequestHandoff value = RequestHandoff
  { handedRequest :: Response value
  , handoffRetention :: Either ReplyError ()
  } deriving (Show)

retainRequest :: Member Replies effects => Response value -> Eff effects (RequestHandoff value)
retainRequest response = RequestHandoff response <$> detachRequest response

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
  :: (Member Forks effects, Member Replies effects, Member Watches effects, Member AgentInspection effects, Subset CodingEffects effects)
  => DesignSlot -> DesignQuestion -> Eff effects (Response DesignAnswer, Watch (Settlement DesignAnswer))
consultDesign slot question = do
  expert <- unfold (specialistGroup slot) $ child $
    withLifetime ActorOwned $ withInstructions (projectPrompt "specialist") $ withContext (selected (designContext slot)) $
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
