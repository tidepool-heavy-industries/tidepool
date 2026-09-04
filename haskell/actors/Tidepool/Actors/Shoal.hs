{-# LANGUAGE DataKinds #-}
{-# LANGUAGE MonoLocalBinds #-}

-- | The generic Haskell vocabulary available to an interactive Shoal actor.
--
-- This is a facade, not a prewritten actor program. The model authors its
-- declarations and actions incrementally. Project-specific policies such as
-- DevSwarm belong in separately loaded application modules.
module Tidepool.Actors.Shoal
  ( ActorEffects
  , ResearchActorEffects
  , CodingActorEffects
  , ScaffoldActorEffects
  , IntegrationActorEffects
  , CoreEffects
  , ResearchEffects
  , ResearchCoordinatorEffects
  , CodingEffects
  , ScaffoldEffects
  , IntegrationEffects
  , ActorContext
  , ActorContextInfo (..)
  , ActorContextRole (..)
  , ActorNativeTools (..)
  , ActorWorkspaceAccess (..)
  , actorContext
  , AgentLaunch
  , AgentInspection
  , AgentControl
  , BoundWorktree
  , WorktreeRegistry
  , WorktreeAllocation
  , WorktreeIntegration
  , Forks
  , Effects
  , KnownEffect (effectWitness)
  , KnownEffects (knownEffects)
  , Subset
  , CampaignLabel
  , ForkGroupLabel
  , BranchLabel
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
  , withBranchDeadline
  , RolePolicy
  , inspectionPolicy
  , codingPolicy
  , scaffoldPolicy
  , integrationPolicy
  , narrowed
  , ForkRole (..)
  , ForkWorkspaceAccess (..)
  , researching
  , coding
  , scaffolding
  , integrating
  , Unfold
  , child
  , Forked
  , forkedActor
  , forkedResponse
  , forkedLaunch
  , BranchReceipt (..)
  , ForkGroupHandle
  , forkGroupHandle
  , ForkGroupCleanupOutcome (..)
  , cleanupForkGroup
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
  , listAgents
  , AgentForgetOutcome (..)
  , forgetAgent
  , Response
  , Reply
  , codingAgent
  , readonlyAgent
  , startAgent
  , request
  , RequestOptions
  , RequestDeadline
  , requestOptions
  , requestDeadline
  , withRequestGuidance
  , withRequestDeadline
  , requestWith
  , StopOutcome (..)
  , stopAgent
  , RequestId
  , RequestLabel
  , RequestLabelError (..)
  , requestLabel
  , Replies
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
  , watch
  , pollWatch
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
  , withBranchPrefix
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
import Prelude

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
  , RequestDeadline
  , RequestOptions
  , requestDeadline
  , requestOptions
  , requestWith
  , StopOutcome (..)
  , startAgent
  , stopAgent
  , withRequestDeadline
  , withRequestGuidance
  )
import Tidepool.Actors.Role
import Tidepool.Actors.Unfold
import Tidepool.Actors.Worktree hiding (queryWorktrees)
import qualified Tidepool.Actors.Worktree as WorktreeActor
import Tidepool.Effects.Core
  ( ActorContextInfo (..)
  , ActorContextRole (..)
  , ActorNativeTools (..)
  , ActorWorkspaceAccess (..)
  , AgentRosterEntry (..)
  , AgentRosterState (..)
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
   , WorktreeIntegration
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
  , queryBranchPrefix :: Maybe Text
  , queryCreatedAfter :: Maybe Int
  }
  deriving (Show, Eq)

allManagedWorktrees :: WorktreeQuery
allManagedWorktrees = WorktreeQuery Nothing Nothing Nothing

withWorktreePresence :: WorktreePresence -> WorktreeQuery -> WorktreeQuery
withWorktreePresence presence query = query { queryPresence = Just presence }

withBranchPrefix :: Text -> WorktreeQuery -> WorktreeQuery
withBranchPrefix prefix query = query { queryBranchPrefix = Just prefix }

createdAfter :: Int -> WorktreeQuery -> WorktreeQuery
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
  (queryBranchPrefix query)
  (queryCreatedAfter query)
