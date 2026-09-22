{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for 'Tidepool.Swarm.hyloConcurrentM' driven through the real
-- driver: a two-leaf plan tree whose leaves each 'spawnAgent' from their own
-- fold step, unfolded and folded via 'hyloConcurrentM' with
-- 'Tidepool.Async.mapConcurrently' as the concurrent traversal — instead of
-- 'TwoConcurrentSubagentHarness's hand-rolled @async ... ; async ... ; wait
-- ...; wait ...@, this is the actual combinator dev-tree-shaped callers would
-- reach for. Sibling to 'TwoConcurrentSubagentHarness': same overlap
-- receipt, routed through the hylomorphism instead of bare 'Tidepool.Async'.
module HyloConcurrentSubagentHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Agent.Spawn (renderSpawnError, spawnAgent)
import Tidepool.Async (mapConcurrently)
import Tidepool.Effects (spawnSpec)
import Tidepool.Prelude hiding (render)
import Tidepool.Swarm (Alg, Coalg, PlanF (..), hyloConcurrentM)
import Tidepool.Worktree (fromCurrentRepository)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

-- | The curator's typed result — same shape as 'SubagentHarness's.
data CuratorReceipt = CuratorReceipt
  { digest :: Text
  , summary :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

-- | The plan seed: the root splits into exactly two named leaves; a leaf
-- never splits further. Doubles as the 'PlanF' task, so a leaf's own name
-- and brief ride into its fold step unchanged.
data PlanSeed
  = RootSeed
  | LeafSeed Text Text

-- | The folded value: a leaf's own spawn outcome, or the root's combination
-- of its two children's outcomes. One sum type because 'hyloConcurrentM'
-- (like 'hyloM') folds every node — leaf and root alike — to the SAME type.
data FoldResult
  = LeafResult Text Text
  | RootResult Text Text Text

leafDigest :: FoldResult -> Text
leafDigest (LeafResult d _) = d
leafDigest RootResult {} = ""

leafError :: FoldResult -> Text
leafError (LeafResult _ e) = e
leafError RootResult {} = ""

data State = State
  { runs :: Int
  , digestA :: Text
  , digestB :: Text
  , lastError :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {runs = 0, digestA = "", digestB = "", lastError = ""}

render :: State -> Text
render st =
  [fmt|Hylo-concurrent subagent harness. Runs: {runs st}. A: {digestA st}. B: {digestB st}|]

-- | Split the root into its two named leaves; a leaf never splits further.
planCoalg :: Coalg Harness PlanSeed PlanSeed
planCoalg RootSeed =
  pure
    ( PlanF
        RootSeed
        [ LeafSeed "curator-a" "regenerate digest a"
        , LeafSeed "curator-b" "regenerate digest b"
        ]
    )
planCoalg leaf@(LeafSeed _ _) = pure (PlanF leaf [])

-- | A leaf's fold step is where the real work happens: ONE coupled spawn per
-- leaf. The root's fold step only combines its two children's own results,
-- in PLAN order — the property 'hyloConcurrentM' exists to hold.
planAlg :: Alg Harness PlanSeed FoldResult
planAlg (PlanF (LeafSeed name brief) _kids) = do
  r <- spawnAgent @CuratorReceipt (spawnSpec (fromCurrentRepository "memory") name brief)
  pure
    ( case r of
        Right (_outcome, receipt) -> LeafResult receipt.digest ""
        Left e -> LeafResult "" (renderSpawnError e)
    )
planAlg (PlanF RootSeed kids) =
  pure (combine kids)
  where
    combine [a, b] = RootResult (leafDigest a) (leafDigest b) (leafError a <> leafError b)
    combine other = RootResult "" "" ("expected exactly two children, got " <> pack (show (length other)))

-- | Drive the two-leaf plan concurrently: both leaves' 'spawnAgent' calls
-- start as green threads before either is awaited, via
-- 'hyloConcurrentM mapConcurrently' — the combinator itself, not a
-- hand-rolled pair of 'async'/'wait' calls.
loop :: State -> Harness State
loop st = do
  result <- hyloConcurrentM mapConcurrently planAlg planCoalg RootSeed
  pure
    ( case result of
        RootResult da db errs ->
          st {runs = st.runs + 1, digestA = da, digestB = db, lastError = errs}
        LeafResult {} ->
          st {runs = st.runs + 1, lastError = "unexpected leaf-shaped root result"}
    )
