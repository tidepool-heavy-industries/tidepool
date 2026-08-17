{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for PRD 20 S1-L4 ("concurrent cognition windows"): a
-- harness whose 'loop' opens ONE 'runLLMTurnFanout' hole carrying NINE
-- prompts. `runLLMTurnFanout` is generated onto `Tidepool.Effects` whenever
-- `RunLLMTurn` is in the compiling row (row-polymorphic helpers), so it is
-- reachable from the OUTER session unmodified — this fixture is what proves
-- the driver now services that fanout's children CONCURRENTLY, each in its
-- own freshly-minted answerer realm, rather than one at a time.
--
-- Nine prompts (not two) so ONE fixture/compile serves both the
-- order-insensitivity assertion (which only inspects two of the nine
-- answers) and the concurrency-cap assertion (which needs a fan wider than
-- the default cap of 8) — see `tests/outer_fanout.rs`.
module ConcurrentFanoutHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Effects (runLLMTurnFanout)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { loopCount :: Int
  , answers   :: [Int]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, answers = []}

render :: State -> Text
render st =
  [fmt|Concurrent fanout harness. Loop count: {loopCount st}.|]

-- | ONE fanout of nine prompts, answered CONCURRENTLY by the driver (PRD 20
-- S1-L4) — each in its own freshly-minted answerer realm, up to the
-- driver's concurrency cap.
loop :: State -> Harness State
loop st = do
  ns <-
    runLLMTurnFanout @Int
      [ "FANOUT-0", "FANOUT-1", "FANOUT-2", "FANOUT-3", "FANOUT-4"
      , "FANOUT-5", "FANOUT-6", "FANOUT-7", "FANOUT-8"
      ]
  pure st {loopCount = loopCount st + 1, answers = ns}
