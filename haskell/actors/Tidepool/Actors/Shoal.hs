{-# LANGUAGE DataKinds #-}

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
  , awaitFork
  , awaitSettledFork
  , unfold
  , AgentSpec
  , AgentRef
  , AgentState (..)
  , AgentObservation (..)
  , agentIdentity
  , agentBoundWorktree
  , observeAgent
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
  , CancelOutcome (..)
  , requestId
  , attemptReply
  , reply
  , pollResponse
  , cancelResponse
  , Await
  , Watch
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
import Tidepool.Effects.Core (Actor, Worktree)
import Tidepool.Worktree

-- | Capabilities installed for the interactive root incarnation.
--
-- Naming the row makes the workbench's inferred types readable; it does not
-- prescribe any actor protocol, declarations, state machine, or program.
type ActorEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation
   , WorktreeIntegration, Actor, Worktree
   ]

type ResearchActorEffects = ResearchEffects
type CodingActorEffects = CodingEffects
type ScaffoldActorEffects = Actor ': Worktree ': ScaffoldEffects
type IntegrationActorEffects = Worktree ': IntegrationEffects
