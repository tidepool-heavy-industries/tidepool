{-# LANGUAGE FlexibleContexts #-}

-- | Cheap control-plane projections. Reading these values never asks a model
-- to summarize its work. Usage is deduplicated by provider thread identity.
module Tidepool.Actors.Observe
  ( SwarmSnapshot (..)
  , UsageTotal (..)
  , snapshot
  , subtree
  , swarmUsage
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Prelude
import Tidepool.Actors.Internal.Agent (listAgents)
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
  } deriving (Show, Eq)

snapshot :: Member AgentInspection effects => Eff effects SwarmSnapshot
snapshot = SwarmSnapshot <$> listAgents

-- | Supervision descendants, including the selected actor. Context inheritance
-- is a separate relationship and does not determine subtree accounting.
subtree :: (Int, Int) -> SwarmSnapshot -> SwarmSnapshot
subtree root (SwarmSnapshot actors) = SwarmSnapshot (filter belongs actors)
  where
    identity actor = (rosterActorId actor, rosterActorIncarnation actor)
    belongs actor = reaches [] (identity actor)
    reaches seen key
      | key == root = True
      | key `elem` seen = False
      | otherwise = case filter ((== key) . identity) actors of
          actor : _ -> case (rosterSupervisorId actor, rosterSupervisorIncarnation actor) of
            (Just parent, Just incarnation) -> reaches (key : seen) (parent, incarnation)
            _ -> False
          _ -> False

swarmUsage :: SwarmSnapshot -> UsageTotal
swarmUsage (SwarmSnapshot actors) = foldl add (UsageTotal 0 0 0 0 0 [] [] []) actors
  where
    unknown actor total = total { unknownActors = (rosterActorId actor, rosterActorIncarnation actor) : unknownActors total }
    add total actor = case rosterUsageSummary actor of
      Nothing -> unknown actor total
      Just usage -> case usageSummaryScope usage of
        UsageTurn _ _ -> unknown actor total
        UsageThread thread
          | thread `elem` observedProviderThreads total -> total
          | otherwise -> total
              { totalCachedInput = totalCachedInput total + usageSummaryCachedInputTokens usage
              , totalUncachedInput = totalUncachedInput total + usageSummaryUncachedInputTokens usage
              , totalOutput = totalOutput total + usageSummaryOutputTokens usage
              , totalReasoningOutput = totalReasoningOutput total + usageSummaryReasoningTokens usage
              , totalTokens = totalTokens total + usageSummaryTotalTokens usage
              , observedProviderThreads = thread : observedProviderThreads total
              , partialProviderThreads = case usageSummaryCompleteness usage of
                  UsageComplete -> partialProviderThreads total
                  UsagePartial -> thread : partialProviderThreads total
              }
