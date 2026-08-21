{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot #-}

-- | Managed git worktrees.
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
-- signature are NOT exported yet — @Workspace@'s shape is still being settled
-- jointly with the agent lane, and exporting a conversion into it now would
-- freeze the wrong half of a two-sided seam.  The binding enforcement those
-- signatures rest on exists today (one worktree, one agent, explicit refusal)
-- and is tested against a scripted writer.
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
  , WorktreeId (..)
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

import Control.Monad.Freer (send)
import qualified Tidepool.Data.Text as T
import Tidepool.Effects
  ( BranchName (..)
  , DirtyPolicy (..)
  , DirtySummary (..)
  , GitFailureReceipt (..)
  , GitOid (..)
  , GitRef
  , M
  , Worktree (WorktreeBranchOf, WorktreeHeadOf)
  , WorktreeError (..)
  , WorktreeHandle
  , WorktreeId (..)
  , WorktreeReceipt (..)
  , WorktreeSource (..)
  , WorktreeSpec (..)
  , WorktreeSummary (..)
  , createWorktree
  , liftEither
  , listWorktrees
  , lookupWorktree
  , worktreeId
  )
import Tidepool.Prelude hiding (error)

default (Int, Double, Text)

-- The ten definitions below are LIBRARY code, not contract. Each is
-- constructor application, a record update, an argument-adapting send, or an
-- identity unwrap: none is a thin wrapper over one verb, so the effect
-- contract in `tidepool-protocol` cannot describe them without becoming a raw
-- Haskell hatch with extra steps. They live here, where library code belongs,
-- and the Worktree contract carries `import Tidepool.Worktree` as an
-- `extra_imports` row so an eval whose row includes Worktree still sees all
-- fourteen names with nothing authored differently.
--
-- FOUR names are re-exported from the generated "Tidepool.Effects" above
-- rather than defined here, and it is worth knowing WHY before anyone tries to
-- move them:
--
--   * 'createWorktree', 'lookupWorktree', 'listWorktrees' are thin wrappers
--     over one verb, so the contract represents them.
--   * 'worktreeId' is a pure field projection, which the contract represents
--     as one shape — but the reason it HAD to be represented rather than
--     relocated is that "Tidepool.Event"'s @commit@ and @headChanged@ helpers
--     CALL it, and a helper spliced into the generated "Tidepool.Effects" may
--     not reference a name that lives out here. That module cannot import this
--     one — this module imports IT — and defining a name in both places would
--     make it an ambiguous occurrence in any eval.
--
-- The pragma block at the top of this file is the generated
-- "Tidepool.Effects" module's own pragma set, verbatim: these bodies used to
-- be spliced INTO that module, so anything less is a scope this code did not
-- have to compile against before.

-- | Seed a managed worktree from the repository Tidepool is running
-- against. Clean-by-default: a dirty source is REFUSED unless the spec
-- is passed through 'allowDirtySnapshot'.
fromCurrentRepository :: Text -> WorktreeSpec
fromCurrentRepository lbl = WorktreeSpec SourceCurrentRepository lbl RequireClean

-- | Seed from an explicit ref (branch, tag, remote ref, or raw OID).
fromRef :: GitRef -> Text -> WorktreeSpec
fromRef r lbl = WorktreeSpec (SourceRef r) lbl RequireClean

-- | Seed from another managed worktree's current HEAD. This is how a
-- reviewer gets its own isolated tree off the branch it is reviewing.
fromWorktree :: WorktreeHandle -> Text -> WorktreeSpec
fromWorktree h lbl = WorktreeSpec (SourceWorktree (worktreeId h)) lbl RequireClean

-- | Opt IN to snapshotting a dirty source. Spelled at the call site so a
-- reader of the resident can see that a synthetic commit was taken; it
-- never alters the source branch, HEAD, index, or working-tree bytes.
allowDirtySnapshot :: WorktreeSpec -> WorktreeSpec
allowDirtySnapshot s = s { specDirtyPolicy = AllowDirtySnapshot }

-- | The managed branch this worktree is on, read fresh from git.
worktreeBranch :: WorktreeHandle -> M BranchName
worktreeBranch h = send (WorktreeBranchOf (worktreeId h)) >>= liftEither

-- | This worktree's CURRENT @HEAD@, read fresh from git right now.
--
-- Deliberately none of the three things it could be confused with: it
-- is not the handle's recorded @sourceHead@ (the commit the managed
-- branch was rooted at), and it is not the event monitor's
-- last-observed baseline.  The whole purpose is to see what the
-- monitor did NOT.
--
-- It exists for the gap a resident spanning cycles has to close
-- itself.  A subscription never replays, and it lives only for its
-- cycle, so @HEAD@ can move after one cycle unregisters and before the
-- next one registers.  A resident closes that window in ORDINARY
-- AUTHORED CODE: compare @worktreeHead tree@ against the head it
-- checkpointed, act on any difference, and only then register live
-- reactions with 'withHandler'.
--
-- That is a reinforcement of no-replay, not a loophole in it.  The
-- journal stays diagnostic rather than quietly becoming a callback
-- replay mechanism, because the resident — which knows what it already
-- acted on — decides what the gap meant, rather than the runtime
-- guessing on its behalf.
worktreeHead :: WorktreeHandle -> M GitOid
worktreeHead h = send (WorktreeHeadOf (worktreeId h)) >>= liftEither

renderGitOid :: GitOid -> Text
renderGitOid (GitOid t) = t

renderBranchName :: BranchName -> Text
renderBranchName (BranchName t) = t

renderWorktreeId :: WorktreeId -> Text
renderWorktreeId (WorktreeId t) = t

-- | A one-line, operator-readable rendering of a worktree failure.
-- Case-match the constructor when you mean to BRANCH on the failure;
-- this is for receipts and logs.
renderWorktreeError :: WorktreeError -> Text
renderWorktreeError (SourceDirty d) = "source repository is dirty: " <> show (length d.staged) <> " staged, " <> show (length d.unstaged) <> " unstaged, " <> show (length d.untracked) <> " untracked"
renderWorktreeError (NotARepository p) = "not a git repository: " <> p
renderWorktreeError (WorktreeLost i) = "managed worktree " <> renderWorktreeId i <> " is registered but missing on disk"
renderWorktreeError (DirtySubmoduleUnsupported p) = "dirty submodule is unsupported in v1: " <> p
renderWorktreeError (SourceOperationInProgress k) = "source repository has an operation in progress: " <> show k
renderWorktreeError (WorktreeBusy i holder) = "worktree " <> renderWorktreeId i <> " is already bound to agent " <> holder
renderWorktreeError (GitFailure r) = "git " <> T.intercalate " " r.gitArgs <> " failed: " <> T.strip r.gitStderr
renderWorktreeError (WorktreeNotRegistered i) = "no managed worktree registered with id " <> renderWorktreeId i
renderWorktreeError (InvalidRegistryRoot root inside) = "registry root " <> root <> " resolves inside the git working tree at " <> inside <> " — the registry must live outside every source repository"
renderWorktreeError (StorageFailure p d) = "tidepool storage failure at " <> p <> ": " <> d
