{-# LANGUAGE DeriveFunctor #-}
{-# LANGUAGE DeriveFoldable #-}
{-# LANGUAGE DeriveTraversable #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The recursive companion's pure semantic fixture.
--
-- 'ThoughtF' is the companion's base functor: a node either finishes locally
-- or defines only its own next layer of branches — a branch carries its own
-- 'ForkBrief' and 'BranchRole', which a bare kids list would lose, so
-- 'thoughtHylo' is 'Tidepool.Swarm.hyloM's one-recursive-call-site shape
-- specialized to 'ThoughtF''s own derived 'Traversable' rather than
-- 'Tidepool.Swarm.PlanF''s task-plus-flat-kids shape — not a second
-- recursion engine; the body is the identical one-liner.
--
-- Budget clamping ('depthCapped', 'nodeCapped', 'fanOutCapped') mirrors
-- 'Tidepool.Swarm.capped'\/'budgeted'\/'gated': each forces a local 'Finish'
-- rather than letting the wrapped coalgebra run past its cap, stamped on the
-- result ('FinishOrigin' inside 'Draft') rather than materialized as a plan
-- the driver consults separately.
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
  , Budget (..)
  , ForcedReason (..)
  , FinishOrigin (..)
  , Draft (..)
  , branchCount

    -- * What crosses the model boundary vs. what the runtime stamps
  , ArtifactId (..)
  , EditIntent (..)
  , Evidence (..)
  , Artifact (..)
  , ProposedArtifact (..)
  , ModelContribution (..)
  , NodeFailure (..)
  , NodeReceipt (..)
  , NodeResult (..)
  , CompositionOrder (..)
  , FoldDecision (..)

    -- * The driver
  , Coalg
  , Alg
  , thoughtHylo

    -- * Budget middleware
  , depthCapped
  , nodeCapped
  , fanOutCapped
  ) where

import Control.Monad.State.Class (MonadState, get, put)
import Data.List.NonEmpty (NonEmpty)
import qualified Data.List.NonEmpty as NE
import Data.Text (Text)

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

-- | Depth, node-count, and fan-out caps (PRD locked decision 9). Rounds,
-- deadline, and cost are deferred — this pure fixture implements exactly the
-- three caps the lane spec asks for; nothing here forecloses adding the
-- rest later.
data Budget = Budget {maxDepth :: Int, maxNodes :: Int, maxFanOut :: Int}
  deriving (Eq, Show)

-- | Which cap forced a local finish.
data ForcedReason = ForcedDepth | ForcedNodeCount | ForcedFanOut
  deriving (Eq, Show)

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
-- What crosses the model boundary vs. what the runtime stamps
--
-- PRD's dataflow principle, locked: the model attests only to
-- 'ModelContribution' (no ids, no receipts — it cannot attest runtime
-- facts); the RUNTIME assigns ids, computes previews, and stamps the
-- receipt around it, producing 'NodeResult'. Keeping these as two distinct
-- types (rather than one record with optional runtime fields) makes "the
-- model cannot attest to its own execution" a type-level fact, not a
-- convention.
-- ---------------------------------------------------------------------------

newtype ArtifactId = ArtifactId Text
  deriving (Eq, Ord, Show)

-- | Placeholder for the PRD's opaque @EditIntent@ payload.
newtype EditIntent = EditIntent Text
  deriving (Eq, Show)

-- | Placeholder for the PRD's opaque @Evidence@ payload.
newtype Evidence = Evidence Text
  deriving (Eq, Show)

-- | An id-stamped artifact the runtime holds live (PRD: "held live by the
-- runtime"). No 'Eq' instance: a closure has no meaningful equality, only
-- its id and intent do — callers compare on those, never on the whole
-- value.
data Artifact s
  = EditArtifact ArtifactId EditIntent (s -> Either Text s)
  | EvidenceArtifact ArtifactId Evidence

-- | An unstamped artifact proposal, before the runtime assigns it an id.
data ProposedArtifact s = ProposedArtifact
  { proposedIntent :: EditIntent
  , -- | 'Nothing' for an evidence-only proposal.
    proposedApply :: Maybe (s -> Either Text s)
  }

-- | What the MODEL finalizes (PRD): a rendered view of this node plus
-- whatever it proposes — no ids, no receipts.
data ModelContribution s = ModelContribution
  { contributionView :: Text
  , contributionProposed :: [ProposedArtifact s]
  }

newtype NodeFailure = NodeFailure {failureReason :: Text}
  deriving (Eq, Show)

-- | What the runtime observed about one fold. 'receiptForced' is set exactly
-- when a budget cap produced this node's 'Finish' rather than the model
-- choosing to stop (PRD: "the transformation stamped in the receipt").
data NodeReceipt = NodeReceipt {receiptDepth :: Int, receiptForced :: Maybe ForcedReason}
  deriving (Eq, Show)

-- | What the RUNTIME constructs around a 'ModelContribution' — ids assigned,
-- receipt stamped, and failure representable (PRD locked decision 6): a
-- coalgebra or algebra invocation that exits abnormally is folded as
-- 'NodeFailed' at its branch position, never an exception that erases
-- sibling results.
data NodeResult s
  = NodeSucceeded
      { contribution :: ModelContribution s
      , artifacts :: [Artifact s]
      , receipt :: NodeReceipt
      }
  | NodeFailed
      { failure :: NodeFailure
      , receipt :: NodeReceipt
      }

-- | Placeholder for the PRD's opaque @CompositionOrder@ payload: the order
-- the algebra composes selected artifacts in.
newtype CompositionOrder = CompositionOrder [ArtifactId]
  deriving (Eq, Show)

-- | What the algebra's model window decides about a realized layer: a
-- synthesis, which child (or self-authored) artifacts to keep, in what
-- order, plus any new proposals of its own.
data FoldDecision s = FoldDecision
  { synthesis :: Text
  , selected :: [ArtifactId]
  , composition :: CompositionOrder
  , decisionProposed :: [ProposedArtifact s]
  }

-- ---------------------------------------------------------------------------
-- The driver
-- ---------------------------------------------------------------------------

-- | How to split: a seed becomes its next 'ThoughtF' layer.
type Coalg m a = a -> m (ThoughtF a)

-- | How to combine: a realized layer (branch seeds already replaced by their
-- worked results, in declared branch order — never completion order, PRD
-- locked decision 5) folds into one value.
type Alg m b = ThoughtF b -> m b

-- | The one recursive call site (mirrors 'Tidepool.Swarm.hyloM' exactly):
-- unfold with the coalgebra, recurse into every branch via 'ThoughtF''s own
-- derived 'Traversable' (which preserves declared order by construction —
-- that IS the completion-order guarantee, not something this function has
-- to enforce separately), then fold with the algebra. No 'ThoughtF' tree
-- ever materializes beyond the current layer.
thoughtHylo :: Monad m => Alg m b -> Coalg m a -> a -> m b
thoughtHylo alg coalg = go
  where
    go a = coalg a >>= traverse go >>= alg

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
  | depthOf a >= limit = pure (Finish (Draft "<depth cap reached>" (BudgetForced ForcedDepth) (depthOf a)))
  | otherwise = coalg a

-- | Refuse to unfold past a running node-count budget, mirroring
-- 'Tidepool.Swarm.budgeted' but over shared state rather than a per-seed
-- reader — node count is a property of the WHOLE traversal, not of any one
-- seed. Every node that is allowed to proceed spends one unit, counting the
-- root. Needs the seed's own depth too, purely to stamp the forced 'Draft'
-- correctly (see 'Draft''s doc).
nodeCapped :: MonadState Int m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
nodeCapped depthOf limit coalg a = do
  n <- get
  if n >= limit
    then pure (Finish (Draft "<node-count cap reached>" (BudgetForced ForcedNodeCount) (depthOf a)))
    else put (n + 1) >> coalg a

-- | Refuse a layer whose fan-out exceeds the cap, mirroring
-- 'Tidepool.Swarm.gated': the unfold has already happened when this
-- decides, because fan-out is a property of the produced layer, not the
-- seed — refusing here refuses the DESCENT, not the coalgebra's own work.
fanOutCapped :: Monad m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
fanOutCapped depthOf limit coalg a =
  coalg a >>= \layer ->
    pure
      ( if branchCount layer > limit
          then Finish (Draft "<fan-out cap reached>" (BudgetForced ForcedFanOut) (depthOf a))
          else layer
      )
