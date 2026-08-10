{-# LANGUAGE OverloadedStrings #-}

-- | Managed git worktrees — the authored surface of
-- @plans\/self-iterating-harness\/19-managed-worktrees-events-prd.md@.
--
-- A resident allocates isolated worktrees, looks retained ones back up by
-- durable id, and reads what git actually became.  It does not perform git
-- workflow here: there is no @rebaseOnto@, @merge@, @cherryPick@, conflict
-- resolution, or branch promotion in this module, and their absence is the
-- design rather than a gap.  Coding agents do that work with their native
-- tools; Tidepool observes the result through 'Tidepool.Event'.
--
-- == The vocabulary
--
-- Build a spec, create from it, get a handle:
--
-- @
-- created <- 'createWorktree' ('fromCurrentRepository' "dev-tree\/root")
-- case created of
--   Left ('SourceDirty' summary) -> ...
--   Right tree                   -> ...
-- @
--
-- A dirty source is refused by default.  The escape hatch is spelled at the
-- call site, so a reader of the resident can see that a snapshot was taken:
--
-- @
-- 'createWorktree' ('allowDirtySnapshot' ('fromCurrentRepository' "dev-tree\/root"))
-- @
--
-- == Retention
--
-- Managed worktrees are retained indefinitely in v1.  There is deliberately no
-- @releaseWorktree@ or @deleteWorktree@: losing work is worse than
-- accumulating it, and every tree, branch, snapshot ref, and receipt survives
-- restart under its 'WorktreeId'.  A tree a human removed by hand comes back
-- as 'WorktreeLost' and is never silently recreated.
--
-- == Isolation
--
-- One worktree per agent, every agent isolated.  Binding a second agent to a
-- bound worktree fails explicitly.  A reviewer is isolated like everyone else
-- — give it its own worktree created 'fromWorktree' off the branch it is
-- reviewing.
--
-- == Reading @HEAD@ across a cycle boundary
--
-- 'worktreeHead' is a FRESH git read of a worktree's current @HEAD@ — not the
-- handle's recorded @sourceHead@, and not the event monitor's last-observed
-- baseline.  It is how a resident that spans cycles closes a gap the event
-- system deliberately will not close for it.
--
-- A subscription never replays and lives only for its cycle, so @HEAD@ can move
-- after one cycle unregisters and before the next registers.  A resident
-- reconciles that window itself, in ordinary code, as the FIRST ACTION inside
-- the newly registered handler scope — registration is active before the read,
-- so a movement before registration is found by the reconciliation read while
-- a movement after it is queued for the handler (deduplicate by observed head
-- if both paths see the same movement).  Reading @HEAD@ before registering
-- instead reopens the very window this closes:
--
-- @
-- 'Tidepool.Event.withHandler' ('Tidepool.Event.headChanged' tree) onChange $ do
--   current <- 'worktreeHead' tree
--   when (current \/= checkpointedHead) (reactToMissedMovement current)
--   ...
-- @
--
-- This REINFORCES no-replay rather than working around it.  The journal stays
-- diagnostic instead of quietly becoming a callback-replay mechanism, because
-- the resident — which knows what it already acted on — decides what the gap
-- meant, rather than the runtime guessing on its behalf.
--
-- HOLD: @workspaceOf :: WorktreeHandle -> Workspace@ and the coupled-spawn
-- signature are NOT exported yet.  @Workspace@ is PRD 18's type and the
-- coupling revision is being designed jointly with the agent lane through
-- root; exporting a conversion into a type whose shape is still being settled
-- would freeze the wrong half of a two-sided seam.  The binding enforcement
-- those signatures rest on exists today (one worktree, one agent, explicit
-- refusal) and is tested against a scripted writer.
module Tidepool.Worktree
  ( -- * Specs
    WorktreeSpec
  , fromCurrentRepository
  , fromRef
  , fromWorktree
  , allowDirtySnapshot

    -- * Creation and lookup
  , createWorktree
  , lookupWorktree
  , listWorktrees

    -- * Handles
  , WorktreeHandle
  , WorktreeId
  , BranchName
  , GitRef
  , GitOid
  , worktreeId
  , worktreeBranch
  , worktreeHead

    -- * Receipts and failures
  , WorktreeReceipt (..)
  , WorktreeSummary (..)
  , WorktreeError (..)
  , DirtySummary (..)
  , renderWorktreeError
  , renderWorktreeId
  , renderBranchName
  , renderGitOid
  ) where

import Tidepool.Effects
  ( BranchName
  , DirtySummary (..)
  , GitOid
  , GitRef
  , WorktreeError (..)
  , WorktreeHandle
  , WorktreeId
  , WorktreeReceipt (..)
  , WorktreeSummary (..)
  , WorktreeSpec
  , allowDirtySnapshot
  , createWorktree
  , fromCurrentRepository
  , fromRef
  , fromWorktree
  , listWorktrees
  , lookupWorktree
  , renderBranchName
  , renderGitOid
  , renderWorktreeError
  , renderWorktreeId
  , worktreeBranch
  , worktreeHead
  , worktreeId
  )
