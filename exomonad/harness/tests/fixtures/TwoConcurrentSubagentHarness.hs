{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for CONCURRENT outer-row Subagent servicing: a harness
-- whose 'loop' starts TWO 'spawnAgent' calls as green threads (via
-- 'Tidepool.Async.async') BEFORE either is 'wait'-ed — both reach their own
-- @Subagent@ suspension at the same logical moment, the exact shape
-- @CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md@ names as the blocked workload.
-- Sibling to 'SubagentHarness' (the single-spawn case); this fixture is what
-- proves the driver services two ready @Subagent@ requests concurrently
-- rather than one fully to completion before even looking at the other.
module TwoConcurrentSubagentHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Agent.Spawn (renderSpawnError, spawnAgent)
import Tidepool.Async (async, wait)
import Tidepool.Effects (spawnSpec)
import Tidepool.Prelude hiding (render)
import Tidepool.Worktree (fromCurrentRepository)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

-- | The curator's typed result — same shape as 'SubagentHarness's.
data CuratorReceipt = CuratorReceipt
  { digest :: Text
  , summary :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

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
  [fmt|Two-concurrent subagent harness. Runs: {runs st}. A: {digestA st}. B: {digestB st}|]

-- | TWO coupled spawns, both started as green threads before either is
-- awaited — no model holes: each typed receipt (or rendered spawn failure)
-- lands in durable state for the test to assert on.
loop :: State -> Harness State
loop st = do
  ha <-
    async
      ( spawnAgent @CuratorReceipt
          (spawnSpec (fromCurrentRepository "memory") "curator-a" "regenerate digest a")
      )
  hb <-
    async
      ( spawnAgent @CuratorReceipt
          (spawnSpec (fromCurrentRepository "memory") "curator-b" "regenerate digest b")
      )
  ra <- wait ha
  rb <- wait hb
  pure (finish (recordB rb (recordA ra st)))
  where
    recordA result st0 = case result of
      Right (_outcome, receipt) -> st0 {digestA = receipt.digest}
      Left e -> st0 {lastError = st0.lastError <> renderSpawnError e}
    recordB result st0 = case result of
      Right (_outcome, receipt) -> st0 {digestB = receipt.digest}
      Left e -> st0 {lastError = st0.lastError <> renderSpawnError e}
    finish st0 = st0 {runs = st0.runs + 1}
