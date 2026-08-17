{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for PRD 20 S1-L4 ("concurrent cognition windows") and
-- PRD 21 locked decision 6 (a branch's abnormal exit folds as DATA at its
-- own position): a harness whose 'loop' opens ONE 'runLLMTurnFanout' hole
-- carrying NINE prompts. `runLLMTurnFanout` is generated onto
-- `Tidepool.Effects` whenever `RunLLMTurn` is in the compiling row
-- (row-polymorphic helpers), so it is reachable from the OUTER session
-- unmodified — this fixture is what proves the driver services that
-- fanout's children CONCURRENTLY, each in its own freshly-minted answerer
-- realm, rather than one at a time.
--
-- Nine prompts (not two) so ONE fixture/compile serves every assertion in
-- `tests/outer_fanout.rs`: the order-insensitivity one (which only inspects
-- two of the nine answers), the concurrency-cap one (which needs a fan
-- wider than the default cap of 8), and the typed-exit one (which starves
-- ONE child of rounds and checks its siblings still arrive).
--
-- `runLLMTurnFanout @Int` answers @[Either InvocationExit Int]@ — one result
-- per prompt, in DECLARED order, each position independently either an
-- answer or the typed reason that window ended without one. The fixture
-- keeps BOTH projections in its state so a test can assert on either: the
-- successful answers alone, and the per-position outcome including the
-- failures.
module ConcurrentFanoutHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Effects (InvocationExit, renderInvocationExit, runLLMTurnFanout)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { loopCount :: Int
  , answers   :: [Int]
  , outcomes  :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, answers = [], outcomes = []}

render :: State -> Text
render st =
  [fmt|Concurrent fanout harness. Loop count: {loopCount st}.|]

-- | ONE fanout of nine prompts, answered CONCURRENTLY by the driver (PRD 20
-- S1-L4) — each in its own freshly-minted answerer realm, up to the
-- driver's concurrency cap.
loop :: State -> Harness State
loop st = do
  es <-
    runLLMTurnFanout @Int
      [ "FANOUT-0", "FANOUT-1", "FANOUT-2", "FANOUT-3", "FANOUT-4"
      , "FANOUT-5", "FANOUT-6", "FANOUT-7", "FANOUT-8"
      ]
  pure st { loopCount = loopCount st + 1
          , answers = answered es
          , outcomes = map renderOutcome es
          }

-- | The answers that arrived, in branch order — a window that exited
-- without one contributes nothing here and does NOT displace its siblings.
answered :: [Either InvocationExit Int] -> [Int]
answered = foldr keep []
  where
    keep (Right n) acc = n : acc
    keep (Left _)  acc = acc

-- | One line per BRANCH POSITION, so a failure is legible where it happened
-- rather than as a hole in the answer list.
renderOutcome :: Either InvocationExit Int -> Text
renderOutcome (Right n) = "ok:" <> show n
renderOutcome (Left e)  = "exit:" <> renderInvocationExit e
