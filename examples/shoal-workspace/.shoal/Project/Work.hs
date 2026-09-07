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
  , integrateReviewed
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (AgentInspection, Forks, GitRef (..))
import Project.Types
import Shoal.Workspace (workspacePrompt)

-- Missing project configuration is an error, never an instruction-free worker.
data Instructions = TaskInstructions | ReviewInstructions | RepairInstructions | IntegrationInstructions

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
