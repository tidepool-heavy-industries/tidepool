{-# LANGUAGE DeriveFunctor, DeriveFoldable, DeriveTraversable #-}

-- | The swarm engine's hylo core and its policy middleware.
--
-- The swarm is a monadic hylomorphism: 'PlanF' is the plan's base functor,
-- and 'hyloM' unfolds a seed into a tree, works every node, and folds it
-- back down, without the tree ever materializing. Cognition enters only at
-- the two functions a caller supplies — the coalgebra (how to split) and
-- the algebra (how to combine) — never inside 'hyloM' itself.
--
-- == Policies are middleware over those two seams
--
-- 'receipted', 'budgeted', 'capped', and 'gated' are 'Alg'\/'Coalg'
-- transformers composed by ordinary function application:
--
-- @
-- let coalg = 'gated' approveLayer ('capped' depthOf maxDepth onCap ('budgeted' spend decompose))
--     alg   = 'receipted' stampFold integrate
-- in 'hyloM' alg coalg rootSeed
-- @
--
-- A new policy is a new wrapper, never a driver change, and each one is
-- testable in isolation against a pure algebra — no agent process anywhere in
-- the logic test path.
--
-- == Every policy slot is effectful, and that is the point
--
-- A slot is @a -> m (Maybe t)@, not @a -> Bool@. One ordinary function with
-- ordinary branching can therefore tier: run a cheap deterministic heuristic
-- first, escalate to a specifically-prompted model turn on ambiguity, consult
-- memory or the run journal, ask the operator past that. Pure policies are the
-- degenerate @pure . f@ case and should stay pure where they can, for
-- testability. A model call inside a slot is journaled and mock-substitutable
-- like any other model turn.
--
-- == Refusal truncates; it never throws
--
-- A coalgebra cannot produce an outcome — its result type is @PlanF t a@ — so
-- a coalgebra-side veto cannot "return a failure". It TRUNCATES the node to a
-- childless @PlanF t@ carrying the caller's refusal task, and the algebra
-- reads that as ordinary data through the same exhaustive case it uses for
-- everything else. This is why the wrappers are @Coalg -> Coalg@ rather than
-- exceptions, and it is what makes failure-as-data structural instead of a
-- rule to remember.
--
-- == Deliberately NOT here
--
-- Any 'Strategy' parameter or concurrent\/pooled traversal (a later
-- green-threads lane); node residency; and every domain type — @Outcome@, @Review@, @Spec@,
-- receipts, budgets, approvals. This module is the recursion scheme plus four
-- transformers over it; everything that decides what a node's task MEANS is a
-- caller's.
--
-- Journaling is deliberately not a wrapper either: a journal entry's payload
-- is domain-shaped (what was decided, spawned, and folded), and this module
-- cannot know it. A harness records at each swarm step from inside its own
-- algebra and coalgebra.
module Tidepool.Swarm
  ( -- * The scheme
    PlanF(..)
  , Alg
  , Coalg
  , hyloM
    -- * Policy middleware
  , receipted
  , budgeted
  , capped
  , gated
    -- * Cycles budget
  , Cycles
  , mkCycles
  , cyclesToInt
  , spendCycles
  , splitAllowance
  ) where

import Prelude
  ( Monad, Functor, Foldable, Traversable, Int, Eq, Show, Ord((>=), (<=))
  , Maybe(..), pure, (>>=), max, div, replicate, (-), (*), otherwise
  )
import Data.Traversable (traverse)

-- | The plan's base functor: a node carries a task and its unfolded
-- children. Derived over the last type parameter — 'Functor'\/'Foldable'
-- map/fold the children; 'Traversable' is what lets 'hyloM' thread an
-- effectful "work this child" over 'kids' via the single 'traverse' call.
data PlanF t a = PlanF { task :: t, kids :: [a] }
  deriving (Functor, Foldable, Traversable)

-- | How to combine: a node's task plus its children's worked results, in PLAN
-- order (never completion order — that is the concurrency-correctness
-- contract, and it holds by construction because 'traverse' preserves order).
type Alg m t b = PlanF t b -> m b

-- | How to split: a seed becomes a task plus the seeds of its children.
type Coalg m t a = a -> m (PlanF t a)

-- | The sequential monadic hylomorphism: unfold a seed with the coalgebra,
-- recurse into every child, then fold the worked children back up with the
-- algebra. The plan tree never materializes — 'go' holds only the current
-- node's frame.
--
-- @traverse go@ is the ONE recursive call site, and therefore the one place a
-- later lane changes: a 'Strategy' value interpreted here, or a resident node
-- loop replacing the one-shot traversal, leaves everything else in this module
-- untouched.
hyloM :: Monad m => Alg m t b -> Coalg m t a -> a -> m b
hyloM alg coalg = go
  where
    go a = coalg a >>= traverse go >>= alg

-- | Stamp every fold with its evidence.
--
-- The slot sees the node it folded AND the outcome the algebra produced, so it
-- can journal the fold, apply an evidence ladder to the outcome (a verdict
-- computed from receipts rather than claimed by the folder), or refuse an
-- outcome whose evidence does not support it — returning a different @b@ is
-- exactly how a refusal is expressed, since the fold has already happened and
-- what is in question is what it is WORTH.
--
-- It wraps the algebra rather than living inside it so that leaf folds and
-- interior folds cannot drift apart: there is one place a fold becomes
-- durable.
receipted :: Monad m => (PlanF t b -> b -> m b) -> Alg m t b -> Alg m t b
receipted stamp alg node = alg node >>= stamp node

-- | Refuse to unfold past a cap.
--
-- The primitive guard, and the shape every coalgebra-side veto takes: the slot
-- returns the refusal TASK to truncate with, or 'Nothing' to let the wrapped
-- coalgebra run. The wrapped coalgebra never runs on a refusal, which is the
-- point — a budget that only reported afterwards would have already spent the
-- cycle it was refusing.
--
-- Named for its motivating use (an agent-cycle or wall-clock allowance), but
-- the slot is the whole policy: what "past the cap" means is the caller's, and
-- because it is effectful it may consult the run journal or the operator
-- rather than only the seed in hand.
budgeted :: Monad m => (a -> m (Maybe t)) -> Coalg m t a -> Coalg m t a
budgeted veto coalg a =
  veto a >>= \refusal -> case refusal of
    Just t -> pure (PlanF t [])
    Nothing -> coalg a

-- | Refuse to unfold past a depth. 'budgeted' at a depth-shaped slot — the
-- same truncation, spelled so a reader of a composed coalgebra can see WHICH
-- cap is which without reading the slot.
--
-- @depthOf@ reads the depth off the seed (the seed carries it; this module has
-- no state), and @refuse@ still returns a 'Maybe' so a caller can decline to
-- cap a node that was never going to split anyway.
capped :: Monad m => (a -> Int) -> Int -> (a -> m (Maybe t)) -> Coalg m t a -> Coalg m t a
capped depthOf limit refuse =
  budgeted (\a -> if depthOf a >= limit then refuse a else pure Nothing)

-- | Approve a LAYER, after the unfold and before the descent.
--
-- The slot sees the produced @PlanF t a@, not the seed, and that is locked
-- (PRD 20): an operator approves one layer at a time with its parent's real
-- outcomes already attached, never a speculative whole-tree sign-off. Approving
-- before the unfold would mean approving a layer nobody has seen.
--
-- The unfold itself has already happened when the slot runs, so a gate that
-- refuses does not un-spend the coalgebra's own work — it refuses the DESCENT,
-- which is where the cost is.
gated :: Monad m => (PlanF t a -> m (Maybe t)) -> Coalg m t a -> Coalg m t a
gated approve coalg a =
  coalg a >>= \layer ->
    approve layer >>= \refusal -> case refusal of
      Just t -> pure (PlanF t [])
      Nothing -> pure layer

-- ---------------------------------------------------------------------------
-- Cycles budget (operator's type-level review, 2026-08-17)
--
-- An agent-cycle allowance, currency for the SAME budget 'budgeted' spends
-- against ("Harness.hs"'s dev-tree: one cycle for a leaf's implementation,
-- two for a node that splits). The raw constructor is NOT exported —
-- 'mkCycles' clamps to non-negative and 'spendCycles' is monus-style (never
-- underflows) — so a negative 'Cycles' is simply not representable, which is
-- what deletes "budget-minting-from-nothing" as a bug class rather than
-- merely guarding against it at whichever call site remembers to check.
-- ---------------------------------------------------------------------------

-- | An agent-cycle allowance. See the section doc above for why the
-- constructor stays unexported.
newtype Cycles = Cycles Int
  deriving (Eq, Show)

-- | Clamp a raw count into 'Cycles' — the only way to mint one from outside
-- this module.
mkCycles :: Int -> Cycles
mkCycles n = Cycles (max 0 n)

-- | Read the underlying count back out (e.g. to compare against a plain
-- 'Int' budget, or to render a receipt).
cyclesToInt :: Cycles -> Int
cyclesToInt (Cycles n) = n

-- | Monus subtraction: what is left of the first argument after spending the
-- second. Never goes negative — spending more than is held leaves 'Cycles'
-- 0's own zero, not a negative allowance.
spendCycles :: Cycles -> Cycles -> Cycles
spendCycles (Cycles a) (Cycles b) = Cycles (max 0 (a - b))

-- | Split an allowance among children after reserving a node's own cost:
-- @splitAllowance input reservation n@ divides what remains of @input@ after
-- @reservation@ evenly among @n@ children, returning @(kept, perChild)@ where
-- @perChild@ has exactly @n@ entries (all equal — the floor share) and
-- @kept@ absorbs both the reservation itself and the division remainder.
--
-- CONSERVATION LAW (property-tested — see the @thought-driver-test@ suite's
-- @SwarmSpec@): @sum perChild + kept@ never exceeds @input@ (in fact it is
-- always EXACTLY @input@, since 'spendCycles' never underflows and every
-- unit taken from @input@ lands in either a child's share or @kept@) — no
-- call site can ever mint a cycle this combinator did not account for.
--
-- @n <= 0@ divides among nobody: the whole @input@ stays kept and
-- @perChild@ is @[]@, never a bogus per-child figure for zero recipients.
splitAllowance :: Cycles -> Cycles -> Int -> (Cycles, [Cycles])
splitAllowance input reservation n
  | n <= 0 = (input, [])
  | otherwise = (kept, replicate n perChild)
  where
    available = input `spendCycles` reservation
    perChild = mkCycles (cyclesToInt available `div` n)
    distributed = mkCycles (cyclesToInt perChild * n)
    kept = input `spendCycles` distributed
