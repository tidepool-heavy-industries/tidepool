{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.Observe
  ( WorkObservation (..), observeWork, workSummary, deliverySummary
  , candidateSummary, reviewSummary, attentionSummary, progressSummary, workSnapshotSummary
  , workingAndAbnormal, actorSummary
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
import Project.Routing (WorkState (..), WorkSource (..), WorkEvent (..), WorkDelta (..), outstandingEvidence)

-- Existing owned handles and observations are the evidence. This value is a
-- snapshot to inspect or pass on, not another registry or mutable task record.
data WorkObservation = WorkObservation
  { observedAssignment :: Task
  , observedDefinition :: Text
  , observedOwner :: (Int, Int)
  , observedOwnerVisible :: Bool
  , observedTree :: SwarmSnapshot
  , observedResult :: ResponseState Delivery
  , observedProgress :: ProgressState WorkProgress
  } deriving (Show)

observeWork
  :: (Member AgentInspection effects, Member Replies effects)
  => Task -> (Forked Delivery, Progress WorkProgress) -> Eff effects WorkObservation
observeWork task (worker, progress) = do
  current <- snapshot
  result <- pollResponse (forkedResponse worker)
  observation <- pollProgress progress
  let owner = agentIdentity (forkedActor worker)
  let scope = creationTree owner current
  pure $ WorkObservation task workspaceIdentity owner
    (any (\actor -> (rosterActorId actor, rosterActorIncarnation actor) == owner) (snapshotActors scope))
    scope result observation

shown :: Show value => value -> Text
shown = Text.pack . show

candidateSummary :: Outcome Candidate -> Text
candidateSummary (Blocked reason evidence) = blockedSummary reason evidence
candidateSummary (Produced candidate) = candidateRef candidate

reviewSummary :: Outcome ReviewDecision -> Text
reviewSummary (Blocked reason evidence) = blockedSummary reason evidence
reviewSummary (Produced (Repair candidate findings)) =
  "repair " <> candidateCommit candidate <> ": " <> Text.intercalate "; " findings
reviewSummary (Produced (Accepted accepted)) =
  "accepted " <> candidateRef (reviewedCandidate accepted)
    <> "; review checks " <> shown (length (reviewChecks accepted))

deliverySummary :: Delivery -> Text
deliverySummary (Blocked reason evidence) = blockedSummary reason evidence
deliverySummary (Produced (Delivered accepted head checks)) =
  head <> "; reviewed " <> candidateRef (reviewedCandidate accepted)
    <> "; integration checks " <> shown (length checks)

candidateRef :: Candidate -> Text
candidateRef candidate = candidateCommit candidate
  <> "; checks " <> shown (length (checkedCommands candidate))
  <> (if null (remainingGates candidate) then ""
      else "; gates " <> Text.intercalate "; " (remainingGates candidate))

blockedSummary :: Text -> [Text] -> Text
blockedSummary reason evidence = "blocked " <> reason <> "; " <> Text.intercalate "; " evidence

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
  , "Progress: " <> case observedProgress observed of
      ProgressPending -> "no publication observed"
      ProgressUpdate _ progress -> shown (progressSummary progress)
      ProgressClosed -> "progress closed; wave router retains earlier evidence"
      ProgressRejected failure -> "unavailable: " <> shown failure
  , "Requested-model usage (coverage retained): " <> shown (usageByRequestedModel (observedTree observed))
  , "Working/abnormal actors (identity, label, lifecycle, provider, stale, current/queued): "
      <> shown (actorSummary (workingAndAbnormal (observedTree observed)))
  ]

-- Small views for the next decision. Keep original values for full evidence.
attentionSummary :: Attention -> [(Text, Text, Text)]
attentionSummary = map (\q -> (questionKey q, questionSource (questionDetails q), questionFinding (questionDetails q)))

progressSummary :: WorkProgress -> ([(Text, [Text])], [(Text, Text, Text)])
progressSummary progress =
  ([(candidateCommit candidate, remainingGates candidate) | candidate <- workEvidence progress]
  , attentionSummary (workQuestions progress))

-- Render the existing collector without copying its state or consuming notices.
-- The caller chooses how much of a final value belongs in this view.
workSnapshotSummary :: (value -> Text) -> WorkState value -> Text
workSnapshotSummary render state = Text.unlines (map sourceLine (collectedWork state))
  where
    sourceLine source = sourceName source <> " " <> shown (sourceStatus source)
      <> "; candidates " <> Text.intercalate ", "
        [candidateRef candidate <> " events " <> shown
          [index | (index, WorkChanged name delta) <- zip [0 :: Int ..] (workHistory state),
            name == sourceName source, candidate `elem` addedEvidence delta]
        | candidate <- outstandingEvidence state source]
      <> "; questions " <> Text.intercalate ", "
        [questionKey q <> "@" <> questionSource (questionDetails q)
        | q <- workQuestions (sourceProgress source)]
      <> "; result " <> case sourceResult source of
        Nothing -> "pending"
        Just (Left failure) -> "unavailable " <> shown failure
        Just (Right result) -> render (responseValue result)

-- Preserve full rows for drill-down. This is a view, not cleanup authorization;
-- an omitted actor is not thereby proven safe to retire.
workingAndAbnormal :: SwarmSnapshot -> SwarmSnapshot
workingAndAbnormal current = current { snapshotActors = filter relevant (snapshotActors current) }
  where
    relevant actor = rosterProviderObservationStale actor
      || not (null (rosterCurrentRequests actor) && null (rosterQueuedRequests actor))
      || case rosterState actor of
        RosterFailed _ -> True
        RosterCancelled _ -> True
        RosterStopped -> case rosterProviderHealth actor of
          ProviderSucceeded -> False
          _ -> True
        RosterRunning -> case rosterDisposition actor of
          Just IdleRetained -> False
          _ -> True

actorSummary :: SwarmSnapshot -> [((Int, Int), Text, AgentRosterState, ProviderHealth, Bool, ([Int], [Int]))]
actorSummary current =
  [ ((rosterActorId actor, rosterActorIncarnation actor), rosterLabel actor,
      rosterState actor, rosterProviderHealth actor, rosterProviderObservationStale actor,
      (rosterCurrentRequests actor, rosterQueuedRequests actor))
  | actor <- snapshotActors current
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
