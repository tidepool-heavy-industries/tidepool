{-# LANGUAGE DeriveFunctor, DeriveFoldable, DeriveTraversable #-}

-- | The swarm engine's hylo core (PRD 20, "The hylo core").
--
-- The swarm is a monadic hylomorphism: 'PlanF' is the plan's base functor,
-- and 'hyloM' unfolds a seed into a tree, works every node, and folds it
-- back down, without the tree ever materializing. Cognition enters only at
-- the two functions a caller supplies — the coalgebra (how to split) and
-- the algebra (how to combine) — never inside 'hyloM' itself.
--
-- Deliberately NOT here: any 'Strategy' parameter or concurrent/pooled
-- traversal (a later green-threads lane — PRD 20 "Concurrency substrate" /
-- "Green threads"); domain types like 'Outcome'\/'Review'\/'Spec' or the
-- middleware wrappers ('receipted'\/'budgeted'\/'gated'\/'capped') (dev-tree
-- v2, PRD 20 "Locked decisions — The hylo core" and "The trust ladder and
-- evidence"). This module is exactly the recursion scheme; everything that
-- decides what a node's task means, or how outcomes combine, is a caller.
module Tidepool.Swarm
  ( PlanF(..)
  , hyloM
  ) where

import Prelude (Monad, Functor, Foldable, Traversable, (>>=))
import Data.Traversable (traverse)

-- | The plan's base functor: a node carries a task and its unfolded
-- children. Derived over the last type parameter — 'Functor'\/'Foldable'
-- map/fold the children; 'Traversable' is what lets 'hyloM' thread an
-- effectful "work this child" over 'kids' via the single 'traverse' call.
data PlanF t a = PlanF { task :: t, kids :: [a] }
  deriving (Functor, Foldable, Traversable)

-- | The sequential monadic hylomorphism: unfold a seed with the coalgebra,
-- recurse into every child, then fold the worked children back up with the
-- algebra. The plan tree never materializes — 'go' holds only the current
-- node's frame.
hyloM :: Monad m => (PlanF t b -> m b) -> (a -> m (PlanF t a)) -> a -> m b
hyloM alg coalg = go
  where
    go a = coalg a >>= traverse go >>= alg
