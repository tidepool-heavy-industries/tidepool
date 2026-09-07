{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

module Project.Work
  ( taskContext
  , solTask
  , specialistTask
  , implement
  , reviewCandidate
  , requestRepair
  , requestIncorporation
  , integrateReviewed
  , deliverLane
  , consultDesign
  , settledValue
  ) where

import Control.Monad.Freer (Eff, Member)
import Control.Monad (void)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (AgentInspection, Forks, GitRef (..))
import Project.Types
import Shoal.Workspace (workspacePrompt)

-- Missing project configuration is an error, never an instruction-free worker.
data Instructions = TaskInstructions | ReviewInstructions | RepairInstructions | IntegrationInstructions | DesignInstructions | IncorporationInstructions

instructions :: Instructions -> Text
instructions kind = case workspacePrompt name of
  Just body -> body
  Nothing -> error ("Missing configured project prompt: " <> Text.unpack name)
  where
    name = case kind of
      TaskInstructions -> "task"
      ReviewInstructions -> "review"
      RepairInstructions -> "repair"
      IntegrationInstructions -> "integrate"
      DesignInstructions -> "specialist"
      IncorporationInstructions -> "incorporate"

-- Keep stable project vocabulary in modules; send the branch's relevant plan,
-- rationale and acceptance rather than a transcript or repeated status digest.
taskContext :: Task -> Text
taskContext task = Text.unlines
  [ "Plan: " <> planPath task
  , "Obligation: " <> obligation task
  , "Acceptance: " <> acceptance task
  ]

solTask :: BranchLabel -> WorktreeSeed -> Task -> Branch CodingEffects Task result
solTask label seed task =
  withInstructions (instructions TaskInstructions) $
  withContext (selected taskContext) $
  withModel "gpt-5.6-sol" $ withEffort Low $ coding label seed task

-- Use only at the specialist obligations tagged in the authored plan. Keep the
-- expert alive to finish; surface cost/architecture choices through ordinary talk.
specialistTask :: BranchLabel -> WorktreeSeed -> Task -> Branch CodingEffects Task result
specialistTask label seed task =
  withInstructions (instructions TaskInstructions) $
  withContext (selected taskContext) $
  withModel "gpt-6-astra" $ withEffort Medium $ coding label seed task

implement
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => ForkGroupPath -> BranchLabel -> WorktreeSeed -> Task -> Eff effects (Forked Candidate)
implement group label seed task = unfold group (child (solTask label seed task))

-- A route callback can invoke these recipes directly. It runs as the route
-- owner's actor, so its handles/permissions still belong to that owner.
reviewContext :: ReviewTask -> Text
reviewContext task = Text.unlines
  [ taskContext (reviewAssignment task)
  , "Candidate: " <> candidateCommit (reviewInput task)
  , "Claimed checks: " <> Text.intercalate "; " (checkedCommands (reviewInput task))
  , "Remaining product gates: " <> Text.intercalate "; " (remainingGates (reviewInput task))
  , "Retained implementer: " <> Text.pack (show (agentIdentity (reviewImplementer task)))
  ]

reviewCandidate
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => ForkGroupPath -> BranchLabel -> Task -> AgentRef -> Candidate -> Eff effects (Forked ReviewDecision)
reviewCandidate group label task implementer candidate =
  unfold group $ child $ withInstructions (instructions ReviewInstructions) $
    withContext (selected reviewContext) $
    withModel "gpt-5.6-sol" $ withEffort Low $
    coding label (atRef (GitRef (candidateCommit candidate))) (ReviewTask task candidate implementer)

requestRepair
  :: Member Replies effects
  => RequestLabel -> ReviewTask -> Candidate -> [Text] -> Eff effects (Response Candidate)
requestRepair label review candidate findings =
  requestWith (reviewImplementer review) $
    withRequestGuidance (instructions RepairInstructions) $
    requestOptions label (RepairTask (reviewAssignment review) candidate findings)

-- Invoke after the owning decision accepts the amendment. Never queue this back
-- to a lead already waiting on this request. The retained implementer is free
-- after its candidate reply; the review retains its own original reply handle.
requestIncorporation
  :: Member Replies effects
  => AgentRef -> RequestLabel -> Task -> PlanAmendment -> Eff effects (Response Incorporation)
requestIncorporation recipient label assignment amendment =
  requestWith recipient $
    withRequestGuidance (instructions IncorporationInstructions) $
    requestOptions label (IncorporationTask assignment amendment)

integrateReviewed
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset IntegrationEffects effects)
  => ForkGroupPath -> BranchLabel -> WorktreeSeed -> ReviewedCandidate -> Eff effects (Forked Delivery)
integrateReviewed group label seed candidate =
  unfold group $ child $ withInstructions (instructions IntegrationInstructions) $
    withContext (selected integrationContext) $
    withModel "gpt-5.6-sol" $ withEffort Low $ integrating label seed candidate
  where
    integrationContext value = "Accepted commit: " <> candidateCommit (reviewedCandidate value)
      <> "; reviewed head: " <> reviewHead value
      <> "; review checks: " <> Text.intercalate ", " (reviewChecks value)
      <> "; rationale: " <> reviewRationale value
      <> "; remaining product gates: "
      <> Text.intercalate ", " (remainingGates (reviewedCandidate value))

-- Install the declared chain and return promptly. Only exceptional local
-- decisions need a lead's model turn; the final result settles its owned reply.
deliverLane
  :: (Member Forks effects, Member Replies effects, Member Watches effects, Member AgentInspection effects, Subset CodingEffects effects, Subset IntegrationEffects effects)
  => DeliveryLane -> Reply Delivery -> Eff effects Route
deliverLane lane destination = do
  candidate <- implement (implementationGroup lane) (implementationLabel lane)
    (implementationSeed lane) (laneTask lane)
  onResult destination (awaitSettledFork candidate) $ \value -> do
    reviewed <- reviewCandidate (reviewGroup lane) (reviewLabel lane)
      (laneTask lane) (forkedActor candidate) value
    void $ onResult destination (awaitSettledFork reviewed) $ \decision -> case decision of
      Accepted exact -> do
        integrated <- integrateReviewed (integrationGroup lane) (integrationLabel lane)
          (integrationSeed lane) exact
        void $ onResult destination (awaitSettledFork integrated) (void . reply destination)
      Repair exact finding -> void $ reply destination (ReviewBlocked exact finding)
      NeedsDesign question -> void $ reply destination (DesignBlocked question)

onResult
  :: (Member Watches effects, Member Replies effects)
  => Reply Delivery -> Await (Settlement result) -> (result -> Eff effects ()) -> Eff effects Route
onResult destination awaiting continuation = route awaiting $ \settled -> case settled of
  ReplyAvailable answer -> continuation (responseValue answer)
  ReplyUnavailable failure -> void $ reply destination (ExecutionUnavailable failure)

-- The waiting actor keeps its original obligation while a declared specialist
-- answers the narrow question. Only that useful answer wakes this owner.
consultDesign
  :: (Member Forks effects, Member Replies effects, Member Watches effects, Member AgentInspection effects, Subset CodingEffects effects)
  => DesignSlot -> DesignQuestion -> Eff effects (Forked DesignAnswer, Watch (Settlement DesignAnswer))
consultDesign slot question = do
  expert <- unfold (specialistGroup slot) $ child $
    withInstructions (instructions DesignInstructions) $
    withContext (selected (designContext slot)) $
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
