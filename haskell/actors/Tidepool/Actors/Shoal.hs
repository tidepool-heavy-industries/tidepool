{-# LANGUAGE DataKinds #-}
{-# LANGUAGE MonoLocalBinds #-}

-- | The generic Haskell vocabulary available to an interactive Shoal actor.
--
-- This is a facade, not a prewritten actor program. The model authors its
-- declarations and actions incrementally. Project-specific policies such as
-- DevSwarm belong in separately loaded application modules.
module Tidepool.Actors.Shoal
  ( ActorEffects
  , (:-), State, Call, NoReply, Event
  , Shape, Definition, Client, Self, Private
  , ActorState, Handler, ActorSpec, ActorHandle
  , Send, Request, EventHandler, EventSource
  , get, gets, put, modify'
  , definition, client, start, finish, progress, settlement, lifecycle, self, sender
  , ActorInputOrigin (..)
  , LocalEffects, Forwarding, forwardResult, forwardingExit
  , inspectFull
  , FullInspection
  , CoreEffects
  , ResearchEffects
  , ResearchLeafEffects
  , CodingEffects
  , IntegrationEffects
  , ActorContext
  , ActorContextInfo (..)
  , ActivationKind (..)
  , ActorContextRole (..)
  , ActorNativeTools (..)
  , ActorWorkspaceAccess (..)
  , AgentLaunch
  , AgentInspection
  , AgentControl
  , Notifications
  , Actor
  , sendMessage
  , pollNotification
  , NotificationReceipt
  , NotificationError (..)
  , NotificationState (..)
  , BoundWorktree
  , WorktreeRegistry
  , WorktreeAllocation
  , WorktreeIntegration
  , Forks
  , Effects
  , KnownEffect
  , KnownEffects (knownEffects)
  , Subset
  , CampaignLabel
  , ForkGroupLabel
  , Label
  , NameError (..)
  , Assignment (..)
  , SettlementReporting (..)
  , assignment
  , labelFromText
  , ActorPath
  , GitBranchPrefix
  , actorGitBranchPrefix
  , ForkGroupPath
  , WorktreeSeed
  , projectHead
  , boundHead
  , existingWorktree
  , atRef
  , snapshotDirty
  , campaignLabel
  , forkGroupLabel
  , batch
  , subgroup
  , Branch
  , withInstructions
  , withLifetime
  , WorkerLifetime (..)
  , ForkEffort (..)
  , Model (..)
  , withEffort
  , withModel
  , WorkerContext
  , inherited
  , selected
  , withContext
  , RolePolicy
  , inspectionPolicy
  , codingPolicy
  , narrowed
  , ForkRole (..)
  , ForkWorkspaceAccess (..)
  , ForkBudget (..)
  , ForkAllowance (..)
  , WorkerLaunchPreview (..)
  , ForkContext (..)
  , BranchPreview (..)
  , DelegationAuthority (..)
  , withForkBudget
  , previewBranch
  , researching
  , researchingLeaf
  , coding
  , scaffolding
  , integrating
  , Unfold
  , child
  , childWithProgress
  , BranchReceipt (..)
  , ForkGroupHandle
  , forkGroupHandle
  , forkGroupGitBranchPrefix
  , ForkGroupSnapshot (..)
  , observeForkGroup
  , CleanupPlan (..)
  , CleanupActorPlan (..)
  , CleanupActorState (..)
  , CleanupReceipt (..)
  , CleanupStepReceipt (..)
  , planCleanup
  , executeCleanup
  , responseActor
  , responseLaunch
  , UnfoldError (..)
  , attemptUnfold
  , unfold
  , AgentSpec
  , AgentRef
  , AgentState (..)
  , AgentObservation (..)
  , agentIdentity
  , agentBoundWorktree
  , observeAgent
  , AgentRosterEntry (..)
  , AgentRosterState (..)
  , AgentDisposition (..)
  , ProviderHealth (..)
  , ProviderFailureKind (..)
  , AgentWorkbenchPosture (..)
  , AgentWorkbenchTransfer (..)
  , ProviderUsageObservation (..)
  , ProviderUsageScope (..)
  , ProviderUsageCompleteness (..)
  , ProviderUsageSummary (..)
  , CacheBoundaryReason (..)
  , listAgents
  , SwarmSnapshot (..)
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
  , AgentForgetOutcome (..)
  , forgetAgent
  , Response
  , codingAgent
  , readonlyAgent
  , startAgent
  , request
  , Duration
  , milliseconds
  , seconds
  , minutes
  , requestWith
  , requestWithProgress
  , requestWithProgressInto
  , Progress
  , ProgressCursor (..)
  , ProgressState (..)
  , pollProgress
  , StopOutcome (..)
  , stopAgent
  , RequestId
  , Replies
  , RequestUpdate
  , RequestUpdateState (..)
  , updateRequest
  , pollRequestUpdate
  , ReplyError (..)
  , ResponseFailure (..)
  , ResponseResult (..)
  , ExecutionReceipt (..)
  , WorktreeEvidence (..)
  , ResponseState (..)
  , CancellationReason (..)
  , CancelRequestOutcome (..)
  , AbandonOutcome (..)
  , ForgetResponseOutcome (..)
  , requestId
  , pollResponse
  , cancelRequest
  , abandonResponse
  , forgetResponse
  , acknowledgeCancellation
  , Await
  , Watch
  , WatchId
  , WatchLabel
  , WatchLabelError (..)
  , watchLabel
  , Watches
  , WatchFailure (..)
  , WatchState (..)
  , Settlement (..)
  , settledValue
  , awaitResponse
  , awaitValue
  , awaitSettled
  , awaitProgressAfter
  , awaitAnyProgress
  , watch
  , pollWatch
  , Route
  , RouteState (..)
  , route
  , pollRoute
  , listRoutes
  , forgetRoute
  , ForgetWatchOutcome (..)
  , forgetWatch
  , WorktreeSpec
  , fromCurrentRepository
  , fromRef
  , fromWorktree
  , allowDirtySnapshot
  , createWorktree
  , lookupWorktree
  , boundWorktree
  , listWorktrees
  , WorktreeHandle
  , WorktreeId (..)
  , BranchName
  , mkBranchName
  , GitRef
  , InProgressKind (..)
  , GitOid (..)
  , worktreeId
  , worktreeBranch
  , worktreeHead
  , observeSubmission
  , MergeOutcome (..)
  , MergeRequest (..)
  , tryMerge
  , WorktreeReceipt (..)
  , WorktreeSummary (..)
  , WorktreeError (..)
  , DirtySummary (..)
  , HeadState (..)
  , WorkingState (..)
  , SubmissionObservation (..)
  ) where

import Tidepool.Inspection (FullInspection, inspectFull)

import Tidepool.Agent.Reply
import Tidepool.Actor.Record
  ( (:-), State, Call, NoReply, Event
  , Shape, Definition, Client, Self, Private
  , ActorState, Handler, ActorSpec, ActorHandle
  , Send, Request, EventHandler, EventSource
  , get, gets, put, modify'
  , definition, client, start, finish, progress, settlement, lifecycle, self, sender
  , ActorInputOrigin (..)
  , LocalEffects, Forwarding, forwardResult, forwardingExit
  )
import Tidepool.Agent.Watch
import Tidepool.Actors.Internal.Agent
  ( AgentRef
  , AgentObservation (..)
  , AgentSpec
  , AgentState (..)
  , Response
  , codingAgent
  , agentBoundWorktree
  , agentIdentity
  , observeAgent
  , listAgents
  , AgentForgetOutcome (..)
  , forgetAgent
  , readonlyAgent
  , request
  , Assignment (..)
  , Label
  , SettlementReporting (..)
  , assignment
  , labelFromText
  , Duration
  , milliseconds
  , seconds
  , minutes
  , requestWith
  , requestWithProgress
  , requestWithProgressInto
  , StopOutcome (..)
  , startAgent
  , stopAgent
  , sendMessage
  , pollNotification
  , NotificationReceipt
  , NotificationError (..)
  , NotificationState (..)
  )
import Tidepool.Actors.Role
import Tidepool.Actors.Observe
import Tidepool.Actors.Unfold
import Tidepool.Actors.Worktree
import Tidepool.Effects.Core
  ( ActorContextInfo (..)
  , ActivationKind (..)
  , ActorContextRole (..)
  , ActorNativeTools (..)
  , ActorWorkspaceAccess (..)
  , AgentRosterEntry (..)
  , AgentRosterState (..)
  , AgentDisposition (..)
  , ProviderHealth (..)
  , ProviderFailureKind (..)
  , AgentWorkbenchPosture (..)
  , AgentWorkbenchTransfer (..)
  , ProviderUsageObservation (..)
  , ProviderUsageScope (..)
  , ProviderUsageCompleteness (..)
  , ProviderUsageSummary (..)
  , CacheBoundaryReason (..)
  , GitOid (..)
  )
import Tidepool.Worktree hiding
  ( boundWorktree
  , createWorktree
  , listWorktrees
  , lookupWorktree
  , observeSubmission
  , tryMerge
  , worktreeBranch
  , worktreeHead
  , GitOid
  )
