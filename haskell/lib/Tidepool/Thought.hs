{-# LANGUAGE DeriveFunctor #-}
{-# LANGUAGE DeriveFoldable #-}
{-# LANGUAGE DeriveTraversable #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The recursive companion's pure semantic fixture.
--
-- 'ThoughtF' is the companion's base functor: a node either finishes locally
-- or defines only its own next layer of branches — a branch carries its own
-- 'ForkBrief' and 'BranchRole', which a bare kids list would lose.
--
-- Budget clamping ('depthCapped', 'fanOutCapped') mirrors
-- 'Tidepool.Swarm.capped'\/'gated': each forces a local 'Finish'
-- rather than letting the wrapped coalgebra run past its cap, stamped on the
-- result ('FinishOrigin' inside 'Draft') rather than materialized as a plan
-- the driver consults separately. (A third, @nodeCapped@, mirrors
-- 'Tidepool.Swarm.budgeted' the same way; it lives in
-- @haskell\/test-thought\/ThoughtDriver.hs@ with the rest of the old
-- two-phase driver, since the production companion carries its node
-- allowance structurally on the seed instead of over shared @MonadState
-- Int@.)
--
-- Genuine model-invocation failure is represented the same way: a coalgebra
-- that cannot decide a real layer still returns an ordinary 'Finish', tagged
-- 'InvocationFailed'.  A caller's algebra reads both budget-forced and
-- invocation-failed 'Finish' nodes as data through the same exhaustive match
-- it uses for everything else — nothing in this module throws.
module Tidepool.Thought
  ( -- * The base functor
    ThoughtF (..)
  , Branch (..)
  , ForkBrief (..)
  , BranchRole (..)
  , Strategy (..)

    -- * Budgets and forced finish
  , ForcedReason (..)
  , renderForcedReason
  , forcedDraftText
  , FinishOrigin (..)
  , Draft (..)
  , branchCount

    -- * A coalgebra invocation's own failure
  , NodeFailure (..)

    -- * The driver
  , Coalg

    -- * Budget middleware
  , depthCapped
  , fanOutCapped
  ) where

import Data.List.NonEmpty (NonEmpty (..))
import qualified Data.List.NonEmpty as NE
import Data.Text (Text)
import qualified Data.Text as T

-- ---------------------------------------------------------------------------
-- The base functor (PRD 21, "Core types")
-- ---------------------------------------------------------------------------

-- | One node's discovered layer: finish locally, or split into the next
-- layer's branches under one of three postures. Describes ONE layer only —
-- a branch's own value is an opaque @a@ (an undiscovered seed, or later a
-- worked result), never an already-unfolded subtree, so a single coalgebra
-- invocation is structurally incapable of deciding more than its own layer.
data ThoughtF a
  = Finish {draft :: Draft}
  | Explore {focus :: Text, branches :: NonEmpty (Branch a), strategy :: Strategy}
  | Compare {decision :: Text, options :: NonEmpty (Branch a), strategy :: Strategy}
  | Challenge {claim :: Text, attacks :: NonEmpty (Branch a), strategy :: Strategy}
  deriving (Eq, Show, Functor, Foldable, Traversable)

data Branch a = Branch {brief :: ForkBrief, value :: a}
  deriving (Eq, Show, Functor, Foldable, Traversable)

data ForkBrief = ForkBrief {title :: Text, role :: BranchRole, instruction :: Text}
  deriving (Eq, Show)

-- | Placeholder enumeration (PRD: types are starting points, "expected to be
-- edited through dogfood") — what stance a branch takes relative to its
-- parent's posture.
data BranchRole = Primary | Alternative | Critic
  deriving (Eq, Show)

-- | Controls scheduling only, never context inheritance (PRD 21) — every
-- child forks the same frozen snapshot regardless of 'Strategy'. Not
-- interpreted anywhere in this pure fixture: residency/concurrency is
-- existing substrate (green threads), out of C0's scope.
data Strategy = Sequential | Concurrent | Pooled Int
  deriving (Eq, Show)

-- ---------------------------------------------------------------------------
-- Budgets and forced finish
-- ---------------------------------------------------------------------------

-- | Which cap forced a local finish, carrying the exact numbers that decided
-- it — so a caller renders "depth cap 2 of 2 reached" without re-deriving
-- anything from the seed or config that produced the forced finish.
data ForcedReason
  = ForcedDepth {atDepth :: Int, depthLimit :: Int}
  | ForcedNodeCount {atNodeCount :: Int, nodeCountLimit :: Int}
  | ForcedFanOut {proposedBranches :: Int, fanOutLimit :: Int}
  deriving (Eq, Show)

-- | The ONE rendering of a 'ForcedReason' every caller shares: this module's
-- own middleware below uses it to stamp a forced 'Draft''s text, and the
-- companion's @HarnessTypes.renderOrigin@ reuses it verbatim for the journal
-- and the tree-line badge — so the two can never say something different
-- about the same forced finish.
renderForcedReason :: ForcedReason -> Text
renderForcedReason reason = case reason of
  ForcedDepth {atDepth = d, depthLimit = lim} ->
    "depth cap " <> tshow d <> "/" <> tshow lim <> " reached"
  ForcedNodeCount {atNodeCount = n, nodeCountLimit = lim} ->
    "node allowance exhausted (" <> tshow n <> "/" <> tshow lim <> ")"
  ForcedFanOut {proposedBranches = n, fanOutLimit = lim} ->
    "fan-out cap " <> tshow lim <> " exceeded (" <> tshow n <> " branches proposed)"
  where
    tshow = T.pack . show

-- | The 'Draft' text a budget-forced 'Finish' carries — what a parent's fold
-- window (and the journal's "draft" field) actually reads, never a bare
-- placeholder like the old @"\<depth cap reached\>"@ marker it replaces.
forcedDraftText :: ForcedReason -> Text
forcedDraftText reason = "this node stopped without exploring further — " <> renderForcedReason reason

-- | Why a node carries 'Finish': the model chose to stop, a budget forced it
-- (still an ordinary completion — PRD: "a model-proposed Strategy is
-- transformed EXPLICITLY... and the transformation stamped in the
-- receipt"), or the coalgebra invocation itself exited abnormally (a
-- genuine failure). Distinguishing these is what lets one caller algebra
-- fold both budget caps and invocation failure as ordinary data without
-- conflating "finished early, on purpose or by policy" with "never produced
-- a real answer".
data FinishOrigin
  = ModelFinished
  | BudgetForced ForcedReason
  | InvocationFailed NodeFailure
  deriving (Eq, Show)

-- | Placeholder for the PRD's opaque @Draft@ payload type, extended with
-- 'FinishOrigin' (why this node finished) and 'draftDepth' (this node's own
-- depth). The depth field exists because an algebra folding a bare
-- 'ThoughtF' layer can recover an interior node's depth from any child
-- (child depth minus one), but a childless 'Finish' has no child to read it
-- from — so the one node shape with no children is the one place depth must
-- ride on the data itself.
data Draft = Draft {draftText :: Text, draftOrigin :: FinishOrigin, draftDepth :: Int}
  deriving (Eq, Show)

-- | How many branches a layer declared — 0 for 'Finish'.
branchCount :: ThoughtF a -> Int
branchCount layer = case layer of
  Finish {} -> 0
  Explore {branches = bs} -> NE.length bs
  Compare {options = os} -> NE.length os
  Challenge {attacks = as} -> NE.length as

-- ---------------------------------------------------------------------------
-- A coalgebra invocation's own failure
-- ---------------------------------------------------------------------------

newtype NodeFailure = NodeFailure {failureReason :: Text}
  deriving (Eq, Show)

-- ---------------------------------------------------------------------------
-- The driver
-- ---------------------------------------------------------------------------

-- | How to split: a seed becomes its next 'ThoughtF' layer.
type Coalg m a = a -> m (ThoughtF a)

-- ---------------------------------------------------------------------------
-- Budget middleware — Coalg -> Coalg, mirroring Tidepool.Swarm's
-- capped/budgeted/gated (PRD locked decision 9: budgets clamp
-- deterministically, and a hard cap forces a local finish, stamped rather
-- than silently substituted).
-- ---------------------------------------------------------------------------

-- | Refuse to unfold past a depth, mirroring 'Tidepool.Swarm.capped': the
-- caller reads the seed's own depth (this fixture threads depth on the
-- seed, as 'Harness.hs' does for its own tree), and the wrapped coalgebra
-- never runs once the cap is reached — the cap is never itself the reason a
-- cycle gets spent.
depthCapped :: Monad m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
depthCapped depthOf limit coalg a
  | depthOf a >= limit =
      let reason = ForcedDepth (depthOf a) limit
       in pure (Finish (Draft (forcedDraftText reason) (BudgetForced reason) (depthOf a)))
  | otherwise = coalg a

-- | Refuse a layer whose fan-out exceeds the cap, mirroring
-- 'Tidepool.Swarm.gated': the unfold has already happened when this
-- decides, because fan-out is a property of the produced layer, not the
-- seed — refusing here refuses the DESCENT, not the coalgebra's own work.
fanOutCapped :: Monad m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
fanOutCapped depthOf limit coalg a =
  coalg a >>= \layer ->
    let n = branchCount layer
        reason = ForcedFanOut n limit
     in pure
          ( if n > limit
              then Finish (Draft (forcedDraftText reason) (BudgetForced reason) (depthOf a))
              else layer
          )
