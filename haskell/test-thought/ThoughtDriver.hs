-- | The old two-phase hylomorphism driver over 'Tidepool.Thought.ThoughtF' —
-- unfold with a coalgebra, recurse into every branch, fold with an algebra.
-- The production recursive companion ("Harness.hs") replaced this with an
-- explicit single recursive walk (@walkGroup@/@walkNode@/@foldAt@) because
-- the fold must stay attached to the node's own window/context ref, which a
-- generic 'Traversable' hylo's single abstract recursion site cannot carry.
-- 'ThoughtDriverTest.hs' is this driver's only remaining caller, exercising
-- 'Tidepool.Thought.ThoughtF''s pure driver contract directly rather than
-- through the companion.
module ThoughtDriver
  ( Alg
  , thoughtHylo
  , nodeCapped
  ) where

import Control.Monad.State.Class (MonadState, get, put)

import Tidepool.Thought

-- | How to combine: a realized layer (branch seeds already replaced by their
-- worked results, in declared branch order) folds into one value.
type Alg m b = ThoughtF b -> m b

-- | The one recursive call site: unfold with the coalgebra, recurse into
-- every branch via 'ThoughtF''s own derived 'Traversable' (which preserves
-- declared order by construction), then fold with the algebra. No
-- 'ThoughtF' tree ever materializes beyond the current layer.
thoughtHylo :: Monad m => Alg m b -> Coalg m a -> a -> m b
thoughtHylo alg coalg = go
  where
    go a = coalg a >>= traverse go >>= alg

-- | Refuse to unfold past a running node-count budget, over shared state
-- rather than a per-seed reader — node count is a property of the WHOLE
-- traversal, not of any one seed. Every node that is allowed to proceed
-- spends one unit, counting the root. Needs the seed's own depth too,
-- purely to stamp the forced 'Draft' correctly (see 'Draft''s doc).
nodeCapped :: MonadState Int m => (a -> Int) -> Int -> Coalg m a -> Coalg m a
nodeCapped depthOf limit coalg a = do
  n <- get
  if n >= limit
    then
      let reason = ForcedNodeCount n limit
       in pure (Finish (Draft (forcedDraftText reason) (BudgetForced reason) (depthOf a)))
    else put (n + 1) >> coalg a
