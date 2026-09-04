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

inspectCampaign
  :: Forked result
  -> Eff ActorEffects CampaignSnapshot
inspectCampaign = observeCampaign . forkGroupHandle

cacheFacts
  :: AgentRosterEntry
  -> (Maybe ProviderUsageScope, Maybe CacheBoundaryReason, Int)
cacheFacts entry =
  (rosterUsageScope entry, rosterCacheBoundary entry, rosterEventWatermark entry)

activationFacts :: ActorContextInfo -> (ActivationKind, Int)
activationFacts context =
  (contextActivationKind context, contextEventWatermark context)

currentTree :: Eff CodingActorEffects (Either WorktreeError WorktreeHandle)
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

releaseForkGroup :: Forked result -> Eff ActorEffects ForkGroupCleanupOutcome
releaseForkGroup = cleanupForkGroup . forkGroupHandle

safeHead
  :: WorktreeHandle
  -> Eff CodingActorEffects (Either WorktreeError GitOid)
safeHead = worktreeHead

recentCampaignTrees :: ObservedAt -> ForkGroupHandle -> Eff ActorEffects (Either WorktreeError [WorktreeSummary])
recentCampaignTrees timestamp group =
  queryWorktrees $
    createdAfter timestamp $
      withinForkGroup group $
        withWorktreePresence PresentWorktrees allManagedWorktrees

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
      <$> child (researching @Text textLeaf projectHead ())
      <*> child (coding @Int intLeaf projectHead ())

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
  -> Branch ResearchActorEffects () Text
configuredBranch deadline leaf =
  withBranchDeadline deadline $
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

result :: Int
result = 42
