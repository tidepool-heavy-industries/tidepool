{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | REFERENCE ARTIFACT (self-iterating-harness S3 scaffold,
-- 07-impl-orchestration.md). This is the TARGET authored-harness contract —
-- an author writes a module shaped exactly like this one to drive the
-- self-iterating harness (`tidepool-selfharness`). Loaded at RUNTIME by
-- 'tidepool_harness::load_harness_source' (WS-D), NOT compiled by cargo —
-- it references 'Harness'/'runLLMTurn' (the 'RunLLMTurn' effect WS-B adds)
-- and so does not compile until WS-B's Haskell effects land.
--
-- Contract this freezes (see @plans/self-iterating-harness/02-runtime.md@,
-- @03-agent-surface.md@):
--
--   * 'State' — any author-defined @(ToJSON s, FromJSON s) => s@. LOCKED.
--     Small + typed ("bag of typed values": enums, levels, tag lists,
--     per-loop notes) — not an unbounded log; whether it accumulates
--     history is an author choice.
--   * 'render' @:: State -> Maybe Text -> Text@ — LOCKED signature. Invoked
--     by the runtime at loop boundaries ONLY, never per-turn (the 'Maybe
--     Text' is the prior loop's compaction summary, 'Nothing' only before
--     the first compaction).
--   * 'loop' @:: State -> Harness State@ — LOCKED signature. One context
--     window's worth of orchestration, ending at a compaction boundary; the
--     returned 'State' IS the durable memory carried into the next loop.
module Harness (State (..), Mode (..), initialState, render, loop) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude
import Tidepool.QQ (fmt)

-- The self-iterating harness's own orchestration monad ('Eff \'[RunLLMTurn]'
-- for v1, per 07-impl-orchestration.md's locked decisions) and its typed
-- yield. Neither exists yet — WS-B adds them; this reference compiles once
-- that effect lands.
import Tidepool.Harness (Harness, runLLMTurn)

-- | The author-defined 'State' this harness threads through 'loop' and
-- reads in 'render'. A single-constructor record, per the
-- @deriving (Generic, ToJSON, FromJSON)@ convention used throughout
-- @haskell/lib/Tidepool@ (structural, no Template Haskell).
data State = State
  { mode      :: Mode
  , loopCount :: Int
  , notes     :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | An example small, typed State field — the "enum/level/mode" shape
-- 02-runtime.md calls out, not a free-form blob.
data Mode = Observing | Deciding | Acting
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The runtime's very first loop starts from this 'State' (before any
-- persisted State exists to restore).
initialState :: State
initialState = State {mode = Observing, loopCount = 0, notes = []}

-- | @render :: State -> Maybe Text -> Text@. LOCKED signature. Plain
-- Haskell conditionals + the @[fmt|]@ quasiquoter over 'State' — no jinja,
-- no effects: this function cannot itself suspend or call 'runLLMTurn'
-- (that's what makes per-turn re-rendering unrepresentable by construction).
render :: State -> Maybe Text -> Text
render st lastCompaction =
  [fmt|You are a self-iterating agent, currently {modeLine}.
Loop count so far: {loopCount st}.
{notesBlock}
{compactionBlock}|]
  where
    modeLine = case mode st of
      Observing -> "observing" :: Text
      Deciding -> "deciding what to do next"
      Acting -> "acting on a prior decision"
    notesBlock
      | null (notes st) = "No notes carried forward yet."
      | otherwise =
          "Notes carried forward:\n"
            <> T.intercalate "\n" (map ("- " <>) (notes st))
    compactionBlock = case lastCompaction of
      Nothing -> ""
      Just summary -> "Summary of the prior window:\n" <> summary

-- | @loop :: State -> Harness State@. LOCKED signature. One context
-- window's worth of work: ask the calling model one typed question via
-- 'runLLMTurn' (suspending 'loop' as a hole the self-harness driver
-- services — WS-A — by handing it to a nested Agent turn loop that answers
-- via @finalize@, WS-B), fold the typed answer into a fresh 'State', and
-- return it as this loop's durable memory.
loop :: State -> Harness State
loop st = do
  decision <-
    runLLMTurn @Text
      "Given the current state, decide the single next thing to do. \
      \Reply with one short sentence."
  pure
    st
      { mode = nextMode (mode st)
      , loopCount = loopCount st + 1
      , notes = take 5 (decision : notes st)
      }

nextMode :: Mode -> Mode
nextMode Observing = Deciding
nextMode Deciding = Acting
nextMode Acting = Observing
