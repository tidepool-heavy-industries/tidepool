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

-- | A witness the test can walk: the driven 'NodeResult' plus (for every
-- non-leaf) its already-folded children, so properties can inspect shape
-- without re-deriving it from 'NodeResult' alone (which does not keep
-- children once folded — that is the runtime's business, not this fixture's).
data Folded = Folded {_wResult :: NodeResult (), wDepth :: Int, _wChildren :: [Folded]}

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
      Finish (Draft t (BudgetForced r) _) -> NodeSucceeded (ModelContribution t []) [] (NodeReceipt d (Just r))
      Finish (Draft t ModelFinished _) -> NodeSucceeded (ModelContribution t []) [] (NodeReceipt d Nothing)
      other -> NodeSucceeded (ModelContribution (synthesize other) []) [] (NodeReceipt d Nothing)

synthesize :: ThoughtF Folded -> Text
synthesize = \case
  Explore f _ _ -> "explored: " <> f
  Compare dTxt _ _ -> "compared: " <> dTxt
  Challenge c _ _ -> "challenged: " <> c
  Finish _ -> ""

-- | The comparable projection of a 'Folded': everything except the runtime
-- machinery ('Artifact's carry functions, which have no meaningful 'Eq').
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
    { snapView = case res of NodeSucceeded c _ _ -> Just (contributionView c); NodeFailed {} -> Nothing
    , snapFailed = case res of NodeFailed f _ -> Just (failureReason f); NodeSucceeded {} -> Nothing
    , snapDepth = d
    , snapForced = receipt' res
    , snapKids = map snapshot kids
    }
  where
    receipt' (NodeSucceeded _ _ r) = receiptForced r
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

-- | Nodes whose own layer was genuinely decided by the base coalgebra —
-- i.e. everything except a node the node-count cap forced. What
-- 'nodeCapped' actually bounds is how many nodes may EXPAND (spend a real
-- coalgebra invocation), mirroring 'Tidepool.Swarm.budgeted''s cycle-cost
-- reading in "Harness.hs" — not the final tree's total size, which the
-- last permitted expansion's own fan-out can still grow past the limit.
countExpandable :: Snapshot -> Int
countExpandable s = (if snapForced s == Just ForcedNodeCount then 0 else 1) + sum (map countExpandable (snapKids s))

allLeavesAtDepthForced :: Int -> Snapshot -> Bool
allLeavesAtDepthForced limit s
  | snapDepth s == limit = snapForced s == Just ForcedDepth && null (snapKids s)
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
              [ counterexample "root is forced immediately (its own fan-out already exceeds the cap)" (snapForced w === Just ForcedFanOut)
              , counterexample "a forced root has no children" (null (snapKids w))
              ]

prop_capsForceLocalFinish :: Property
prop_capsForceLocalFinish = conjoin [prop_depthCap, prop_nodeCap, prop_fanOutCap]

-- ---------------------------------------------------------------------------
-- PRD 21 lane C4 — checked edits. Four more families over the same pure
-- driver: selection-order composition, failure isolation, receipt
-- completeness, and 'Narrative' never approving. A fifth, run through
-- 'thoughtHylo' itself rather than hand-assembled: a child proposes an edit
-- artifact, the parent's algebra selects and composes a subset, and
-- 'applyEdits' runs them against a known snapshot and stamps one receipt
-- per plan — "child proposes, parent selects, runtime applies and stamps",
-- through the real recursive driver.
-- ---------------------------------------------------------------------------

tshow :: Show a => a -> Text
tshow = T.pack . show

artifactIdOf :: Int -> ArtifactId
artifactIdOf i = ArtifactId ("a" <> tshow i)

-- | A pool of edit plans, one per id: each appends its own numbered tag to a
-- 'Text' snapshot, except ids in 'failing', which always refuse — distinct
-- tags make application ORDER legible directly in the composed output.
mkPool :: [Int] -> [Int] -> [Artifact Text]
mkPool ids failing =
  [EditArtifact (artifactIdOf i) (EditIntent ("tag " <> tshow i)) (mkApply i) | i <- ids]
  where
    mkApply i
      | i `elem` failing = const (Left (EditFailure ("refused " <> tshow i)))
      | otherwise = \s -> Right (s <> tshow i)

-- | A decision that selects exactly 'order', composed in exactly that order.
mkDecision :: [Int] -> FoldDecision Text
mkDecision order =
  FoldDecision
    { synthesis = "unused"
    , selected = map artifactIdOf order
    , composition = CompositionOrder (map artifactIdOf order)
    , decisionProposed = []
    }

isLeftOutcome :: Either a b -> Bool
isLeftOutcome e = case e of Left _ -> True; Right _ -> False

-- | Given ANY nonempty subset of ids, in ANY order, the composed result
-- reflects exactly that order — not the pool's declaration order, and not
-- whatever order a naive selection-list traversal might have produced.
prop_selectionOrderComposition :: Property
prop_selectionOrderComposition =
  forAll (choose (2, 6)) $ \n ->
    forAll (sublistOf [1 .. n] `suchThat` (not . null)) $ \chosen0 ->
      forAll (shuffle chosen0) $ \chosen ->
        let pool = mkPool [1 .. n] []
         in case approve (resolveSelection (mkDecision chosen) pool) of
              Nothing -> counterexample "expected ProposedEdits, got Narrative" False
              Just approved ->
                let (final, receipts) = applyEdits id approved ""
                    expected = T.concat (map tshow chosen)
                 in conjoin
                      [ counterexample "final snapshot reflects exactly the chosen ids, in the decision's own order" (final === expected)
                      , counterexample "one receipt per approved plan" (NE.length receipts === length chosen)
                      , counterexample "receipt ids follow the same order" (map receiptArtifact (NE.toList receipts) === map artifactIdOf chosen)
                      ]

-- | A failing artifact never erases a sibling's outcome: every plan in the
-- selection still gets a receipt, the successes still compose correctly
-- (skipping the failures, not poisoned by them), and every failure is its
-- own isolated 'Left'.
prop_failureIsolation :: Property
prop_failureIsolation =
  forAll (choose (2, 6)) $ \n ->
    forAll (sublistOf [1 .. n] `suchThat` (not . null)) $ \failing ->
      let pool = mkPool [1 .. n] failing
       in case approve (resolveSelection (mkDecision [1 .. n]) pool) of
            Nothing -> counterexample "expected ProposedEdits" False
            Just approved ->
              let (final, receipts) = applyEdits id approved ""
                  succeeding = [i | i <- [1 .. n], i `notElem` failing]
                  receiptFor i = receipts NE.!! (i - 1)
               in conjoin
                    [ counterexample "final snapshot reflects only the succeeding edits, in order" (final === T.concat (map tshow succeeding))
                    , counterexample "every plan still produced a receipt, failing or not" (NE.length receipts === n)
                    , counterexample "a failing id's own receipt is Left" (conjoin [counterexample (show i) (isLeftOutcome (receiptOutcome (receiptFor i))) | i <- failing])
                    , counterexample "a succeeding id's own receipt is Right" (conjoin [counterexample (show i) (not (isLeftOutcome (receiptOutcome (receiptFor i)))) | i <- succeeding])
                    ]

-- | Receipt completeness: for ANY mix of pass/fail over ANY selection,
-- there is exactly one receipt per approved plan, in the same order —
-- never fewer (a failure dropped from the list) and never more.
prop_receiptCompleteness :: Property
prop_receiptCompleteness =
  forAll (choose (1, 8)) $ \n ->
    forAll (sublistOf [1 .. n]) $ \failing ->
      let pool = mkPool [1 .. n] failing
       in case approve (resolveSelection (mkDecision [1 .. n]) pool) of
            Nothing -> counterexample "expected ProposedEdits" False
            Just approved ->
              let (_, receipts) = applyEdits id approved ""
               in counterexample "exactly one receipt per approved plan, same order" (map receiptArtifact (NE.toList receipts) === map artifactIdOf [1 .. n])

-- | 'Narrative' can never mint 'ApprovedEdits' — the structural guarantee
-- open question 4 asks for, checked as a property rather than one
-- hand-picked value.
prop_narrativeNeverApproves :: Property
prop_narrativeNeverApproves =
  forAll genText $ \t -> case approve (Narrative t :: FoldProduct ()) of
    Nothing -> property True
    Just _ -> counterexample "a Narrative value minted ApprovedEdits" False

-- | End to end, through 'thoughtHylo' itself: an Explore layer of N leaf
-- children, each of whose fold proposes exactly one edit artifact tagged
-- with its own id; the root's algebra gathers every child's artifact into a
-- pool, builds a 'FoldDecision' that selects and REVERSES them (so the
-- result can only match if composition order, not arrival/declared order,
-- governed the apply), resolves, approves, and applies.
data E2ENode = E2ENode {e2eText :: Text, e2eArtifacts :: [Artifact Text], e2eReceipts :: [EditReceipt]}

e2eCoalg :: Int -> Seed -> ThoughtF Seed
e2eCoalg n (Seed d s) = case s of
  SFinish nid _ | d > 0 -> Finish (Draft (tshow nid) ModelFinished d)
  _ ->
    Explore
      "root"
      (NE.fromList [Branch (ForkBrief ("leaf" <> tshow i) Primary "propose") (Seed (d + 1) (SFinish i "")) | i <- [1 .. n]])
      Sequential

e2eAlg :: Int -> ThoughtF E2ENode -> Identity E2ENode
e2eAlg n layer = pure $ case layer of
  Finish dr ->
    let nid = draftText dr
        aid = ArtifactId ("a" <> nid)
     in E2ENode "" [EditArtifact aid (EditIntent ("leaf " <> nid)) (\s -> Right (s <> nid))] []
  _ ->
    let kids = foldr (:) [] layer
        pool = concatMap e2eArtifacts kids
        chosen = reverse [1 .. n]
     in case approve (resolveSelection (mkDecision chosen) pool) of
          Nothing -> E2ENode "" pool []
          Just approved ->
            let (final, receipts) = applyEdits id approved ""
             in E2ENode final pool (NE.toList receipts)

prop_endToEndChildProposesParentSelects :: Property
prop_endToEndChildProposesParentSelects =
  forAll (choose (2, 6)) $ \n ->
    let seed0 = Seed 0 (SFinish 0 "")
        result = runIdentity (thoughtHylo (e2eAlg n) (pure . e2eCoalg n) seed0)
        expected = T.concat (map tshow (reverse [1 .. n]))
     in conjoin
          [ counterexample "root text reflects the reversed composition order, not declared branch order" (e2eText result === expected)
          , counterexample "the root gathered every leaf's proposed artifact" (length (e2eArtifacts result) === n)
          , counterexample "one receipt per approved (== every) leaf artifact" (length (e2eReceipts result) === n)
          , counterexample "every receipt succeeded — no failures injected in this slice" (all (not . isLeftOutcome . receiptOutcome) (e2eReceipts result))
          ]

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
      , run "C4 selection-order composition" prop_selectionOrderComposition
      , run "C4 failure isolation" prop_failureIsolation
      , run "C4 receipt completeness" prop_receiptCompleteness
      , run "C4 Narrative never approves" prop_narrativeNeverApproves
      , run "C4 end-to-end: child proposes, parent selects, runtime applies" prop_endToEndChildProposesParentSelects
      ]
  swarmResults <- mapM (uncurry run) properties
  if and results && and swarmResults then exitSuccess else exitFailure
  where
    run name prop = do
      putStrLn ("--- " <> name <> " ---")
      res <- quickCheckWithResult stdArgs {maxSuccess = 200} prop
      pure (isSuccess res)
