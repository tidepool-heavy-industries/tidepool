{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.Observe
  ( LaneObservation (..)
  , observeLane
  , RsiInput (..)
  , rsiContext
  , rsiBranch
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (AgentInspection)
import Shoal.Workspace (workspaceIdentity, workspacePrompt)
import Project.Types

-- A projection of existing handles and observations, never another task registry.
data LaneObservation = LaneObservation
  { observedAssignment :: Task
  , observedDefinition :: Text
  , observedOwner :: (Int, Int)
  , observedOwnerVisible :: Bool
  , observedTree :: SwarmSnapshot
  , observedResult :: ResponseState Delivery
  } deriving (Show)

observeLane
  :: (Member AgentInspection effects, Member Replies effects)
  => Task -> Forked Delivery -> Eff effects LaneObservation
observeLane task worker = do
  current <- snapshot
  result <- pollResponse (forkedResponse worker)
  let owner = agentIdentity (forkedActor worker)
  let scope = creationTree owner current
  pure $ LaneObservation task workspaceIdentity owner
    (any (\actor -> (rosterActorId actor, rosterActorIncarnation actor) == owner) (snapshotActors scope))
    scope result

-- An ordinary typed input for a human-requested expert engagement. The sender
-- selects evidence once; the expert can request a precise missing observation.
data RsiInput = RsiInput
  { rsiSource :: Text
  , rsiQuestion :: Text
  , rsiLanes :: [LaneObservation]
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
  , "Requested model groups (not billing attribution): " <> shown (usageByRequestedModel (rsiAfter input))
  ] ++ concatMap laneSummary (rsiLanes input) ++ rsiEvidence input
  where
    shown :: Show value => value -> Text
    shown = Text.pack . show
    laneSummary lane =
      [ "Plan: " <> planPath (observedAssignment lane) <> "; definitions: " <> observedDefinition lane
      , "Owner: " <> shown (observedOwner lane) <> "; visible: " <> shown (observedOwnerVisible lane)
      , "Result: " <> shown (observedResult lane)
      , "Actors (label, exact identity, current requests, queued requests, received requests/events, compactions): "
          <> shown [(rosterLabel actor, (rosterActorId actor, rosterActorIncarnation actor),
               rosterCurrentRequests actor, rosterQueuedRequests actor,
               rosterReceivedRequests actor, rosterReceivedCoordinationEvents actor, rosterCompactions actor)
             | actor <- snapshotActors (observedTree lane)]
      ]

rsiBranch :: BranchLabel -> WorktreeSeed -> RsiInput -> Branch CodingEffects RsiInput Candidate
rsiBranch label seed input =
  withInstructions instructions $ withContext (selected rsiContext) $
  withModel "gpt-6-astra" $ withEffort Medium $ coding label seed input
  where
    instructions = case workspacePrompt "rsi" of
      Just body -> body
      Nothing -> error "Missing configured project prompt: rsi"
