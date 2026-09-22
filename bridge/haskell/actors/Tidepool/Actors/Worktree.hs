{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}

-- | Granular model-facing worktree capabilities.
--
-- Rust delegates every request to the canonical Worktree handler; these
-- constructors only make the intended authority boundary legible to GHC and
-- to the actor using the Shoal facade.
module Tidepool.Actors.Worktree
  ( createWorktree
  , lookupWorktree
  , boundWorktree
  , listWorktrees
  , queryWorktrees
  , worktreeBranch
  , worktreeHead
  , observeSubmission
  , tryMerge
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core
  ( BoundWorktree (..)
  , BranchName
  , GitOid
  , MergeOutcome
  , MergeRequest
  , SubmissionObservation
  , WorktreeAllocation (..)
  , WorktreeError
  , WorktreeHandle
  , WorktreeIntegration (..)
  , WorktreeRegistry (..)
  , WorktreeSpec
  , WorktreeSummary
  )
import qualified Tidepool.Worktree as Worktree

createWorktree
  :: Member WorktreeAllocation effs
  => WorktreeSpec
  -> Eff effs (Either WorktreeError WorktreeHandle)
createWorktree = send . WorktreeAllocationCreate

lookupWorktree
  :: Member WorktreeRegistry effs
  => Worktree.WorktreeId
  -> Eff effs (Either WorktreeError WorktreeHandle)
lookupWorktree = send . WorktreeRegistryLookup

boundWorktree
  :: Member BoundWorktree effs
  => Eff effs (Either WorktreeError WorktreeHandle)
boundWorktree = send BoundWorktreeGet

listWorktrees
  :: Member WorktreeRegistry effs
  => Eff effs (Either WorktreeError [WorktreeSummary])
listWorktrees = send WorktreeRegistryList

queryWorktrees
  :: Member WorktreeRegistry effs
  => Maybe Bool
  -> Maybe Text
  -> Maybe Int
  -> Eff effs (Either WorktreeError [WorktreeSummary])
queryWorktrees present branchPrefix createdAfter =
  send (WorktreeRegistryQuery present branchPrefix createdAfter)

worktreeBranch
  :: Member BoundWorktree effs
  => WorktreeHandle
  -> Eff effs (Either WorktreeError BranchName)
worktreeBranch tree =
  send (BoundWorktreeBranchOf (Worktree.worktreeId tree))

worktreeHead
  :: Member BoundWorktree effs
  => WorktreeHandle
  -> Eff effs (Either WorktreeError GitOid)
worktreeHead tree =
  send (BoundWorktreeHeadOf (Worktree.worktreeId tree))

observeSubmission
  :: Member BoundWorktree effs
  => Worktree.WorktreeId
  -> Eff effs (Either WorktreeError SubmissionObservation)
observeSubmission = send . BoundWorktreeObserveSubmission

tryMerge
  :: Member WorktreeIntegration effs
  => MergeRequest
  -> Eff effs (Either WorktreeError MergeOutcome)
tryMerge = send . WorktreeIntegrationTryMerge
