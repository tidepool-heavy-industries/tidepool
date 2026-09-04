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
  , unfold
  , AgentSpec
  , AgentRef
  , Response
  , Reply
  , codingAgent
  , readonlyAgent
  , startAgent
  , request
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
  , requestId
  , attemptReply
  , reply
  , pollResponse
  , Await
  , Watch
  , WatchLabel
  , WatchLabelError (..)
  , watchLabel
  , Watches
  , WatchFailure (..)
  , WatchState (..)
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
  , AgentSpec
  , Reply
  , Response
  , codingAgent
  , readonlyAgent
  , request
  , startAgent
  , stopAgent
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
