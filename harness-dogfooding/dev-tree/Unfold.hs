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
  , childAllowances
  , escalationBudget
  , fundingShortfall
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
      -- A node that declares (or derives — 'planScaffolds') no scaffold has
      -- nothing to run: its children seed straight from the parent's current
      -- HEAD and NO worker spawns.  The typed form exists because a prose
      -- "make no changes" brief demonstrably cannot be trusted (two live
      -- runs had the scaffold invent files and trip its own boundary).
      Nothing | not (planScaffolds p) ->
        worktreeHead seed.seedTree >>= emitSplit seed Nothing
      Nothing ->
        runWorker seed.seedTree name (scaffoldPrompt p kids) >>= \case
          -- A spawn failure is a FAILURE of this node, not a policy refusal:
          -- 'WorkFailed' folds to a Failed outcome the parent's failure
          -- policy can retry or replan, where the refusal channel would
          -- launder a transient backend hiccup into a permanent Skipped.
          Left err ->
            pure
              ( Swarm.PlanF
                  (failureWork seed (Failure SpawnDenied [fmt|{name} scaffold: {renderSpawnError err}|] []))
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
-- The split is appended at least THREE times under the same @(kind, key)@,
-- and the fold keeps the later one (max seq): the first records the
-- DECISION, so a crash during child allocation replays the plan instead of
-- re-running the scaffold worker; 'allocateChildren' then appends
-- INCREMENTALLY, once per child worktree as it is created — a crash after
-- child K exists still finds child K's binding on resume, instead of
-- orphaning it behind a batch write that never landed; the final append here
-- closes with the complete picture (harmless even when it repeats the last
-- incremental one — same content, same max-seq winner).
emitSplit :: NodeSeed -> Maybe WorkerResult -> GitOid -> Harness (Swarm.PlanF NodeWork NodeSeed)
emitSplit seed scaffold scaffoldHead = do
  recordEvent (SplitEvent (JournalKey branch) p scaffoldHeadText Nothing)
  (childSeeds, denied) <- allocateChildren (JournalKey branch) p scaffoldHeadText [] seed kids (freshChild seed)
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
  | width <= b.gateWiderThan = pure Nothing
  | otherwise = do
      say [fmt|{nodeName parent.seedPlan} proposes {width} children (gate is {b.gateWiderThan})|]
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
    -- Gate on the PLANNED width for a ready split: worktree denials shrink
    -- the allocated seed list, and a wide unfold must not slip under the
    -- gate precisely when the run is already unhealthy enough to be denying
    -- worktrees.
    width = case Swarm.task layer of
      WorkReady {} -> length (childPlans parent.seedPlan)
      _ -> length childSeeds

-- | Seed one child per plan, in plan order.
--
-- HOW a child's worktree is obtained is a parameter, and that is the whole of
-- what resume changes here: a fresh unfold creates one from the parent's
-- CURRENT state (which by the ordering above is the scaffold worker's final
-- commit), while a replayed split REBINDS the retained tree the journal names.
-- Everything else — the allowance division, the plan ordering, a denial riding
-- on as data rather than failing the split — is one implementation either way.
--
-- The four extra parameters are what let this function CLOSE THE CRASH
-- WINDOW between allocating child worktrees: as soon as one is created, an
-- incremental 'SplitEvent' is journaled naming every child bound SO FAR
-- (@priorBound@ carries whatever a replayed split already recorded before
-- this call, @[]@ for a fresh split) — so a crash after child K's worktree
-- exists finds child K's binding on resume, instead of the batch-write gap
-- where only a completed allocation was ever durable and a crash mid-way
-- orphaned every worktree created before it.
allocateChildren
  :: JournalKey
  -> DevPlan
  -> Text
  -> [(Text, Text)]
  -> NodeSeed
  -> [DevPlan]
  -> (DevPlan -> Harness (Either Text WorktreeHandle))
  -> Harness ([NodeSeed], [Text])
allocateChildren journalKey plan scaffoldHeadText priorBound parent kids obtain =
  go priorBound (zip kids (childAllowances parent.seedCycles parent.seedPlan))
  where
    -- Allowances stay POSITIONALLY paired with their plans: keying positional
    -- data by name would silently hand two same-named children the first
    -- one's share.
    go _ [] = pure ([], [])
    go bound ((k, allowance) : rest) =
      obtain k >>= \case
        Left why -> do
          (seeds, denied) <- go bound rest
          pure (seeds, [fmt|{nodeName k}: {why}|] : denied)
        Right childTree -> do
          let boundNow = bound <> [(nodeName k, branchOf childTree)]
          recordEvent (SplitEvent journalKey plan scaffoldHeadText (Just boundNow))
          (seeds, denied) <- go boundNow rest
          let s =
                NodeSeed
                  { seedPlan = k
                  , seedTree = childTree
                  , seedDepth = parent.seedDepth + 1
                  , seedCycles = allowance
                  , seedAdopted = Nothing
                  }
          pure (s : seeds, denied)

-- | Per-child cycle allowances from one parent's allowance, in PLAN order.
-- Each plan's own 'nodeCycles' ask is honored, defaulting to the equal
-- floor share ('Swarm.splitAllowance'); asks that oversubscribe the
-- parent's remainder scale down proportionally and NEVER round up — a share
-- that floors to zero stays zero ('cycleRefusal' turns it into a typed
-- budget refusal), so no division here can mint a cycle the parent does not
-- hold.  This is what makes a sprint item's budget real at runtime — an
-- equal split across a wide sprint starves every interior item (sol review,
-- run 24).
--
-- The ONE division rule: 'allocateChildren' applies it at unfold time and
-- 'fundingShortfall' simulates it at proposal time, so the two cannot
-- disagree about what a plan costs.
childAllowances :: Int -> DevPlan -> [Int]
childAllowances cycles p = map allowanceFor asks
  where
    kids = childPlans p
    available = max 0 (cycles - requiredCycles p)
    equal = case Swarm.splitAllowance (Swarm.mkCycles cycles) (Swarm.mkCycles (requiredCycles p)) (length kids) of
      (_, s : _) -> Swarm.cyclesToInt s
      (_, []) -> 0
    asks = map (\k -> fromMaybe equal k.nodeCycles) kids
    totalAsk = sum asks
    allowanceFor ask
      | totalAsk <= available || totalAsk == 0 = ask
      | otherwise = ask * available `div` totalAsk

-- | What an interior node's OWN fold-time machinery — the eager rebase
-- cascade's resolution/retry cycles, and the integration worker — may spend,
-- drawn from nowhere new: it is what remains of the node's own 'seedCycles'
-- once its scaffold cost and its children's allocated shares are both
-- subtracted.  Total spend (scaffold + children's shares + this budget) can
-- therefore never exceed the subtree allowance the node was actually given —
-- the property "enforced, not advisory" claims but the eager cascade never
-- checked before now.  A cap of 0 is an honest, typed refusal
-- ('Fold.escalate'/'Fold.cascade' turn it into 'BudgetSpent' evidence rather
-- than spending anyway).
escalationBudget :: Bool -> NodeSeed -> Int
escalationBudget scaffoldRan seed =
  max 0 (seed.seedCycles - sum (childAllowances seed.seedCycles seed.seedPlan) - scaffoldCost)
  where
    scaffoldCost = if scaffoldRan then 1 else 0

-- | The first node the given allowance cannot fund, if any — the same
-- reservation-and-division walk the unfold performs, run at PROPOSAL time so
-- an unfundable plan is refused before an operator approval, a worktree, or
-- a scaffold cycle is spent on it.
fundingShortfall :: Int -> DevPlan -> Maybe Text
fundingShortfall cycles p
  | cycles < required =
      Just [fmt|node {nodeName p} needs {required} agent cycles but its share of the budget is {cycles} — the plan cannot be funded as shaped|]
  | otherwise = listToMaybe (catMaybes (zipWith fundingShortfall (childAllowances cycles p) (childPlans p)))
  where
    required = requiredCycles p

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
