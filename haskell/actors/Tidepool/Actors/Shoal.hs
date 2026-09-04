{-# LANGUAGE DataKinds #-}

-- | The generic Haskell vocabulary available to an interactive Shoal actor.
--
-- This is a facade, not a prewritten actor program. The model authors its
-- declarations and actions incrementally. Project-specific policies such as
-- DevSwarm belong in separately loaded application modules.
module Tidepool.Actors.Shoal
  ( ActorEffects
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
  , Replies
  , ReplyError (..)
  , ResponseFailure (..)
  , ResponseState (..)
  , requestId
  , attemptReply
  , reply
  , pollResponse
  , Await
  , Watch
  , Watches
  , WatchFailure (..)
  , WatchState (..)
  , awaitResponse
  , watch
  , pollWatch
  , WorktreeSpec
  , fromCurrentRepository
  , fromRef
  , fromWorktree
  , allowDirtySnapshot
  , createWorktree
  , lookupWorktree
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
  , mergeBranchInto
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
import Tidepool.Effects.Core (Actor, Worktree)
import Tidepool.Worktree

-- | Capabilities installed for the interactive root incarnation.
--
-- Naming the row makes the workbench's inferred types readable; it does not
-- prescribe any actor protocol, declarations, state machine, or program.
type ActorEffects = '[Replies, Watches, Actor, Worktree]
