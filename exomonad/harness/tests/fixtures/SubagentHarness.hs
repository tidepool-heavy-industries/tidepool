{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for the outer-row Subagent seam: a harness whose 'loop'
-- calls the typed @spawnAgent \@CuratorReceipt@ DIRECTLY (no model round at
-- all — the loop is authored orchestration), so the driver's
-- suspension-servicing path is the only thing under test. The receipt's
-- fields crossing into 'State' proves the whole round trip: authored loop →
-- Subagent suspension → driver-owned handler → typed payload decode →
-- resumed continuation.
--
-- The @lastError@ FIELD NAME is deliberate and load-bearing: it collides
-- with GHC.List's bottoming worker of the same occ name, which the
-- extract's `isErrorVar` recognizer used to match by BARE NAME — tagging
-- the selector as an error sentinel and compiling this module's record
-- update into a runtime raise (found live 2026-08-14). The recognizer is
-- now module-qualified (`Translate.hs`); this fixture passing IS the
-- regression pin.
module SubagentHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Agent.Spawn (renderSpawnError, spawnAgent)
import Tidepool.Effects (spawnSpec)
import Tidepool.Prelude hiding (render)
import Tidepool.Worktree (fromCurrentRepository)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

-- | The curator's typed result — the memory-plan 'MemReceipt' shape: a
-- single-constructor record (spawnAgent's schema rule).
data CuratorReceipt = CuratorReceipt
  { digest :: Text
  , summary :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

data State = State
  { runs :: Int
  , lastDigest :: Text
  , lastError :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {runs = 0, lastDigest = "", lastError = ""}

render :: State -> Text
render st =
  [fmt|Subagent harness. Runs: {runs st}. Digest: {lastDigest st}|]

-- | ONE coupled spawn per loop, no model holes: the typed receipt (or the
-- rendered spawn failure) lands in durable state for the test to assert on.
loop :: State -> Harness State
loop st = do
  r <-
    spawnAgent @CuratorReceipt
      (spawnSpec (fromCurrentRepository "memory") "curator" "regenerate the digest")
  pure
    ( case r of
        Right (_outcome, receipt) ->
          st {runs = st.runs + 1, lastDigest = receipt.digest}
        Left e ->
          st {runs = st.runs + 1, lastError = renderSpawnError e}
    )
