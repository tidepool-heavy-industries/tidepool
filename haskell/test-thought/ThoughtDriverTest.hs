{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

-- | PRD 21 lane C0 — the four property families STEP 3 asks for, over
-- "Tidepool.Thought"'s pure driver: layer-at-a-time discovery,
-- completion-order permutation invariance, failure accumulation, and caps
-- forcing local finish. A plain @exitcode-stdio-1.0@ test-suite (the
-- @varid-mechanism-test@/@extract-fidelity-test@ precedent), not hspec/tasty
-- — QuickCheck alone is already a new dependency for this package, and
-- reusing this codebase's existing "assert and exit non-zero" idiom keeps it
-- to one.
module Main (main) where

import Control.Monad.State (State, evalState, get, modify, put, runState)
import Data.Functor.Identity (Identity, runIdentity)
import qualified Data.List as L
import Data.List.NonEmpty (NonEmpty (..))
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import Data.Map.Strict (Map)
import Data.Text (Text)
import qualified Data.Text as T
import System.Exit (exitFailure, exitSuccess)
import Test.QuickCheck

import SwarmSpec (properties)
import ThoughtDriver (nodeCapped, thoughtHylo)
import Tidepool.Thought

-- ---------------------------------------------------------------------------
-- A scripted tree: what the coalgebra will produce for every seed, decided
-- up front so a property can assert against a known shape instead of
-- guessing what a live model would have said. Every node carries a unique
-- 'NodeId', assigned by 'labelIds' in pre-order.
-- ---------------------------------------------------------------------------

type NodeId = Int

data Shape
  = ShFinish Text
  | ShLayer Kind Text (NonEmpty (BranchRole, Text, Shape))
  deriving (Show)

data Kind = KExplore | KCompare | KChallenge
  deriving (Show)

data Script
  = SFinish NodeId Text
  | SLayer NodeId Kind Text (NonEmpty (BranchRole, Text, Script))
  deriving (Show)

scriptId :: Script -> NodeId
scriptId = \case
  SFinish n _ -> n
  SLayer n _ _ _ -> n

labelIds :: Shape -> Script
labelIds shape = evalState (go shape) 0
  where
    fresh :: State Int Int
    fresh = get >>= \n -> put (n + 1) >> pure n
    go :: Shape -> State Int Script
    go = \case
      ShFinish t -> SFinish <$> fresh <*> pure t
      ShLayer k t bs -> do
        n <- fresh
        bs' <- traverse (\(r, bt, s) -> (,,) r bt <$> go s) bs
        pure (SLayer n k t bs')

allIds :: Script -> [NodeId]
allIds s = scriptId s : concatMap allIds (childScripts s)

childScripts :: Script -> [Script]
childScripts = \case
  SFinish {} -> []
  SLayer _ _ _ bs -> [c | (_, _, c) <- NE.toList bs]

leafIds :: Script -> [NodeId]
leafIds s = case s of
  SFinish n _ -> [n]
  SLayer {} -> concatMap leafIds (childScripts s)

parentChildPairs :: Script -> [(NodeId, NodeId)]
parentChildPairs s = [(scriptId s, scriptId c) | c <- childScripts s] ++ concatMap parentChildPairs (childScripts s)

-- ---------------------------------------------------------------------------
-- Generators
-- ---------------------------------------------------------------------------

genText :: Gen Text
genText = T.pack <$> vectorOf 5 (elements ['a' .. 'z'])

genShape :: Int -> Gen Shape
genShape depthBudget
  | depthBudget <= 0 = ShFinish <$> genText
  | otherwise =
      frequency
        [ (1, ShFinish <$> genText)
        ,
          ( 3
          , do
              k <- elements [KExplore, KCompare, KChallenge]
              t <- genText
              n <- choose (1, 3)
              bs <- vectorOf n (do r <- elements [Primary, Alternative, Critic]; bt <- genText; s <- genShape (depthBudget - 1); pure (r, bt, s))
              pure (ShLayer k t (NE.fromList bs))
          )
        ]

-- ---------------------------------------------------------------------------
-- Seed = a script node plus its own depth, and the base coalgebra/algebra
-- shared by the first three property families (no budgets involved).
-- ---------------------------------------------------------------------------

data Seed = Seed {seedDepth :: Int, _seedScript :: Script}

toLayer :: Script -> ThoughtF Script
toLayer = \case
  SFinish _ t -> Finish (Draft t ModelFinished 0) -- depth is overwritten by seedCoalg below
  SLayer _ k t bs -> mk k t (NE.map (\(r, bt, c) -> Branch (ForkBrief bt r "do it") c) bs)
  where
    mk KExplore t bs = Explore t bs Sequential
    mk KCompare t bs = Compare t bs Sequential
    mk KChallenge t bs = Challenge t bs Sequential

-- | The base coalgebra: reveal exactly this seed's own next layer, tagging
-- 'Finish' with the real depth and, for ids in 'fails', an
-- 'InvocationFailed' origin simulating an exited model invocation (PRD
-- locked decision 6) rather than a thrown exception.
seedCoalg :: [NodeId] -> Seed -> ThoughtF Seed
seedCoalg fails (Seed d s)
  | scriptId s `elem` fails = Finish (Draft "<invocation exit>" (InvocationFailed (NodeFailure "simulated model exit")) d)
  | otherwise = case toLayer s of
      Finish dr -> Finish dr {draftDepth = d}
      layer -> fmap (Seed (d + 1)) layer

-- | What the model finalizes: a rendered view of a node. This test's own
-- witness type — no production consumer defines the real recursive
-- companion's own configuration/result shapes instead.
newtype ModelContribution = ModelContribution
  { contributionView :: Text
  }

-- | What the runtime observed about one fold in this test's witness tree.
data NodeReceipt = NodeReceipt {receiptDepth :: Int, receiptForced :: Maybe ForcedReason}

-- | What the runtime constructs around a 'ModelContribution' for this
-- test's witness tree — receipt stamped, and failure representable: a
-- coalgebra or algebra invocation that exits abnormally is folded as
-- 'NodeFailed' at its branch position, never an exception that erases
-- sibling results.
data NodeResult
  = NodeSucceeded
      { contribution :: ModelContribution
      , receipt :: NodeReceipt
      }
  | NodeFailed
      { failure :: NodeFailure
      , receipt :: NodeReceipt
      }

-- | A witness the test can walk: the driven 'NodeResult' plus (for every
-- non-leaf) its already-folded children, so properties can inspect shape
-- without re-deriving it from 'NodeResult' alone (which does not keep
-- children once folded — that is the runtime's business, not this fixture's).
data Folded = Folded {_wResult :: NodeResult, wDepth :: Int, _wChildren :: [Folded]}

witnessAlg :: Monad m => ThoughtF Folded -> m Folded
witnessAlg layer = pure (Folded result d kids)
  where
    kids = foldr (:) [] layer
    d = case layer of
      Finish dr -> draftDepth dr
      _ -> case kids of
        (c : _) -> wDepth c - 1
        [] -> 0 -- unreachable: Explore/Compare/Challenge branches are NonEmpty
    result = case layer of
      Finish (Draft _ (InvocationFailed nf) _) -> NodeFailed nf (NodeReceipt d Nothing)
      Finish (Draft t (BudgetForced r) _) -> NodeSucceeded (ModelContribution t) (NodeReceipt d (Just r))
      Finish (Draft t ModelFinished _) -> NodeSucceeded (ModelContribution t) (NodeReceipt d Nothing)
      other -> NodeSucceeded (ModelContribution (synthesize other)) (NodeReceipt d Nothing)

synthesize :: ThoughtF Folded -> Text
synthesize = \case
  Explore f _ _ -> "explored: " <> f
  Compare dTxt _ _ -> "compared: " <> dTxt
  Challenge c _ _ -> "challenged: " <> c
  Finish _ -> ""

-- | The comparable projection of a 'Folded': everything except the runtime
-- machinery.
data Snapshot = Snapshot
  { snapView :: Maybe Text
  , snapFailed :: Maybe Text
  , snapDepth :: Int
  , snapForced :: Maybe ForcedReason
  , snapKids :: [Snapshot]
  }
  deriving (Eq, Show)

snapshot :: Folded -> Snapshot
snapshot (Folded res d kids) =
  Snapshot
    { snapView = case res of NodeSucceeded c _ -> Just (contributionView c); NodeFailed {} -> Nothing
    , snapFailed = case res of NodeFailed f _ -> Just (failureReason f); NodeSucceeded {} -> Nothing
    , snapDepth = d
    , snapForced = receipt' res
    , snapKids = map snapshot kids
    }
  where
    receipt' (NodeSucceeded _ r) = receiptForced r
    receipt' (NodeFailed _ r) = receiptForced r

countNodes :: Snapshot -> Int
countNodes s = 1 + sum (map countNodes (snapKids s))

maxDepthOf :: Snapshot -> Int
maxDepthOf s = maximum (snapDepth s : map maxDepthOf (snapKids s))

-- ---------------------------------------------------------------------------
-- Property 1 — layer-at-a-time discovery: the coalgebra trace visits every
-- node exactly once, and a node's own visit always precedes every one of its
-- descendants' visits (a coalgebra call can only ever hand back its own
-- immediate layer, never a materialized subtree, so nothing can visit a
-- grandchild before its parent).
-- ---------------------------------------------------------------------------

tracingCoalg :: Seed -> State [NodeId] (ThoughtF Seed)
tracingCoalg seed@(Seed _ s) = modify (++ [scriptId s]) >> pure (seedCoalg [] seed)

prop_layerAtATime :: Property
prop_layerAtATime = forAll (genShape 3) $ \shape ->
  let script = labelIds shape
      seed0 = Seed 0 script
      (_ :: Folded, trace) = runState (thoughtHylo witnessAlg tracingCoalg seed0) []
      ids = allIds script
   in counterexample ("trace: " <> show trace <> " ids: " <> show ids) $
        conjoin
          [ counterexample "every node visited exactly once" (L.sort trace === L.sort ids)
          , counterexample "no node visited twice" (L.nub trace === trace)
          , counterexample
              "parent visited before every child"
              (conjoin [counterexample (show (p, c)) (indexOf p trace < indexOf c trace) | (p, c) <- parentChildPairs script])
          ]
  where
    indexOf x xs = case [i | (i, y) <- zip [(0 :: Int) ..] xs, y == x] of
      (i : _) -> i
      [] -> error "indexOf: not found"

-- ---------------------------------------------------------------------------
-- Property 2 — completion-order permutation invariance: the algebra folds
-- children in DECLARED branch order, never completion order (PRD locked
-- decision 5). Simulate two different orders in which children's work
-- "arrives" via a side log threaded alongside the real computation, and
-- assert the real folded value never depends on it — only the (discarded)
-- log does.
-- ---------------------------------------------------------------------------

arrivalCoalg :: Map NodeId Int -> Seed -> State [(NodeId, Int)] (ThoughtF Seed)
arrivalCoalg arrival seed@(Seed _ s) = do
  case s of
    SFinish n _ -> modify (++ [(n, Map.findWithDefault (-1) n arrival)])
    SLayer {} -> pure ()
  pure (seedCoalg [] seed)

prop_permutationInvariance :: Property
prop_permutationInvariance = forAll (genShape 3) $ \shape ->
  let script = labelIds shape
      leaves = leafIds script
   in length leaves >= 2 ==> forAll (shuffle leaves) $ \perm1 ->
        forAll (shuffle leaves) $ \perm2 ->
          let seed0 = Seed 0 script
              table1 = Map.fromList (zip perm1 [0 ..])
              table2 = Map.fromList (zip perm2 [0 ..])
              (w1, log1) = runState (thoughtHylo witnessAlg (arrivalCoalg table1) seed0) []
              (w2, log2) = runState (thoughtHylo witnessAlg (arrivalCoalg table2) seed0) []
           in counterexample ("perm1=" <> show perm1 <> " perm2=" <> show perm2) $
                conjoin
                  [ counterexample "fold is order-invariant" (snapshot w1 === snapshot w2)
                  , counterexample "both orders visited every leaf" (map fst log1 === leaves .&&. map fst log2 === leaves)
                  ]

-- ---------------------------------------------------------------------------
-- Property 3 — failure accumulation: a coalgebra invocation failure at one
-- branch does not throw and does not stop siblings; the parent's algebra
-- receives every branch, failed or not, in the same declared-order layer.
-- ---------------------------------------------------------------------------

prop_failureAccumulation :: Property
prop_failureAccumulation = forAll (choose (2, 4)) $ \n ->
  forAll (choose (0, n - 1)) $ \failIdx ->
    let bs = NE.fromList [(Primary, "b" <> T.pack (show i), ShFinish ("finish" <> T.pack (show i))) | i <- [1 .. n]]
        shape = ShLayer KExplore "root" bs
        script = labelIds shape
        rootChildren = childScripts script
        failedId = scriptId (rootChildren !! failIdx)
        seed0 = Seed 0 script
        w = runIdentity (thoughtHylo witnessAlg (pure . seedCoalg [failedId]) seed0)
        snap = snapshot w
     in counterexample (show (map snapFailed (snapKids snap))) $
          conjoin
            [ counterexample "root itself still succeeds" (snapView snap =/= Nothing)
            , counterexample "every branch reached the algebra" (length (snapKids snap) === n)
            , counterexample "exactly one branch failed" (length (filter (/= Nothing) (map snapFailed (snapKids snap))) === 1)
            , counterexample "the failed branch is the one that was made to fail" (snapFailed (snapKids snap !! failIdx) =/= Nothing)
            , counterexample "every other branch succeeded" (length (filter (== Nothing) (map snapFailed (snapKids snap))) === n - 1)
            ]

-- ---------------------------------------------------------------------------
-- Property 4 — caps forcing local finish: depth, node-count, and fan-out
-- budgets each clamp the tree deterministically, and every clamp is stamped
-- on the result rather than silently dropped. The base coalgebra here would
-- branch forever left unchecked, so termination itself is part of what each
-- sub-property demonstrates. Each cap is exercised IN ISOLATION: composing
-- all three onto one traversal makes "how many nodes may exist" ambiguous
-- with "how many nodes may themselves EXPAND" (a node that a cap forces to
-- 'Finish' is already a node — the caps bound further branching from a
-- point, not retroactively the count of points already reached), so a
-- combined property would need to re-derive that interaction instead of
-- testing each cap's own contract.
-- ---------------------------------------------------------------------------

unboundedCoalg :: Int -> Seed -> ThoughtF Seed
unboundedCoalg fanout (Seed d _) =
  Explore
    "keep going"
    (NE.fromList [Branch (ForkBrief ("child" <> T.pack (show i)) Primary "go") (Seed (d + 1) (SFinish 0 "")) | i <- [1 .. fanout]])
    Sequential

-- | Whether a 'Snapshot''s own forced reason is the named constructor,
-- irrespective of the numbers it now carries — the properties below assert
-- on WHICH cap fired, not on the exact numbers (those are exercised directly
-- by the companion's own acceptance tier).
isForcedNodeCount :: Maybe ForcedReason -> Bool
isForcedNodeCount (Just ForcedNodeCount {}) = True
isForcedNodeCount _ = False

isForcedDepth :: Maybe ForcedReason -> Bool
isForcedDepth (Just ForcedDepth {}) = True
isForcedDepth _ = False

-- | Nodes whose own layer was genuinely decided by the base coalgebra —
-- i.e. everything except a node the node-count cap forced. What
-- 'nodeCapped' actually bounds is how many nodes may EXPAND (spend a real
-- coalgebra invocation), mirroring 'Tidepool.Swarm.budgeted''s cycle-cost
-- reading in "Harness.hs" — not the final tree's total size, which the
-- last permitted expansion's own fan-out can still grow past the limit.
countExpandable :: Snapshot -> Int
countExpandable s = (if isForcedNodeCount (snapForced s) then 0 else 1) + sum (map countExpandable (snapKids s))

allLeavesAtDepthForced :: Int -> Snapshot -> Bool
allLeavesAtDepthForced limit s
  | snapDepth s == limit = isForcedDepth (snapForced s) && null (snapKids s)
  | otherwise = all (allLeavesAtDepthForced limit) (snapKids s)

-- | Depth alone: a full tree of the given fan-out down to exactly
-- 'depthLimit', every node at the cap stamped 'ForcedDepth' and childless.
prop_depthCap :: Property
prop_depthCap =
  forAll (choose (0, 4)) $ \depthLimit ->
    forAll (choose (1, 3)) $ \fanoutWanted ->
      let seed0 = Seed 0 (SFinish 0 "")
          coalg = depthCapped seedDepth depthLimit (pure . unboundedCoalg fanoutWanted)
          w = snapshot (runIdentity (thoughtHylo witnessAlg coalg seed0))
          expectedCount = sum [fanoutWanted ^ i | i <- [0 .. depthLimit]]
       in counterexample (show (depthLimit, fanoutWanted, countNodes w, expectedCount)) $
            conjoin
              [ counterexample "max depth reached is exactly the cap" (maxDepthOf w === depthLimit)
              , counterexample "exact node count for a full tree of this depth/fan-out" (countNodes w === expectedCount)
              , counterexample "every node at the cap is stamped ForcedDepth and childless" (allLeavesAtDepthForced depthLimit w)
              ]

-- | Node count alone: the base coalgebra would branch forever; the number
-- of nodes allowed to actually expand never exceeds the budget.
prop_nodeCap :: Property
prop_nodeCap =
  forAll (choose (1, 30)) $ \nodeLimit ->
    forAll (choose (1, 3)) $ \fanoutWanted ->
      let seed0 = Seed 0 (SFinish 0 "")
          coalg = nodeCapped seedDepth nodeLimit (pure . unboundedCoalg fanoutWanted)
          w = snapshot (evalState (thoughtHylo witnessAlg coalg seed0) 0)
       in counterexample (show (nodeLimit, fanoutWanted, countExpandable w)) $
            counterexample "nodes allowed to actually expand stay within the node budget" (countExpandable w <= nodeLimit)

-- | Fan-out alone: a layer whose declared branch count exceeds the cap is
-- forced to 'Finish' before any of those branches become nodes at all —
-- unlike the depth/node caps, a fan-out refusal is a 'Tidepool.Swarm.gated'-
-- style post-check, so termination is immediate rather than after a probe.
prop_fanOutCap :: Property
prop_fanOutCap =
  forAll (choose (1, 3)) $ \fanLimit ->
    forAll (choose (fanLimit + 1, fanLimit + 3)) $ \fanoutWanted ->
      let seed0 = Seed 0 (SFinish 0 "")
          coalg = fanOutCapped seedDepth fanLimit (pure . unboundedCoalg fanoutWanted)
          w = snapshot (runIdentity (thoughtHylo witnessAlg coalg seed0))
       in counterexample (show (fanLimit, fanoutWanted)) $
            conjoin
              [ counterexample
                  "root is forced immediately, stamped with the exact proposed count and cap"
                  (snapForced w === Just (ForcedFanOut fanoutWanted fanLimit))
              , counterexample "a forced root has no children" (null (snapKids w))
              ]

prop_capsForceLocalFinish :: Property
prop_capsForceLocalFinish = conjoin [prop_depthCap, prop_nodeCap, prop_fanOutCap]

-- ---------------------------------------------------------------------------
-- Runner
-- ---------------------------------------------------------------------------

main :: IO ()
main = do
  results <-
    sequence
      [ run "layer-at-a-time discovery" prop_layerAtATime
      , run "completion-order permutation invariance" prop_permutationInvariance
      , run "failure accumulation" prop_failureAccumulation
      , run "caps forcing local finish" prop_capsForceLocalFinish
      ]
  swarmResults <- mapM (uncurry run) properties
  if and results && and swarmResults then exitSuccess else exitFailure
  where
    run name prop = do
      putStrLn ("--- " <> name <> " ---")
      res <- quickCheckWithResult stdArgs {maxSuccess = 200} prop
      pure (isSuccess res)
