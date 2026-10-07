{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

module ExomonadPublicAgents where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Prelude
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Actors.Exomonad hiding (result)
import Tidepool.Actors.Observe (actorContext)
import qualified Tidepool.Actors.Exomonad as Exomonad

type ReplyPin = Exomonad.Reply Int
type StopControlPin = Exomonad.AgentStopControlOutcome
type ProfilePin protocol effects = Exomonad.EffectProfile protocol effects
type MessagePin api = Exomonad.Message api
type ExitPin value = Exomonad.ActorExit value
type LifecyclePin = Exomonad.ActorLifecycle
type CancellationPin = Exomonad.Void
type DurationInputPin = Exomonad.Natural
type GitFailurePin = Exomonad.GitFailureReceipt

type MinimalSpec = AgentSpec NoTools '[]

minimalSpec :: MinimalSpec
minimalSpec = defaultSpec

spawnFresh :: Text -> Eff ActorEffects (Either SpawnError AgentRef)
spawnFresh prompt =
  spawnSubagent (FreshCtx prompt) SameDir (defaultSpawnOptions minimalSpec)

spawnFromCheckpoint
  :: ContextCheckpoint
  -> Workspace
  -> Eff ActorEffects (Either SpawnError AgentRef)
spawnFromCheckpoint context workspace =
  spawnSubagent (ForkCtx context) workspace (defaultSpawnOptions minimalSpec)

spawnWithActualSpec :: SpawnOptions NoTools '[] -> Eff ActorEffects (Either SpawnError AgentRef)
spawnWithActualSpec options = spawnSubagent (FreshCtx "inspect the project") SameDir options

submitRaw :: AgentRef -> Int -> Eff ActorEffects (Either RequestError (Request Text))
submitRaw agent input = request @Text agent input defaultRequestOptions

submitConfigured :: AgentRef -> Text -> Eff ActorEffects (Either RequestError (Request Text))
submitConfigured agent input = request @Text agent input
  (defaultRequestOptions
    { requestLabel = Just "review / API"
    , requestGuidance = Just "inspect the typed input before acting"
    , requestDeadline = Just (milliseconds 2500)
    })

submitWithProgress
  :: AgentRef
  -> Text
  -> Eff ActorEffects (Either RequestError (Request Text, Progress Int))
submitWithProgress agent input =
  requestWithProgress @Int @Text agent input defaultRequestOptions

composeSettlements
  :: Request left
  -> Request right
  -> Await (Either ResponseFailure left, Either ResponseFailure right)
composeSettlements left right = (,) <$> settlement left <*> settlement right

awaitOne :: Request value -> Eff ActorEffects (Either AwaitError value)
awaitOne = await . Exomonad.result

watchBoth
  :: Maybe Text
  -> Request left
  -> Request right
  -> Eff ActorEffects (Watch (Either ResponseFailure left, Either ResponseFailure right))
watchBoth label left right = watch label (composeSettlements left right)

stop :: AgentRef -> Eff ActorEffects StopOutcome
stop = stopAgent

inspect :: AgentRef -> Eff ActorEffects AgentObservation
inspect = observeAgent

inspectAll :: Eff ActorEffects [AgentRosterEntry]
inspectAll = listAgentsFull

inspectSelf :: Eff ActorEffects ActorContextInfo
inspectSelf = actorContext

cancel :: Request value -> Eff '[Replies] CancelRequestOutcome
cancel = cancelRequest

releaseRequest :: Request value -> Eff '[Replies] ForgetResponseOutcome
releaseRequest = forgetResponse

releaseWatch :: Watch value -> Eff '[Watches] ForgetWatchOutcome
releaseWatch = forgetWatch

releaseAgent :: AgentRef -> Eff ActorEffects AgentForgetOutcome
releaseAgent = forgetAgent

retainedAgentDependencies :: AgentForgetOutcome -> ([RequestId], [WatchId])
retainedAgentDependencies outcome = case outcome of
  AgentForgetRetained requests watches -> (requests, watches)
  AgentForgotten -> ([], [])
  AgentForgetRunning -> ([], [])
  AgentForgetUnavailable -> ([], [])
  AgentForgetOutputPending _ -> ([], [])

safeHead :: Eff ActorEffects (Either WorktreeError GitOid)
safeHead = boundWorktree >>= either (pure . Left) worktreeHead

currentWorkspaceGrant :: Eff ActorEffects (Either WorktreeError WorkspaceHandle)
currentWorkspaceGrant = currentWorkspace

usageTotals
  :: ActorContextInfo
  -> Maybe (ProviderUsageScope, ProviderUsageCompleteness, Int, Int, Int)
usageTotals context = fmap project (contextUsageSummary context)
  where
    project summary =
      ( usageSummaryScope summary
      , usageSummaryCompleteness summary
      , usageSummaryObservations summary
      , usageSummaryCachedInputTokens summary
      , usageSummaryUncachedInputTokens summary
      )

latestWorkerTurn :: AgentRosterEntry -> Maybe ProviderUsageSummary
latestWorkerTurn = rosterLatestTurnUsage

watchReport :: WatchState (Either ResponseFailure Text) -> WatchState Text
watchReport = fmap (either (const "request failed") id)

-- The unqualified import above intentionally hides this facade function.
result :: Int
result = 42
