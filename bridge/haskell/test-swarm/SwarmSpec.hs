{-# LANGUAGE OverloadedStrings #-}

-- | Independent allocation laws and order-sensitive recursion properties.
module SwarmSpec (properties, allocationContractHistories, allocationContractPartitions) where

import Control.Monad (unless)
import Data.Functor.Identity (Identity (..))
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.List (sortOn)
import Test.QuickCheck hiding (label)

import Tidepool.Swarm (Alg, Coalg, PlanF (..), cyclesToInt, hyloConcurrentM, hyloM, mkCycles, splitAllowance)

data Allocation = Allocation Int Int Int deriving (Eq, Show)

allocations :: Gen Allocation
allocations = frequency
  [ (2, Allocation <$> chooseInt (0, 1000) <*> chooseInt (0, 1000) <*> chooseInt (-2, 0))
  , (1, Allocation 0 <$> chooseInt (0, 1000) <*> chooseInt (1, 20))
  , (2, do
      input <- chooseInt (0, 1000)
      Allocation input <$> chooseInt (input, 1001) <*> chooseInt (1, 20))
  , (3, do
      input <- chooseInt (1, 1000)
      Allocation input <$> chooseInt (0, input - 1) <*> chooseInt (1, 20))
  , (1, Allocation <$> elements [0, 1, maxBound - 1, maxBound]
      <*> elements [0, 1, maxBound - 1, maxBound] <*> chooseInt (-2, 20))
  , (1, Allocation <$> chooseInt (0, 1000) <*> chooseInt (0, 1000) <*> chooseInt (-2, 20))
  ]

shrinkAllocation :: Allocation -> [Allocation]
shrinkAllocation (Allocation input reservation children) =
  [Allocation i r n | (i,r,n) <- shrink (input,reservation,children), i >= 0, r >= 0]

-- Count, conservation and maximal-share inequalities determine the contract
-- without using the implementation's division or spendCycles operation.
-- Integer observations keep an overflowing Int oracle from hiding a defect.
allocationObservation :: Allocation -> (String, [(String, Bool)], [String])
allocationObservation request@(Allocation input reservation children) =
  let (kept, parts) = splitAllowance (mkCycles input) (mkCycles reservation) children
      held = toInteger (cyclesToInt kept)
      shares = map (toInteger . cyclesToInt) parts
      offered = toInteger input
      reserved = toInteger reservation
      available = max 0 (offered - reserved)
      count = toInteger children
      laws =
        [ ("exact child count", length shares == max 0 children)
        , ("exact conservation", held + sum shares == offered)
        , ("kept bounds", 0 <= held && held <= offered)
        , ("share bounds", all (\q -> 0 <= q && q <= offered) shares)
        , ("reservation remains kept", children <= 0 || held >= min offered reserved)
        , ("no children keep the whole allowance", children > 0 || held == offered)
        , ("maximal equal shares", children <= 0 || all
            (\q -> count*q <= available && available < count*(q+1)) shares)
        ]
      partitions =
        [ label | (label, reached) <-
          [ ("no children", children <= 0)
          , ("multiple children", children > 1)
          , ("zero allowance", input == 0)
          , ("reservation exhausted", children > 0 && reservation >= input)
          , ("positive shares possible", children > 0 && available >= count)
          , ("division remainder", children > 0 && available `mod` count /= 0)
          , ("machine bounds", input == maxBound || reservation == maxBound)
          ], reached ]
  in (show request ++ " -> kept=" ++ show held ++ " shares=" ++ show shares, laws, partitions)

allocationContractPartitions :: IO ()
allocationContractPartitions = do
  let requests =
        [Allocation 10 2 2, Allocation 10 2 3, Allocation 10 2 1,
         Allocation 10 2 0, Allocation 10 2 (-2), Allocation 0 0 2,
         Allocation 10 10 2, Allocation 10 11 2, Allocation maxBound 1 3,
         Allocation 1 maxBound 2]
  mapM_ (\request -> let (trace,laws,_) = allocationObservation request
    in unless (all snd laws) (fail (trace ++ ": " ++ show (filter (not . snd) laws)))) requests
  putStrLn ("allocation deterministic partition evaluations=" ++ show (length requests))

allocationContractHistories :: Args -> IO ()
allocationContractHistories arguments = do
  observed <- newIORef (0 :: Int, [] :: [(String, Int)])
  initialFailure <- newIORef Nothing
  result <- quickCheckWithResult arguments $ forAllShrink allocations shrinkAllocation $ \request ->
    ioProperty $ do
      let (trace,laws,partitions) = allocationObservation request
      modifyIORef' observed $ \(callbacks,counts) ->
        (callbacks+1, foldr (\label rows ->
          (label, maybe 1 (+1) (lookup label rows)) : filter ((/= label) . fst) rows) counts partitions)
      unless (all snd laws) $ modifyIORef' initialFailure $ \prior -> case prior of
        Nothing -> Just trace
        Just _ -> prior
      pure (counterexample trace (conjoin [counterexample label passed | (label,passed) <- laws]))
  (callbacks,partitions) <- readIORef observed
  firstFailure <- readIORef initialFailure
  putStrLn ("allocation configured fresh cases=" ++ show (maxSuccess arguments)
    ++ " actual callbacks=" ++ show callbacks ++ " observed partitions=" ++ show partitions)
  putStrLn ("allocation result=" ++ show result)
  putStrLn ("allocation initial failure=" ++ show firstFailure)
  unless (isSuccess result) (fail "allocation contract property failed")

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
  [ ("hyloConcurrentM under a scrambled traversal matches hyloM (plan order, not completion order)", property prop_hyloConcurrentPreservesPlanOrder)
  , ("hyloConcurrentM under sequential mapM is hyloM", property prop_hyloConcurrentWithSequentialTraverseIsHyloM)
  ]
