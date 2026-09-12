{-# LANGUAGE FlexibleContexts #-}

-- | Cheap control-plane projections. Reading these values never asks a model
-- to summarize its work. Usage is deduplicated by provider thread identity.
module Tidepool.Actors.Observe
  ( SwarmSnapshot (..)
  , UsageTotal (..)
  , UsageDelta (..)
  , snapshot
  , subtree
  , creationTree
  , shareObservation
  , ObservationShareResult (..)
  , swarmUsage
  , usageByRequestedModel
  , usageDelta
  , actorContext
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Data.List (nub)
import Data.Maybe (isNothing)
import Prelude
import Tidepool.Actors.Internal.Agent (AgentRef, agentIdentity, listAgents)
import Tidepool.Effects.Core

newtype SwarmSnapshot = SwarmSnapshot { snapshotActors :: [AgentRosterEntry] }
  deriving (Show)

data UsageTotal = UsageTotal
  { totalCachedInput :: Int
  , totalUncachedInput :: Int
  , totalOutput :: Int
  , totalReasoningOutput :: Int
  , totalTokens :: Int
  , observedProviderThreads :: [Text]
  , unknownActors :: [(Int, Int)]
  , partialProviderThreads :: [Text]
  , inconsistentProviderThreads :: [Text]
  } deriving (Show, Eq)

data UsageDelta = UsageDelta
  { comparableUsage :: UsageTotal
  , newlyObservedUsage :: UsageTotal
  , lostProviderThreads :: [Text]
  , discontinuousProviderThreads :: [Text]
  } deriving (Show, Eq)

snapshot :: Member AgentInspection effects => Eff effects SwarmSnapshot
snapshot = SwarmSnapshot <$> listAgents

shareObservation :: Member AgentInspection effects => AgentRef -> AgentRef -> Eff effects ObservationShareResult
shareObservation recipient scope = send (AgentShareObservationWith (agentIdentity recipient) (agentIdentity scope))

-- | Supervision descendants, including the selected actor. Context inheritance
-- is a separate relationship and does not determine subtree accounting.
subtree :: (Int, Int) -> SwarmSnapshot -> SwarmSnapshot
subtree = treeBy (\actor -> (rosterSupervisorId actor, rosterSupervisorIncarnation actor))

-- | Creation provenance includes independently owned workers. It does not
-- imply that retiring the creator will retire the members of this tree.
creationTree :: (Int, Int) -> SwarmSnapshot -> SwarmSnapshot
creationTree = treeBy (\actor -> (rosterCreatorId actor, rosterCreatorIncarnation actor))

treeBy :: (AgentRosterEntry -> (Maybe Int, Maybe Int)) -> (Int, Int) -> SwarmSnapshot -> SwarmSnapshot
treeBy parentOf root (SwarmSnapshot actors) = SwarmSnapshot (filter belongs actors)
  where
    identity actor = (rosterActorId actor, rosterActorIncarnation actor)
    belongs actor = reaches [] (identity actor)
    reaches seen key
      | key == root = True
      | key `elem` seen = False
      | otherwise = case filter ((== key) . identity) actors of
          actor : _ -> case parentOf actor of
            (Just parent, Just incarnation) -> reaches (key : seen) (parent, incarnation)
            _ -> False
          _ -> False

swarmUsage :: SwarmSnapshot -> UsageTotal
swarmUsage (SwarmSnapshot actors) = summarize actors

-- | Group by the requested launch model, not an inferred billing model. A
-- thread spanning different or unspecified selections belongs in Nothing.
usageByRequestedModel :: SwarmSnapshot -> [(Maybe Text, UsageTotal)]
usageByRequestedModel (SwarmSnapshot actors) =
  [(model, summarize rows) | model <- nub (map selection groups),
    let rows = concat (filter ((== model) . selection) groups)]
  where
    groups = map snd (threadGroups actors) ++ [[actor] | actor <- actors, threadKey actor == Nothing]
    selection rows = case nub (map rosterRequestedModel rows) of
      [model] -> model
      _ -> Nothing

-- | Subtract comparable cumulative observations. Newly visible threads are
-- reported separately: their history may predate the first snapshot. Missing
-- observations and changed origins never become zero spend or negative spend.
usageDelta :: SwarmSnapshot -> SwarmSnapshot -> UsageDelta
usageDelta (SwarmSnapshot before) (SwarmSnapshot after) = UsageDelta
  { comparableUsage = foldl addChange (emptyUsage { unknownActors = unknownActors (summarize after) }) pairs
  , newlyObservedUsage = summarize (concat [rows | (key, rows) <- newGroups, isNothing (lookup key oldGroups)])
  , lostProviderThreads = [key | (key, _) <- oldGroups, isNothing (lookup key newGroups)]
  , discontinuousProviderThreads = [key | (key, old, new) <- pairs, isNothing (comparable old new)]
  }
  where
    oldGroups = threadGroups before
    newGroups = threadGroups after
    pairs = [(key, old, new) | (key, new) <- newGroups, Just old <- [lookup key oldGroups]]
    comparable old new = do
      earlier <- newest old
      later <- newest new
      oldUsage <- rosterUsageSummary earlier
      newUsage <- rosterUsageSummary later
      if sameOrigin earlier later && dominates newUsage oldUsage
        then Just newUsage
          { usageSummaryCachedInputTokens = usageSummaryCachedInputTokens newUsage - usageSummaryCachedInputTokens oldUsage
          , usageSummaryUncachedInputTokens = usageSummaryUncachedInputTokens newUsage - usageSummaryUncachedInputTokens oldUsage
          , usageSummaryOutputTokens = usageSummaryOutputTokens newUsage - usageSummaryOutputTokens oldUsage
          , usageSummaryReasoningTokens = usageSummaryReasoningTokens newUsage - usageSummaryReasoningTokens oldUsage
          , usageSummaryTotalTokens = usageSummaryTotalTokens newUsage - usageSummaryTotalTokens oldUsage
          , usageSummaryCompleteness = if complete oldUsage && complete newUsage then UsageComplete else UsagePartial
          }
        else Nothing
    addChange total (key, old, new) = maybe total (addUsage key total) (comparable old new)

emptyUsage :: UsageTotal
emptyUsage = UsageTotal 0 0 0 0 0 [] [] [] []

identity :: AgentRosterEntry -> (Int, Int)
identity actor = (rosterActorId actor, rosterActorIncarnation actor)

threadKey :: AgentRosterEntry -> Maybe Text
threadKey actor = case rosterUsageSummary actor of
  Just usage -> case usageSummaryScope usage of
    UsageThread key -> Just key
    _ -> Nothing
  _ -> Nothing

threadGroups :: [AgentRosterEntry] -> [(Text, [AgentRosterEntry])]
threadGroups actors = [(key, filter ((== Just key) . threadKey) actors)
  | key <- nub [key | actor <- actors, Just key <- [threadKey actor]]]

sameOrigin :: AgentRosterEntry -> AgentRosterEntry -> Bool
sameOrigin a b = case (rosterFirstUsage a, rosterFirstUsage b) of
  (Just x, Just y) -> usageObservationId x == usageObservationId y
  _ -> False

complete :: ProviderUsageSummary -> Bool
complete usage = case usageSummaryCompleteness usage of
  UsageComplete -> True
  UsagePartial -> False

dominates :: ProviderUsageSummary -> ProviderUsageSummary -> Bool
dominates newer older = and
  [ field newer >= field older
  | field <- [usageSummaryObservations, usageSummaryCachedInputTokens,
      usageSummaryUncachedInputTokens, usageSummaryOutputTokens,
      usageSummaryReasoningTokens, usageSummaryTotalTokens]
  ]

-- Multiple actor incarnations may observe the same provider thread. Select an
-- aggregate that includes every other observation, or expose the inconsistency.
newest :: [AgentRosterEntry] -> Maybe AgentRosterEntry
newest [] = Nothing
newest rows = case filter coversAll rows of
  winner : _ -> Just winner
  [] -> Nothing
  where
    coversAll candidate = all (covers candidate) rows
    covers candidate other = case (rosterUsageSummary candidate, rosterUsageSummary other) of
      (Just x, Just y) -> dominates x y &&
        (identity candidate == identity other || sameOrigin candidate other)
      _ -> False

summarize :: [AgentRosterEntry] -> UsageTotal
summarize actors = foldl addGroup initial (threadGroups actors)
  where
    initial = emptyUsage { unknownActors = map identity (filter ((== Nothing) . threadKey) actors) }
    addGroup total (key, rows) = case newest rows >>= rosterUsageSummary of
      Just usage -> addUsage key total usage
      Nothing -> total
        { unknownActors = map identity rows ++ unknownActors total
        , inconsistentProviderThreads = key : inconsistentProviderThreads total
        }

addUsage :: Text -> UsageTotal -> ProviderUsageSummary -> UsageTotal
addUsage key total usage = total
  { totalCachedInput = totalCachedInput total + usageSummaryCachedInputTokens usage
  , totalUncachedInput = totalUncachedInput total + usageSummaryUncachedInputTokens usage
  , totalOutput = totalOutput total + usageSummaryOutputTokens usage
  , totalReasoningOutput = totalReasoningOutput total + usageSummaryReasoningTokens usage
  , totalTokens = totalTokens total + usageSummaryTotalTokens usage
  , observedProviderThreads = key : observedProviderThreads total
  , partialProviderThreads = if complete usage then partialProviderThreads total else key : partialProviderThreads total
  }
