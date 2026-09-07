{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.Observe
  ( WorkObservation (..), observeWork, workSummary, deliverySummary
  , RsiInput (..), rsiContext, rsiBranch
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (AgentInspection)
import Shoal.Workspace (workspaceIdentity)
import Project.Types
import Project.Work (projectPrompt)

-- Existing owned handles and observations are the evidence. This value is a
-- snapshot to inspect or pass on, not another registry or mutable task record.
data WorkObservation = WorkObservation
  { observedAssignment :: Task
  , observedDefinition :: Text
  , observedOwner :: (Int, Int)
  , observedOwnerVisible :: Bool
  , observedTree :: SwarmSnapshot
  , observedResult :: ResponseState Delivery
  , observedAttention :: ProgressState Attention
  } deriving (Show)

observeWork
  :: (Member AgentInspection effects, Member Replies effects)
  => Task -> (Forked Delivery, Progress Attention) -> Eff effects WorkObservation
observeWork task (worker, questions) = do
  current <- snapshot
  result <- pollResponse (forkedResponse worker)
  attention <- pollProgress questions
  let owner = agentIdentity (forkedActor worker)
  let scope = creationTree owner current
  pure $ WorkObservation task workspaceIdentity owner
    (any (\actor -> (rosterActorId actor, rosterActorIncarnation actor) == owner) (snapshotActors scope))
    scope result attention

shown :: Show value => value -> Text
shown = Text.pack . show

deliverySummary :: Delivery -> Text
deliverySummary (Blocked reason evidence) = "Blocked: " <> reason <> "; evidence: " <> Text.intercalate "; " evidence
deliverySummary (Produced (Delivered accepted head checks)) = Text.unlines
  [ "Reviewed: " <> candidateCommit (reviewedCandidate accepted)
  , "Checked resulting head: " <> head
  , "Checks: " <> Text.intercalate "; " checks
  , "Remaining gates: " <> shown (remainingGates (reviewedCandidate accepted))
  ]

workSummary :: WorkObservation -> Text
workSummary observed = Text.unlines
  [ "Plan: " <> planPath (observedAssignment observed) <> "; source: " <> taskSource (observedAssignment observed)
  , "Definitions: " <> observedDefinition observed
  , "Owner: " <> shown (observedOwner observed) <> "; visible: " <> shown (observedOwnerVisible observed)
  , "Outcome: " <> case observedResult observed of
      ResponsePending -> "pending"
      ResponseCancellationPending reason -> "cancellation pending: " <> shown reason
      ResponseUnavailable failure -> "unavailable: " <> shown failure
      ResponseReady result -> deliverySummary (responseValue result)
  , "Questions: " <> case observedAttention observed of
      ProgressPending -> "no publication observed"
      ProgressUpdate _ questions -> shown [(questionKey q, questionPlan (questionDetails q), questionSource (questionDetails q), questionFinding (questionDetails q)) | q <- questions]
      ProgressClosed -> "progress closed; retained outcomes/watches carry earlier evidence"
      ProgressRejected failure -> "unavailable: " <> shown failure
  , "Requested-model usage (coverage retained): " <> shown (usageByRequestedModel (observedTree observed))
  , "Actors (label, identity, model, lifecycle/provider state/staleness, current/queued requests, received requests/events, compactions): "
      <> shown [(rosterLabel actor, (rosterActorId actor, rosterActorIncarnation actor), rosterRequestedModel actor,
           (rosterState actor, rosterProviderHealth actor, rosterProviderObservationStale actor),
           rosterCurrentRequests actor, rosterQueuedRequests actor,
           rosterReceivedRequests actor, rosterReceivedCoordinationEvents actor, rosterCompactions actor)
         | actor <- snapshotActors (observedTree observed)]
  ]

-- The human's question selects a useful view, not a permanent monitoring actor.
data RsiInput = RsiInput
  { rsiSource :: Text
  , rsiQuestion :: Text
  , rsiWork :: [WorkObservation]
  , rsiBefore :: SwarmSnapshot
  , rsiAfter :: SwarmSnapshot
  , rsiEvidence :: [Text]
  } deriving (Show)

rsiContext :: RsiInput -> Text
rsiContext input = Text.unlines $
  [ "Question: " <> rsiQuestion input
  , "Source: " <> rsiSource input
  , "Current definitions: " <> workspaceIdentity
  , "Usage interval (newly visible history is separate): " <> shown (usageDelta (rsiBefore input) (rsiAfter input))
  ] ++ map workSummary (rsiWork input) ++ rsiEvidence input

rsiBranch :: BranchLabel -> WorktreeSeed -> RsiInput -> Branch CodingEffects RsiInput (Outcome Candidate)
rsiBranch label seed input = withInstructions (projectPrompt "rsi") $
  withContext (selected rsiContext) $ withModel "gpt-6-astra" $ withEffort Medium $
  coding label seed input
