{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

module ShoalPublicAgents where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Prelude
import Tidepool.Actors.Shoal

startCoding :: WorktreeHandle -> Eff ActorEffects AgentRef
startCoding = startAgent . codingAgent

startReview :: Text -> Eff ActorEffects AgentRef
startReview = startAgent . readonlyAgent

submit
  :: AgentRef
  -> RequestLabel
  -> input
  -> Eff ActorEffects (Response result)
submit = request

submitProgress
  :: AgentRef
  -> RequestOptions input
  -> Eff ActorEffects (Response result, Progress progress)
submitProgress = requestWithProgress

submitWithOnlyReplies
  :: AgentRef
  -> RequestLabel
  -> input
  -> Eff '[Replies] (Response result)
submitWithOnlyReplies = request

submitConfigured
  :: AgentRef
  -> RequestLabel
  -> RequestDeadline
  -> input
  -> Eff ActorEffects (Response result)
submitConfigured actor label deadline input =
  requestWith actor $
    withRequestDeadline deadline $
      withRequestGuidance "inspect the typed input before acting" $
        requestOptions label input

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

inspectFork
  :: Forked result
  -> Eff ActorEffects (ForkObservation result)
inspectFork = observeFork

inspectForkGroup
  :: Forked result
  -> Eff '[AgentInspection] (Maybe ForkGroupSnapshot)
inspectForkGroup = observeForkGroup . forkGroupHandle

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

inspectCleanup :: Forked result -> Eff '[AgentInspection] CleanupPlan
inspectCleanup = planCleanup . forkGroupHandle

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
  :: Forked result
  -> (Int, Int, ForkRole, ForkWorkspaceAccess, WorktreeReceipt)
launchFacts worker =
  let receipt = forkedLaunch worker
  in ( launchedActorId receipt
     , launchedActorIncarnation receipt
     , launchedRole receipt
     , launchedWorkspaceAccess receipt
     , launchedWorktree receipt
     )

heterogeneousUnfold
  :: ForkGroupPath
  -> BranchLabel
  -> BranchLabel
  -> Eff ActorEffects (Forked Text, Forked Int)
heterogeneousUnfold group textLeaf intLeaf =
  unfold group $
    (,)
      <$> child (withEffort High (researching @Text textLeaf projectHead ()))
      <*> child (withEffort Low (coding @Int intLeaf projectHead ()))

progressiveUnfold
  :: ForkGroupPath
  -> BranchLabel
  -> Eff ActorEffects (Forked Text, Progress Int)
progressiveUnfold group leaf =
  unfold group $
    childWithProgress @Int (researching @Text leaf projectHead ())

homogeneousUnfold
  :: ForkGroupPath
  -> [BranchLabel]
  -> Eff ActorEffects [Forked Text]
homogeneousUnfold group leaves =
  unfold group $
    traverse
      (\leaf -> child (researching @Text leaf projectHead ()))
      leaves

recoverableUnfold
  :: ForkGroupPath
  -> BranchLabel
  -> Eff ActorEffects (Either UnfoldError (Forked Text))
recoverableUnfold group leaf =
  attemptUnfold group $
    child (researching @Text leaf projectHead ())

configuredBranch
  :: RequestDeadline
  -> BranchLabel
  -> Branch ResearchEffects () Text
configuredBranch deadline leaf =
  withEffort Medium $ withBranchDeadline deadline $
    withBranchGuidance "inspect only" $
      researching @Text leaf projectHead ()

tenMinuteDeadline :: RequestDeadline
tenMinuteDeadline = after (minutes 10)

tenMinuteDeadlineInSeconds :: RequestDeadline
tenMinuteDeadlineInSeconds = after (seconds 600)

subSecondRequest
  :: AgentRef
  -> RequestLabel
  -> Eff ActorEffects (Response Text)
subSecondRequest actor label =
  requestWith actor $
    withRequestDeadline (after (milliseconds 25)) $
      requestOptions label ()

type TinyResearchEffects = '[Replies, ActorContext]

narrowResearch
  :: ForkGroupPath
  -> BranchLabel
  -> Eff ActorEffects (Forked Text)
narrowResearch group leaf =
  unfold group $
    child $
      narrowed
        (knownEffects @TinyResearchEffects)
        (inspectionPolicy projectHead)
        leaf
        ()

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
