{-# LANGUAGE OverloadedStrings #-}

-- | Property coverage for "Tidepool.Swarm"'s 'Cycles' budget currency
-- (operator's type-level review, 2026-08-17): the conservation law
-- 'splitAllowance' is supposed to hold by construction, checked against many
-- random inputs rather than the one scenario the dev-tree regression pins.
--
-- Also covers 'hyloConcurrentM' (the concurrent-sibling hylomorphism): plan
-- order is preserved regardless of a supplied traversal's own internal
-- processing order, and running it under plain sequential 'mapM' reproduces
-- 'hyloM' exactly — both checked under a bare 'Identity', with no JIT/extract
-- compile (see CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md's addendum for why the
-- real concurrency proof is a separate GHC-heavy driver test instead).
module SwarmSpec (properties) where

import Data.Functor.Identity (Identity (..))
import Data.List (sortOn)
import Test.QuickCheck

import Tidepool.Swarm (Alg, Coalg, PlanF (..), cyclesToInt, hyloConcurrentM, hyloM, mkCycles, splitAllowance)

-- | The conservation law itself: nothing 'splitAllowance' hands out was ever
-- minted from nothing — the sum of every child's share plus what the parent
-- kept never exceeds the input allowance.
prop_splitAllowanceConserves :: Property
prop_splitAllowanceConserves =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (-2, 20)) $ \n ->
        let (kept, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
            grandTotal = cyclesToInt kept + sum (map cyclesToInt parts)
         in counterexample
              ( "input=" <> show inputN <> " reservation=" <> show reservationN <> " n=" <> show n
                  <> " kept=" <> show (cyclesToInt kept) <> " parts=" <> show (map cyclesToInt parts)
              )
              (grandTotal <= inputN)

-- | Every value 'splitAllowance' returns is itself non-negative — 'Cycles'
-- own invariant, pinned here at the one call site authorized to mint a split.
prop_splitAllowanceSharesNonNegative :: Property
prop_splitAllowanceSharesNonNegative =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (-2, 20)) $ \n ->
        let (kept, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
         in cyclesToInt kept >= 0 .&&. conjoin [counterexample (show p) (p >= 0) | p <- map cyclesToInt parts]

-- | Zero or fewer children: nothing is divided out, so the whole input stays
-- kept rather than a bogus per-child figure for zero recipients.
prop_splitAllowanceNoChildrenKeepsAll :: Property
prop_splitAllowanceNoChildrenKeepsAll =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (-5, 0)) $ \n ->
        let (kept, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
         in null parts .&&. cyclesToInt kept === inputN

-- | Every child gets exactly the same share (the floor of what remained
-- after the reservation, divided evenly) — 'splitAllowance' never favors one
-- child over another.
prop_splitAllowanceSharesAreUniform :: Property
prop_splitAllowanceSharesAreUniform =
  forAll (choose (0, 1000)) $ \inputN ->
    forAll (choose (0, 1000)) $ \reservationN ->
      forAll (choose (1, 20)) $ \n ->
        let (_, parts) = splitAllowance (mkCycles inputN) (mkCycles reservationN) n
         in case parts of
              (p : rest) -> conjoin [counterexample (show parts) (q === p) | q <- rest]
              [] -> counterexample "n >= 1 must produce at least one share" False

-- ---------------------------------------------------------------------------
-- 'hyloConcurrentM' coverage
-- ---------------------------------------------------------------------------

-- | A plan tree with its whole shape already decided — doubles as both the
-- seed type ('Coalg' unfolds one layer off the head) and the task carried at
-- each node, so the coalgebra below is a trivial "peel one layer" function
-- and all the interesting behavior lives in the algebra.
data RoseTree = RoseTree Int [RoseTree]
  deriving (Show)

instance Arbitrary RoseTree where
  arbitrary = sized go
    where
      go n = do
        v <- choose (0, 100 :: Int)
        let branchCap = if n <= 0 then 0 else 4
        k <- choose (0, branchCap)
        kids <- vectorOf k (go (n `div` 2))
        pure (RoseTree v kids)
  shrink (RoseTree v kids) =
    kids ++ [RoseTree v kids' | kids' <- shrink kids]

-- | Peel one layer: a node's own value plus the seeds of its children.
roseCoalg :: Coalg Identity Int RoseTree
roseCoalg (RoseTree v kids) = pure (PlanF v kids)

-- | An ORDER-SENSITIVE fold: a node's value consed onto its children's own
-- flattened lists, concatenated in the order the children arrive in the
-- 'PlanF'. Unlike a commutative fold (sum, max, ...), any reassembly bug that
-- swaps two siblings' results changes this output — which is the whole point
-- of using it to check 'hyloConcurrentM' never lets completion/processing
-- order leak into the reassembled tree.
roseAlg :: Alg Identity Int [Int]
roseAlg (PlanF v kids) = pure (v : concat kids)

-- | A traversal that processes a list in a DELIBERATELY non-identity order
-- (odd positions before even positions) before reassembling the result at
-- each element's ORIGINAL index — modeling a concurrent traversal whose
-- children finish in whatever order their own effects settle, the same
-- contract 'Tidepool.Async.mapConcurrently' documents for itself. Reduces to
-- 'mapM' precisely when the reassembly step is trusted, which is exactly
-- what these properties check.
scrambledTraverse :: Monad m => (a -> m b) -> [a] -> m [b]
scrambledTraverse f xs = do
  let indexed = zip [0 :: Int ..] xs
      processingOrder = oddsThenEvens indexed
  results <- mapM (\(i, x) -> (,) i <$> f x) processingOrder
  pure (map snd (sortOn fst results))
  where
    oddsThenEvens pairs =
      [p | p@(i, _) <- pairs, odd i] ++ [p | p@(i, _) <- pairs, even i]

-- | 'hyloConcurrentM' run under a traversal that scrambles processing order
-- reassembles EXACTLY what sequential 'hyloM' does — completion order is not
-- observable in the result, for the same algebra and the same tree.
prop_hyloConcurrentPreservesPlanOrder :: RoseTree -> Property
prop_hyloConcurrentPreservesPlanOrder tree =
  runIdentity (hyloConcurrentM scrambledTraverse roseAlg roseCoalg tree)
    === runIdentity (hyloM roseAlg roseCoalg tree)

-- | 'hyloConcurrentM' driven by plain 'mapM' (a trivially order-preserving,
-- non-concurrent traversal) is 'hyloM', not merely equivalent to it under
-- some algebra-specific coincidence.
prop_hyloConcurrentWithSequentialTraverseIsHyloM :: RoseTree -> Property
prop_hyloConcurrentWithSequentialTraverseIsHyloM tree =
  runIdentity (hyloConcurrentM mapM roseAlg roseCoalg tree)
    === runIdentity (hyloM roseAlg roseCoalg tree)

properties :: [(String, Property)]
properties =
  [ ("splitAllowance conserves (sum of parts + kept <= input)", prop_splitAllowanceConserves)
  , ("splitAllowance shares and kept are never negative", prop_splitAllowanceSharesNonNegative)
  , ("splitAllowance with no children keeps the whole input", prop_splitAllowanceNoChildrenKeepsAll)
  , ("splitAllowance divides evenly among children", prop_splitAllowanceSharesAreUniform)
  , ("hyloConcurrentM under a scrambled traversal matches hyloM (plan order, not completion order)", property prop_hyloConcurrentPreservesPlanOrder)
  , ("hyloConcurrentM under sequential mapM is hyloM", property prop_hyloConcurrentWithSequentialTraverseIsHyloM)
  ]
