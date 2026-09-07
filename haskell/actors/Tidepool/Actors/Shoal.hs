{-# LANGUAGE DataKinds #-}
{-# LANGUAGE MonoLocalBinds #-}

-- | The generic Haskell vocabulary available to an interactive Shoal actor.
--
-- This is a facade, not a prewritten actor program. The model authors its
-- declarations and actions incrementally. Project-specific policies such as
-- DevSwarm belong in separately loaded application modules.
module Tidepool.Actors.Shoal
  ( ActorEffects
  , inspectFull
  , FullInspection
  , ResearchActorEffects
  , CodingActorEffects
  , ScaffoldActorEffects
  , IntegrationActorEffects
  , CoreEffects
  , ResearchEffects
  , ResearchLeafEffects
  , ResearchCoordinatorEffects
  , CodingEffects
  , ScaffoldEffects
  , IntegrationEffects
  , ActorContext
  , ActorContextInfo (..)
  , ActivationKind (..)
  , ActorContextRole (..)
  , ActorNativeTools (..)
  , ActorWorkspaceAccess (..)
  , actorContext
  , AgentLaunch
  , AgentInspection
  , AgentControl
  , Notifications
  , notify
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
  , BranchLabel
  , ActorPath
  , renderActorPath
  , GitBranchPrefix
  , renderGitBranchPrefix
  , actorGitBranchPrefix
  , ForkGroupPath
  , NameError (..)
  , WorktreeSeed
  , projectHead
  , boundHead
  , existingWorktree
  , atRef
  , snapshotDirty
  , campaignLabel
  , forkGroupLabel
  , branchLabel
  , batch
  , subgroup
  , Branch
  , withBranchGuidance
  , withInstructions
  , withLifetime
  , WorkerLifetime (..)
  , withBranchDeadline
  , ForkEffort (..)
  , withEffort
  , withModel
  , WorkerContext
  , inherited
  , selected
  , withContext
  , RolePolicy
  , inspectionPolicy
  , codingPolicy
  , scaffoldPolicy
  , integrationPolicy
  , narrowed
  , ForkRole (..)
  , ForkWorkspaceAccess (..)
  , ForkBudget (..)
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
  , Forked
  , forkedActor
  , forkedResponse
  , forkedLaunch
  , BranchReceipt (..)
  , ForkGroupHandle
  , forkGroupHandle
  , forkGroupGitBranchPrefix
  , ForkObservation (..)
  , observeFork
  , ForkGroupSnapshot (..)
  , observeForkGroup
  , CleanupPlan (..)
  , CleanupActorPlan (..)
  , CleanupActorState (..)
  , CleanupReceipt (..)
  , CleanupStepReceipt (..)
  , planCleanup
  , executeCleanup
  , awaitFork
  , awaitSettledFork
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
  , Reply
  , codingAgent
  , readonlyAgent
  , startAgent
  , request
  , RequestOptions
  , Duration
  , RequestDeadline
  , milliseconds
  , seconds
  , minutes
  , after
  , requestOptions
  , withRequestGuidance
  , withRequestDeadline
  , requestWith
  , requestWithProgress
  , Progress
  , ProgressCursor (..)
  , ProgressState (..)
  , pollProgress
  , StopOutcome (..)
  , stopAgent
  , RequestId
  , RequestLabel
  , RequestLabelError (..)
  , requestLabel
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
  , ReplyState (..)
  , requestId
  , attemptReply
  , reply
  , pollResponse
  , cancelRequest
  , abandonResponse
  , forgetResponse
  , pollReply
  , attemptAcknowledgeCancellation
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
  , awaitResponse
  , awaitValue
  , awaitSettled
  , awaitProgressAfter
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
  , WorktreePresence (..)
  , WorktreeQuery
  , allManagedWorktrees
  , withWorktreePresence
  , withGitBranchPrefix
  , withinForkGroup
  , ObservedAt
  , unixMilliseconds
  , createdAfter
  , queryWorktrees
  , WorktreeHandle
  , WorktreeId (..)
  , BranchName
  , mkBranchName
  , GitRef
  , InProgressKind (..)
  , GitOid
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
  , renderWorktreeError
  , renderWorktreeId
  , renderBranchName
  , renderGitOid
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Numeric.Natural (Natural)
import Prelude
import Tidepool.Inspection (FullInspection, inspectFull)

import Tidepool.Agent.Reply
import Tidepool.Agent.Watch
import Tidepool.Actors.Internal.Agent
  ( AgentRef
  , AgentObservation (..)
  , AgentSpec
  , AgentState (..)
  , Reply
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
  , Duration
  , RequestDeadline
  , RequestOptions
  , milliseconds
  , seconds
  , minutes
  , after
  , requestOptions
  , requestWith
  , requestWithProgress
  , StopOutcome (..)
  , startAgent
  , stopAgent
  , notify
  , pollNotification
  , NotificationReceipt
  , NotificationError (..)
  , NotificationState (..)
  , withRequestDeadline
  , withRequestGuidance
  )
import Tidepool.Actors.Role
import Tidepool.Actors.Observe
import Tidepool.Actors.Unfold
import Tidepool.Actors.Worktree hiding (queryWorktrees)
import qualified Tidepool.Actors.Worktree as WorktreeActor
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
  , actorContext
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
  )

-- | Capabilities installed for the interactive root incarnation.
--
-- Naming the row makes the workbench's inferred types readable; it does not
-- prescribe any actor protocol, declarations, state machine, or program.
type ActorEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation
   , WorktreeIntegration, Notifications
   ]

type ResearchActorEffects = ResearchEffects
type CodingActorEffects = CodingEffects
type ScaffoldActorEffects = ScaffoldEffects
type IntegrationActorEffects = IntegrationEffects

data WorktreePresence
  = PresentWorktrees
  | MissingWorktrees
  deriving (Show, Eq)

data WorktreeQuery = WorktreeQuery
  { queryPresence :: Maybe WorktreePresence
  , queryBranchPrefix :: Maybe GitBranchPrefix
  , queryCreatedAfter :: Maybe ObservedAt
  }
  deriving (Show, Eq)

allManagedWorktrees :: WorktreeQuery
allManagedWorktrees = WorktreeQuery Nothing Nothing Nothing

withWorktreePresence :: WorktreePresence -> WorktreeQuery -> WorktreeQuery
withWorktreePresence presence query = query { queryPresence = Just presence }

withGitBranchPrefix :: GitBranchPrefix -> WorktreeQuery -> WorktreeQuery
withGitBranchPrefix prefix query = query { queryBranchPrefix = Just prefix }

withinForkGroup :: ForkGroupHandle -> WorktreeQuery -> WorktreeQuery
withinForkGroup group = withGitBranchPrefix (forkGroupGitBranchPrefix group)

newtype ObservedAt = ObservedAt Int
  deriving (Show, Eq, Ord)

unixMilliseconds :: Natural -> ObservedAt
unixMilliseconds value
  | value > fromIntegral (maxBound :: Int) = error "timestamp exceeds the runtime integer range"
  | otherwise = ObservedAt (fromIntegral value)

createdAfter :: ObservedAt -> WorktreeQuery -> WorktreeQuery
createdAfter timestamp query = query { queryCreatedAfter = Just timestamp }

-- | Ask the canonical durable registry to filter before returning rows.
queryWorktrees
  :: Member WorktreeRegistry effs
  => WorktreeQuery
  -> Eff effs (Either WorktreeError [WorktreeSummary])
queryWorktrees query = WorktreeActor.queryWorktrees
  (case queryPresence query of
    Nothing -> Nothing
    Just PresentWorktrees -> Just True
    Just MissingWorktrees -> Just False)
  (fmap renderGitBranchPrefix (queryBranchPrefix query))
  (case queryCreatedAfter query of
    Nothing -> Nothing
    Just (ObservedAt value) -> Just value)
