{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Coalgebra-side decomposition for the recursive-development-tree dogfood.
--
-- This module owns scaffold execution, split journaling, policy middleware
-- slots, child worktree allocation, and the budget support that seeds each
-- child.  It depends on the shared worker seam and resume's retained-worktree
-- typestates, while remaining independent of the fold algebra.
module Unfold
  ( decompose
  , emitSplit
  , splitWork
  , refusalWork
  , failureWork
  , requiredCycles
  , childAllowance
  , cycleRefusal
  , depthRefusal
  , layerGate
  , allocateChildren
  , retainedChild
  ) where

import qualified Data.Text as T
import DevTreeJournal
  ( JournalEvent (..)
  , JournalKey (..)
  , recordEvent
  )
import HarnessTypes
import Micro (NodeSeed (..))
import Prompts (scaffoldPrompt)
import Resume
  ( NodeWork (..)
  , retainWorktree
  , retainedHandle
  )
import Tidepool.Agent.Spawn (renderSpawnError)
import Tidepool.Effects
  ( WorktreeHandle
  , say
  )
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import qualified Tidepool.Swarm as Swarm
import Tidepool.Worktree
import Workers
  ( SnapshotResult (..)
  , branchOf
  , runWorker
  , snapshotFailureFor
  )

-- | Unfold one node.  The BARE split — every policy that could refuse it is
-- middleware wrapped around it at the 'loop' call site, so what is left here
-- is only what splitting means.
--
-- Order is load-bearing:
--
-- 1. The scaffold worker runs for a node that HAS children — v1's
--    @spawnWorker@, unmoved.  It is why children seed from a parent HEAD that
--    is already final.  A LEAF spawns nothing here: for a leaf, "how to
--    combine nothing" IS "implement it", so its worker is the algebra's.
-- 2. The split is journaled.  Decomposition is cognition, so it is recorded
--    rather than re-derived; a resumed run replays it instead of re-asking.
-- 3. Child worktrees are allocated from the scaffold HEAD.  A worktree that
--    cannot be created is not a split failure — that child is dropped and the
--    denial rides in 'workDenied' for the algebra to fold as an escalation.
decompose :: NodeSeed -> Harness (Swarm.PlanF NodeWork NodeSeed)
decompose seed
  | null kids = pure (Swarm.PlanF (splitWork seed Nothing [] []) [])
  | otherwise = case seed.seedAdopted of
      -- A resumed run already found this node's scaffold commit sitting in its
      -- retained worktree and VERIFIED it (checks + boundary, at that sha).
      -- Re-running the scaffold worker over it would be exactly the blind redo
      -- the journal exists to prevent.
      Just adopted -> emitSplit seed Nothing adopted
      -- An interior node with an EMPTY task has nothing to scaffold: the plan
      -- is fully authored, its children seed straight from the parent's
      -- current HEAD, and NO worker runs at all.  This is the structural form
      -- of a "make no changes" brief — which two live runs showed a scaffold
      -- worker cannot be trusted to follow (it invented README.md, then
      -- FIELD_GUIDE.md, tripping its own boundary each time).
      Nothing | T.null (T.strip (nodeTask p)) ->
        worktreeHead seed.seedTree >>= emitSplit seed Nothing
      Nothing ->
        runWorker seed.seedTree name (scaffoldPrompt p kids) >>= \case
          Left err ->
            pure
              ( Swarm.PlanF
                  (refusalWork seed (Failure SpawnDenied [fmt|{name} scaffold: {renderSpawnError err}|] []))
                  []
              )
          Right (scaffold, snapshot) ->
            if snapshot.snapshotSucceeded
              then do
                scaffoldHead <- worktreeHead seed.seedTree
                emitSplit seed (Just scaffold) scaffoldHead
              else
                pure
                  ( Swarm.PlanF
                      (failureWork seed (snapshotFailureFor name snapshot))
                      []
                  )
  where
    p = seed.seedPlan
    name = nodeName p
    kids = childPlans p

-- | Journal the split and allocate the children from the scaffold head.
--
-- The split is appended TWICE under the same @(kind, key)@, and the fold keeps
-- the later one (max seq).  That is not redundancy: the two appends close two
-- different crash windows.  The first records the DECISION, so a crash during
-- child allocation replays the plan instead of re-running the scaffold worker.
-- The second adds the child worktrees, which is the only durable record of
-- WHICH retained tree belongs to which child — without it a resumed run would
-- create a second worktree beside a child's orphaned commits and redo its work
-- blind.  Append-only, last-one-wins, no rewrite.
emitSplit :: NodeSeed -> Maybe WorkerResult -> GitOid -> Harness (Swarm.PlanF NodeWork NodeSeed)
emitSplit seed scaffold scaffoldHead = do
  recordEvent (SplitEvent (JournalKey branch) p scaffoldHeadText Nothing)
  (childSeeds, denied) <- allocateChildren seed kids (freshChild seed)
  recordEvent (SplitEvent (JournalKey branch) p scaffoldHeadText (Just (map childTreeEntry childSeeds)))
  pure (Swarm.PlanF (splitWork seed scaffold childSeeds denied) childSeeds)
  where
    p = seed.seedPlan
    kids = childPlans p
    branch = branchOf seed.seedTree
    scaffoldHeadText = renderGitOid scaffoldHead
    childTreeEntry s = (nodeName s.seedPlan, branchOf s.seedTree)

splitWork :: NodeSeed -> Maybe WorkerResult -> [NodeSeed] -> [Text] -> NodeWork
splitWork seed scaffold childSeeds denied =
  WorkReady
    { workSeed = seed
    , workScaffold = scaffold
    , workKids = childSeeds
    , workDenied = denied
    }

-- | The task a truncated node carries.  A coalgebra cannot produce an
-- outcome — its result type is @PlanF@ — so every veto in this file expresses
-- itself by handing the algebra a childless, refused node instead.
refusalWork :: NodeSeed -> Failure -> NodeWork
refusalWork seed f = WorkRefused {workSeed = seed, workFailure = f}

failureWork :: NodeSeed -> Failure -> NodeWork
failureWork seed f = WorkFailed {workSeed = seed, workFailure = f}

-- ---------------------------------------------------------------------------
-- The coalgebra's policy slots
--
-- Each is an ordinary function the middleware calls; each is effectful
-- (@a -> M (Maybe NodeWork)@) so it can tier — a deterministic heuristic
-- first, the operator past that — inside one function with ordinary
-- branching.  The two below that CAN be pure are pure, deliberately: a pure
-- slot is a slot a test can call directly.
-- ---------------------------------------------------------------------------

-- | The agent-cycle cost a node's own work requires: one for a leaf's
-- implementation, two for a node that splits (its own scaffold plus one
-- integration cycle).  Shared with 'childAllowance', which reserves the same
-- amount before dividing what remains among children — so the two can never
-- disagree about what "this node's own reservation" means.
requiredCycles :: DevPlan -> Int
requiredCycles p = if null (childPlans p) then 1 else 2

-- | 'Swarm.budgeted''s slot.  A node reserves its own scaffold plus one
-- integration cycle; a leaf reserves its implementation.  Resolution agents
-- are drawn from the children's shares, which is where the conflicts are.
cycleRefusal :: NodeSeed -> Harness (Maybe NodeWork)
cycleRefusal seed
  | seed.seedCycles >= required = pure Nothing
  | otherwise =
      pure
        ( Just
            ( refusalWork
                seed
                ( Failure
                    BudgetSpent
                    [fmt|{nodeName seed.seedPlan} needs {required} agent cycles, {seed.seedCycles} left in this subtree|]
                    []
                )
            )
        )
  where
    required = requiredCycles seed.seedPlan

-- | 'Swarm.capped''s slot.  A leaf at the depth limit is not capped — there
-- was nothing to unfold — which is exactly why the slot returns a 'Maybe'
-- rather than the wrapper deciding on depth alone.
depthRefusal :: NodeSeed -> Harness (Maybe NodeWork)
depthRefusal seed
  | null (childPlans seed.seedPlan) = pure Nothing
  | otherwise =
      pure
        ( Just
            ( refusalWork
                seed
                (Failure DepthCapped [fmt|depth cap reached at {nodeName seed.seedPlan}|] [])
            )
        )

-- | 'Swarm.gated''s slot: TIERED, and the reason the slots are effectful.
-- Tier 1 is a deterministic width heuristic and costs nothing.  Tier 2 hands
-- the operator a typed form with this layer's real child names attached — the
-- parent's scaffold has already landed by the time it runs, so the approval is
-- about work that exists rather than a speculative whole-tree sign-off.
layerGate :: Budget -> Swarm.PlanF NodeWork NodeSeed -> Harness (Maybe NodeWork)
layerGate b layer
  | length childSeeds <= b.gateWiderThan = pure Nothing
  | otherwise = do
      say [fmt|{nodeName parent.seedPlan} proposes {length childSeeds} children (gate is {b.gateWiderThan})|]
      approval <- askUser @LayerApproval
      pure $
        if approval.layerApproved
          then Nothing
          else
            Just
              ( refusalWork
                  parent
                  (Failure LayerRefused approval.approvalNote (map (nodeName . seedPlan) childSeeds))
              )
  where
    childSeeds = Swarm.kids layer
    parent = (Swarm.task layer).workSeed

-- | Seed one child per plan, in plan order.
--
-- HOW a child's worktree is obtained is a parameter, and that is the whole of
-- what resume changes here: a fresh unfold creates one from the parent's
-- CURRENT state (which by the ordering above is the scaffold worker's final
-- commit), while a replayed split REBINDS the retained tree the journal names.
-- Everything else — the allowance division, the plan ordering, a denial riding
-- on as data rather than failing the split — is one implementation either way.
allocateChildren
  :: NodeSeed
  -> [DevPlan]
  -> (DevPlan -> Harness (Either Text WorktreeHandle))
  -> Harness ([NodeSeed], [Text])
allocateChildren parent kids obtain = go kids
  where
    allowance = childAllowance parent (length kids)
    go [] = pure ([], [])
    go (k : rest) =
      obtain k >>= \case
        Left why -> do
          (seeds, denied) <- go rest
          pure (seeds, [fmt|{nodeName k}: {why}|] : denied)
        Right childTree -> do
          (seeds, denied) <- go rest
          let s =
                NodeSeed
                  { seedPlan = k
                  , seedTree = childTree
                  , seedDepth = parent.seedDepth + 1
                  , seedCycles = allowance
                  , seedAdopted = Nothing
                  }
          pure (s : seeds, denied)

-- | A child that has never existed: a new worktree off the parent's HEAD.
freshChild :: NodeSeed -> DevPlan -> Harness (Either Text WorktreeHandle)
freshChild parent k =
  createWorktree (fromWorktree parent.seedTree (nodeName k)) >>= \case
    Left err -> pure (Left (renderWorktreeError err))
    Right h -> pure (Right h)

-- | A child of a REPLAYED split: rebind the retained worktree the journal
-- names for it, and fall back to creating one only for a child the crash
-- caught before it was ever allocated.  Nothing is recreated and nothing is
-- deleted.
retainedChild :: NodeSeed -> [(Text, Text)] -> DevPlan -> Harness (Either Text WorktreeHandle)
retainedChild parent trees k = case lookup (nodeName k) trees of
  Nothing -> freshChild parent k
  Just branch -> fmap retainedHandle <$> retainWorktree branch

-- | Divide what is left after this node's own reservation among its children.
--
-- Conservative on purpose: a subtree that finishes under its share does not
-- return the remainder to its siblings.  That is the honest cost of enforcing
-- a budget with no shared mutable state in the row — and the division is
-- deterministic, so no scheduling order can change it.  NEVER clamped
-- upward: a share that floors to zero stays zero, so a node that cannot fund
-- every child hands the underfunded ones nothing rather than minting cycles
-- the parent doesn't have — 'cycleRefusal' turns that zero into a typed
-- budget refusal for that child instead of an overspend.
--
-- Delegates the arithmetic to 'Swarm.splitAllowance' (operator's type-level
-- review, 2026-08-17): every child gets the same floor share that combinator
-- computes, and its conservation law — property-tested in the
-- @swarm-spec-test@ suite's @SwarmSpec@, not re-derived here — is what
-- now guarantees no call site can mint a cycle from nothing, in place of the
-- old hand-rolled @max 0 (... ) \`div\` n@ this function used to carry
-- directly. `Swarm.mkCycles`/`Swarm.cyclesToInt` are the boundary: dev-tree's
-- own budget vocabulary ('NodeSeed.seedCycles', 'requiredCycles') stays
-- plain 'Int' — only this one call site speaks 'Swarm.Cycles'.
childAllowance :: NodeSeed -> Int -> Int
childAllowance parent n =
  case Swarm.splitAllowance (Swarm.mkCycles parent.seedCycles) (Swarm.mkCycles (requiredCycles parent.seedPlan)) n of
    (_, s : _) -> Swarm.cyclesToInt s
    (_, []) -> 0

-- ---------------------------------------------------------------------------
