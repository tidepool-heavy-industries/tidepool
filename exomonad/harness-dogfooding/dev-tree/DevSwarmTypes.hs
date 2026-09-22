{-# LANGUAGE NoImplicitPrelude #-}

-- | Shared execution-domain values for the project-local development swarm.
--
-- These types describe a node while the authored plan is being interpreted;
-- they are neither plan-authoring syntax nor durable wire values.  Keeping
-- them below "Micro", "Unfold", "Fold", and "Resume" lets those interpreters
-- share one vocabulary without making any one execution phase its owner.
module DevSwarmTypes
  ( NodeSeed (..)
  , FoldEvidence (..)
  , PlanShape (..)
  , LeafStrategy (..)
  , BranchStrategy (..)
  , PlanShapeIssue (..)
  , planShape
  , planShapeIssue
  ) where

import Data.List.NonEmpty (NonEmpty (..))
import HarnessTypes
  ( CheckResult
  , DevPlan
  , Failure
  , RebaseNote
  , SplitSpec
  , childPlans
  , nodeScaffold
  , nodeSplit
  , planScaffolds
  )
import Tidepool.Effects (WorktreeHandle)
import Tidepool.Prelude
import Tidepool.Worktree (GitOid)

-- | Everything the coalgebra needs to begin interpreting one plan node.
data NodeSeed = NodeSeed
  { seedPlan    :: DevPlan
  , seedTree    :: WorktreeHandle
  , seedDepth   :: Int
  , -- | Agent-cycle allowance for this subtree.  It is divided structurally,
    -- so sibling completion order cannot affect the budget they receive.
    seedCycles  :: Int
  , -- | A verified scaffold commit recovered from a retained worktree.
    -- Fresh nodes always carry 'Nothing'.
    seedAdopted :: Maybe GitOid
  }

-- | Code-owned observations from one fold, gathered before a receipt is
-- constructed and judged.  A record keeps each observation named at call
-- sites and gives future evidence fields one place to live.
data FoldEvidence = FoldEvidence
  { foldHeads       :: (GitOid, GitOid)
  , foldRebases     :: [RebaseNote]
  , foldEscalations :: [Text]
  , foldCycles      :: Int
  , foldAgentRan    :: Bool
  , foldChecks      :: [CheckResult]
  , foldFailure     :: Maybe Failure
  }

-- | The executable shape of a plan node.  Unlike the permissive authoring and
-- wire record, this vocabulary cannot contain a childless branch or attach a
-- leaf strategy to a branch.
data PlanShape
  = LeafPlan LeafStrategy
  | BranchPlan BranchStrategy (NonEmpty DevPlan)
  deriving (Show, Eq)

-- | How a leaf performs its own work.
data LeafStrategy
  = DirectLeaf
  | SplitLeaf SplitSpec
  deriving (Show, Eq)

-- | How an interior node prepares the parent state its children fork from.
data BranchStrategy
  = ScaffoldBranch
  | IntegrationBranch
  deriving (Show, Eq)

-- | A field from the permissive input record that does not belong to the
-- node's actual constructor.  Acceptance rejects these for new plans; the
-- total 'planShape' normalization still gives older durable plans their
-- historical interpretation.
data PlanShapeIssue
  = LeafScaffoldSpecified
  | BranchSplitSpecified
  deriving (Show, Eq)

-- | Normalize the compatibility-shaped 'DevPlan' record into the smaller sum
-- the execution interpreters consume.  Children decide the outer constructor;
-- only fields belonging to that constructor are consulted after that point.
planShape :: DevPlan -> PlanShape
planShape p = case childPlans p of
  [] -> LeafPlan (maybe DirectLeaf SplitLeaf (nodeSplit p))
  child : rest ->
    BranchPlan
      (if planScaffolds p then ScaffoldBranch else IntegrationBranch)
      (child :| rest)

-- | Detect a cross-constructor field before accepting a newly authored or
-- model-proposed plan.  Kept separate from 'planShape' so legacy journaled
-- plans remain readable and normalize with the precedence they always had.
planShapeIssue :: DevPlan -> Maybe PlanShapeIssue
planShapeIssue p = case childPlans p of
  [] -> case nodeScaffold p of
    Just _ -> Just LeafScaffoldSpecified
    Nothing -> Nothing
  _ -> case nodeSplit p of
    Just _ -> Just BranchSplitSpecified
    Nothing -> Nothing
