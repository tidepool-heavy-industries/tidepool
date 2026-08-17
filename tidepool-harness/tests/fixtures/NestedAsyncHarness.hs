{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | The NESTED-`async` reproducer fixture (PRD 20 S1-L4, chartered gap).
--
-- Deliberately standalone rather than folded into `OuterEffectsHarness`:
-- this is a CRASH-CLASS fixture, and the root `CLAUDE.md` test discipline
-- keeps those out of family bundles precisely because a bundled crash
-- destroys its siblings' diagnosis. Bundling it would take the whole
-- outer-effects bundle down with it.
--
-- The ONE thing that distinguishes this from the (passing) flat case in
-- `OuterEffectsHarness`: 'nestedWork' — a green thread's body — itself calls
-- 'async'. Everything else, down to the strict recursive sum and the
-- differing element lengths, is the same shape that passes there.
module NestedAsyncHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Async (async, mapConcurrently, wait)
import Tidepool.Effects (say)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { runs :: Int
  , nestedResults :: [Int]
  }
  deriving (Generic, Show, Eq, FromJSON, ToJSON)

initialState :: State
initialState = State {runs = 0, nestedResults = []}

render :: State -> Text
render st = [fmt|Nested-async probe. Runs: {runs st}.|]

-- | One `mapConcurrently` element's work — and the reproducer's whole point:
-- this body, which is ALREADY running as a green thread, forks another green
-- thread and waits on it.
--
-- The result is forced with `$!` before it settles, so this is NOT the
-- separate (fixed, documented) settle-boundary thunk bug — see
-- `Tidepool.Async`'s module doc. Differing `n` keeps the threads
-- non-interchangeable, as in the flat case.
nestedWork :: Int -> Harness Int
nestedWork n = do
  inner <- async (pure $! sumTo n * 10)
  v <- wait inner
  pure $! v + 1
  where
    sumTo 0 = 0
    sumTo k = k + sumTo (k - 1)

loop :: State -> Harness State
loop st = do
  say "nested-async probe starting"
  results <- mapConcurrently nestedWork [3, 1, 2]
  pure st {runs = st.runs + 1, nestedResults = results}
