{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

module Project.Work
  ( taskContext
  , solTask
  , specialistTask
  , implement
  , reviewCandidate
  , integrateReviewed
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (AgentInspection, Forks, GitRef (..))
import Project.Types

-- Keep stable project vocabulary in modules; send the branch's relevant plan,
-- rationale and acceptance rather than a transcript or repeated status digest.
taskContext :: Task -> Text
taskContext task = Text.unlines
  [ "Read the branch plan: " <> planPath task
  , "Obligation: " <> obligation task
  , "Acceptance: " <> acceptance task
  , "Reply with exact commit/check evidence and explicit remaining gates."
  ]

solTask :: BranchLabel -> WorktreeSeed -> Task -> Branch CodingEffects Task result
solTask label seed task =
  withContext (selected taskContext) $
  withModel "gpt-5.6-sol" $ withEffort Low $ coding label seed task

-- Use only at the specialist obligations tagged in the authored plan. Keep the
-- expert alive to finish; surface cost/architecture choices through ordinary talk.
specialistTask :: BranchLabel -> WorktreeSeed -> Task -> Branch CodingEffects Task result
specialistTask label seed task =
  withContext (selected taskContext) $
  withModel "gpt-6-astra" $ withEffort Medium $ coding label seed task

implement
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => ForkGroupPath -> BranchLabel -> WorktreeSeed -> Task -> Eff effects (Forked Candidate)
implement group label seed task = unfold group (child (solTask label seed task))

-- A route callback can invoke these recipes directly. It runs as the route
-- owner's actor, so its handles/permissions still belong to that owner.
reviewCandidate
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset CodingEffects effects)
  => ForkGroupPath -> BranchLabel -> Task -> Candidate -> Eff effects (Forked Review)
reviewCandidate group label task candidate =
  unfold group $ child $ solTask label (atRef (GitRef (candidateCommit candidate))) $
    task { obligation = "Independently review " <> candidateCommit candidate <> ". " <> obligation task }

integrateReviewed
  :: (Member Forks effects, Member Replies effects, Member AgentInspection effects, Subset IntegrationEffects effects)
  => ForkGroupPath -> BranchLabel -> WorktreeSeed -> Candidate -> Eff effects (Forked Delivery)
integrateReviewed group label seed candidate =
  unfold group $ child $ withContext (selected integrationContext) $
    withModel "gpt-5.6-sol" $ withEffort Low $ integrating label seed candidate
  where
    integrationContext value = "Integrate the accepted exact commit " <> candidateCommit value
      <> "; verify the resulting head with the owning focused checks. Return Integrated only after those checks pass. Preserve partial gates: "
      <> Text.intercalate ", " (remainingGates value)
