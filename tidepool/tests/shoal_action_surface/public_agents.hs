{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

module ShoalPublicAgents where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Prelude
import Tidepool.Actors.Shoal
import Tidepool.Actors.Observe (actorContext)

startCoding :: WorktreeHandle -> Eff ActorEffects AgentRef
startCoding = startAgent . codingAgent

startReview :: Text -> Eff ActorEffects AgentRef
startReview = startAgent . readonlyAgent

submit
  :: AgentRef
  -> Label
  -> input
  -> Eff ActorEffects (Response result)
submit actor name value = request actor (assignment name value)

submitProgress
  :: AgentRef
  -> Assignment input
  -> Eff ActorEffects (Response result, Progress progress)
submitProgress = requestWithProgress

submitWithOnlyReplies
  :: AgentRef
  -> Label
  -> input
  -> Eff '[Replies] (Response result)
submitWithOnlyReplies actor name value = request actor (assignment name value)

submitConfigured
  :: AgentRef
  -> Label
  -> Duration
  -> input
  -> Eff ActorEffects (Response result)
submitConfigured actor name timeout value =
  requestWith actor ((assignment name value)
    { guidance = Just "inspect the typed input before acting"
    , deadline = Just timeout
    })

compose
  :: Response left
  -> Response right
  -> Await (ResponseResult left, ResponseResult right)
compose left right = (,) <$> awaitResponse left <*> awaitResponse right

watchBoth
  :: WatchLabel
  -> Response left
  -> Response right
  -> Eff ActorEffects (Watch (ResponseResult left, ResponseResult right))
watchBoth label left right = watch label (compose left right)

stop :: AgentRef -> Eff ActorEffects StopOutcome
stop = stopAgent

inspect :: AgentRef -> Eff ActorEffects AgentObservation
inspect = observeAgent

inspectAll :: Eff ActorEffects [AgentRosterEntry]
inspectAll = listAgents

inspectSelf :: Eff ActorEffects ActorContextInfo
inspectSelf = actorContext

inspectResponseActor :: Response result -> Eff ActorEffects AgentObservation
inspectResponseActor = observeAgent . responseActor

inspectForkGroup
  :: Response result
  -> Eff '[AgentInspection] (Maybe ForkGroupSnapshot)
inspectForkGroup response = case forkGroupHandle response of
  Nothing -> pure Nothing
  Just group -> observeForkGroup group

cacheFacts
  :: AgentRosterEntry
  -> (Maybe ProviderUsageObservation, Maybe CacheBoundaryReason, Int)
cacheFacts entry =
  (rosterFirstUsage entry, rosterCacheBoundary entry, rosterEventWatermark entry)

workbenchPosture :: AgentRosterEntry -> AgentWorkbenchPosture
workbenchPosture = rosterWorkbenchPosture

activationFacts :: ActorContextInfo -> (ActivationKind, Int)
activationFacts context =
  (contextActivationKind context, contextEventWatermark context)

currentTree :: Eff CodingEffects (Either WorktreeError WorktreeHandle)
currentTree = boundWorktree

cancel :: Response result -> Eff '[Replies] CancelRequestOutcome
cancel = cancelRequest

releaseResponse :: Response result -> Eff '[Replies] ForgetResponseOutcome
releaseResponse = forgetResponse

releaseWatch :: Watch result -> Eff '[Watches] ForgetWatchOutcome
releaseWatch = forgetWatch

releaseAgent :: AgentRef -> Eff ActorEffects AgentForgetOutcome
releaseAgent = forgetAgent

retainedAgentDependencies :: AgentForgetOutcome -> ([RequestId], [WatchId])
retainedAgentDependencies outcome =
  case outcome of
    AgentForgetRetained requests watches -> (requests, watches)
    AgentForgotten -> ([], [])
    AgentForgetRunning -> ([], [])
    AgentForgetUnavailable -> ([], [])

inspectCleanup :: Response result -> Eff '[AgentInspection] (Maybe CleanupPlan)
inspectCleanup response = case forkGroupHandle response of
  Nothing -> pure Nothing
  Just group -> Just <$> planCleanup group

runCleanup :: CleanupPlan -> Eff '[AgentControl] CleanupReceipt
runCleanup = executeCleanup

cleanupBlocked :: CleanupPlan -> Bool
cleanupBlocked plan =
  not (null (cleanupPlanPendingResponses plan))
    || not (null (cleanupPlanPendingWatches plan))
    || maybe False (const True) (cleanupPlanRefusal plan)

safeHead
  :: WorktreeHandle
  -> Eff CodingEffects (Either WorktreeError GitOid)
safeHead = worktreeHead

launchFacts
  :: Response result
  -> (Int, Int, ForkRole, ForkWorkspaceAccess, WorktreeReceipt)
launchFacts worker = case responseLaunch worker of
  Nothing -> error "response was not created by child"
  Just receipt ->
    ( launchedActorId receipt
    , launchedActorIncarnation receipt
    , launchedRole receipt
    , launchedWorkspaceAccess receipt
    , launchedWorktree receipt
    )

heterogeneousUnfold
  :: ForkGroupPath
  -> Label
  -> Label
  -> Eff ActorEffects (Response Text, Response Int)
heterogeneousUnfold group textLeaf intLeaf =
  unfold group $
    (,)
      <$> child (withEffort High (researching @Text projectHead (assignment textLeaf ())))
      <*> child (withEffort Low (coding @Int projectHead (assignment intLeaf ())))

progressiveUnfold
  :: ForkGroupPath
  -> Label
  -> Eff ActorEffects (Response Text, Progress Int)
progressiveUnfold group leaf =
  unfold group $
    childWithProgress @Int (researching @Text projectHead (assignment leaf ()))

homogeneousUnfold
  :: ForkGroupPath
  -> [Label]
  -> Eff ActorEffects [Response Text]
homogeneousUnfold group leaves =
  unfold group $
    traverse
      (\leaf -> child (researching @Text projectHead (assignment leaf ())))
      leaves

recoverableUnfold
  :: ForkGroupPath
  -> Label
  -> Eff ActorEffects (Either UnfoldError (Response Text))
recoverableUnfold group leaf =
  attemptUnfold group $
    child (researching @Text projectHead (assignment leaf ()))

configuredBranch
  :: Duration
  -> Label
  -> Branch ResearchEffects () Text
configuredBranch timeout leaf =
  withEffort Medium $
    researching @Text projectHead ((assignment leaf ())
      { guidance = Just "inspect only", deadline = Just timeout })

tenMinuteDeadline :: Duration
tenMinuteDeadline = minutes 10

tenMinuteDeadlineInSeconds :: Duration
tenMinuteDeadlineInSeconds = seconds 600

subSecondRequest
  :: AgentRef
  -> Label
  -> Eff ActorEffects (Response Text)
subSecondRequest actor label =
  requestWith actor ((assignment label ()) { deadline = Just (milliseconds 25) })

type TinyResearchEffects = '[Replies, ActorContext]

narrowResearch
  :: ForkGroupPath
  -> Label
  -> Eff ActorEffects (Response Text)
narrowResearch group leaf =
  unfold group $
    child $
      narrowed
        (knownEffects @TinyResearchEffects)
        (inspectionPolicy projectHead)
        (assignment leaf ())

usageTotals :: ActorContextInfo -> Maybe (ProviderUsageScope, ProviderUsageCompleteness, Int, Int, Int)
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

watchReport :: WatchState (ResponseResult Text) -> WatchState Text
watchReport = fmap responseValue

result :: Int
result = 42
