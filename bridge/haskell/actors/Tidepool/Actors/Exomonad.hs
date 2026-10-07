{-# LANGUAGE DataKinds #-}
{-# LANGUAGE MonoLocalBinds #-}

-- | The generic Haskell vocabulary available to an interactive Exomonad actor.
--
-- This is a facade, not a prewritten actor program. The model authors its
-- declarations and actions incrementally. Project-specific policies such as
-- DevSwarm belong in separately loaded application modules.
module Tidepool.Actors.Exomonad
  ( ActorEffects
  , Eff, Member
  , Void, Natural
  , EffectProfile, ActorExit, ActorLifecycle
  , (:-), State, Call, NoReply, Event
  , Shape, Definition, Client, Self, Private
  , ActorState, Handler, ActorSpec, ActorHandle, Message
  , Send, EventHandler, EventSource
  , get, gets, put, modify'
  , definition, client, start, finish, progress, lifecycle, self, sender
  , ActorInputOrigin (..)
  , LocalEffects, Forwarding, forwardResult, forwardingExit
  , inspectFull
  , FullInspection
  , ActorContext
  , ActorContextInfo (..)
  , ActivationKind (..)
  , AgentLaunch
  , AgentInspection
  , AgentControl
  , Notifications
  , Actor
  , sendMessage
  , parentAgent
  , pollNotification
  , NotificationReceipt
  , NotificationError (..)
  , NotificationState (..)
  , BoundWorktree
  , WorktreeRegistry
  , WorktreeAllocation
  , WorktreeIntegration
  , Journal
  , record
  , trace
  , Reflect
  , reflect
  , ConversationTurn (..)
  , ConversationRole (..)
  , TurnItem (..)
  , ReflectError (..)
  , Effects
  , KnownEffect
  , KnownEffects (knownEffects)
  , Subset
  , SettlementReporting (..)
  , WorktreeSeed
  , projectHead
  , currentCheckout
  , existingWorktree
  , atRef
  , WorkerLifetime (..)
  , ForkEffort (..)
  , Model (..)
  , ContextCheckpoint
  , checkpoint
  , releaseCheckpoint
  , CheckpointRefusal (..)
  , AgentStopControlOutcome (..)
  , responseActor
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
  , listAgentsFull
  , AgentSummary (..)
  , agentSummary
  , findAgentsByLabel
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
  , request
  , Duration
  , milliseconds
  , seconds
  , minutes
  , requestWithProgress
  , requestWithProgressInto
  , Progress
  , ProgressCursor (..)
  , ProgressState (..)
  , pollProgress
  , StopOutcome (..)
  , stopAgent
  , retainAgent
  , AgentRetentionError (..)
  , RequestId
  , Reply
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
  , retainRequest
  , abandonResponse
  , forgetResponse
  , acknowledgeCancellation
  , Await
  , Watch
  , WatchId
  , Watches
  , WatchState (..)
  , after
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
  , renderGitOid
  , DiscardIntent (..)
  , withDiscardIntent
  , GitFailureReceipt
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
  , Request
  , RequestOptions (..)
  , defaultRequestOptions
  , RequestError (..)
  , SpawnContext (..)
  , Workspace (..)
  , WorkspaceHandle
  , currentWorkspace
  , SpawnLimits (..)
  , DescendantDepth
  , ActiveDescendants
  , SpawnLimitError (..)
  , descendantDepth
  , activeDescendants
  , SpawnOptions (..)
  , defaultSpawnOptions
  , SpawnError (..)
  , SpawnRetainedResources (..)
  , SpawnCleanup (..)
  , spawnSubagent
  , SpecReplacementError (..)
  , replaceSpec
  , ResourceScopes
  , Scope
  , ScopeFailure (..)
  , CleanupError (..)
  , ScopeOutcome (..)
  , withScope
  , observed
  , AwaitError (..)
  , response
  , settledResponse
  , result
  , settlement
  , eitherOf
  , await
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Void (Void)
import Numeric.Natural (Natural)
import Tidepool.Actor (EffectProfile, ActorExit, ActorLifecycle)
import Tidepool.Inspection (FullInspection, inspectFull)

import Tidepool.Agent.Reply
import Tidepool.Actor.Record
  ( (:-), State, Call, NoReply, Event
  , Shape, Definition, Client, Self, Private
  , ActorState, Handler, ActorSpec, ActorHandle, Message
  , Send, EventHandler, EventSource
  , get, gets, put, modify'
  , definition, client, start, finish, progress, lifecycle, self, sender
  , ActorInputOrigin (..)
  , LocalEffects, Forwarding, forwardResult, forwardingExit
  )
import Tidepool.Agent.Watch
import Tidepool.Scope
import Tidepool.Actors.Internal.Agent
import Tidepool.Actors.Role
import Tidepool.Actors.Observe
import Tidepool.Actors.Spawn
import Tidepool.Actors.Worktree
import Tidepool.Effects.Core
  ( ResourceScopes
  , WorkerLifetime (..)
  , ForkEffort (..)
  , Model (..)
  , CheckpointRefusal (..)
  , ActorContextInfo (..)
  , ActivationKind (..)
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
  , GitFailureReceipt
  , AgentStopControlOutcome (..)
  , Reflect
  , reflect
  , ConversationTurn (..)
  , ConversationRole (..)
  , TurnItem (..)
  , ReflectError (..)
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
import Tidepool.Journal (record, trace)
