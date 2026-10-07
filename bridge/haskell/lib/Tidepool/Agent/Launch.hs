{-# LANGUAGE FlexibleContexts #-}

-- | Workspace selection for independent actor admission.
module Tidepool.Agent.Launch
  ( Workspace (..)
  , WorkspaceHandle
  , WorktreeSeed
  , projectHead
  , currentCheckout
  , atRef
  , existingWorktree
  , currentWorkspace
  , workspaceWire
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Prelude
import Tidepool.Effects.Core
  ( BoundWorktree (..), GitRef, WorktreeId, WorktreeSource (..)
  , WorktreeError, WorkspaceHandle, SpawnWorkspaceWire (..), WorkspaceSeedWire (..)
  )

-- | A directory choice does not change the child's installed tools or source.
data Workspace
  = SameDir
  | ExistingWorkspace WorkspaceHandle
  | ForkWorktree WorktreeSeed
  deriving (Show, Eq)

-- | A committed source selected once by the runtime at fork admission.
data WorktreeSeed = CurrentCheckoutSeed | CommittedSeed WorktreeSource
  deriving (Show, Eq)

projectHead :: WorktreeSeed
projectHead = CommittedSeed SourceCurrentRepository

currentCheckout :: WorktreeSeed
currentCheckout = CurrentCheckoutSeed

atRef :: GitRef -> WorktreeSeed
atRef = CommittedSeed . SourceRef

existingWorktree :: WorktreeId -> WorktreeSeed
existingWorktree = CommittedSeed . SourceWorktree

-- | Issue a run-local grant for the caller's actual registered directory.
currentWorkspace :: Member BoundWorktree effects => Eff effects (Either WorktreeError WorkspaceHandle)
currentWorkspace = send BoundWorkspaceGet

workspaceWire :: Workspace -> SpawnWorkspaceWire
workspaceWire SameDir = SameDirectory
workspaceWire (ExistingWorkspace handle) = ExistingDirectory handle
workspaceWire (ForkWorktree CurrentCheckoutSeed) = ForkDirectory CurrentCheckout
workspaceWire (ForkWorktree (CommittedSeed source)) = ForkDirectory (CommittedSource source)
