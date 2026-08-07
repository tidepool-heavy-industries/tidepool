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
--     per-loop notes, prior typed answers) — not an unbounded log; whether it
--     accumulates history is an author choice.
--   * 'render' @:: State -> Maybe Text -> Text@ — LOCKED signature. Invoked
--     by the runtime at loop boundaries ONLY, never per-turn (the 'Maybe
--     Text' is the prior loop's compaction summary, 'Nothing' only before
--     the first compaction).
--   * 'loop' @:: State -> Harness State@ — LOCKED signature. One context
--     window's worth of orchestration, ending at a compaction boundary; the
--     returned 'State' IS the durable memory carried into the next loop.
--
-- The typed yield is the point: 'loop' asks for a 'Decision' (an ADT defined
-- right here), NOT a 'Text' — proving the shared RunLLMTurn/finalize
-- machinery (WS-B) carries any monomorphic @FromJSON a => a@ answer back
-- through GHC-as-validator, so authors thread typed values, not strings.
module Harness
  ( State (..)
  , Mode (..)
  , Decision (..)
  , Confidence (..)
  , initialState
  , render
  , loop
  ) where

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

-- | The author-defined 'State' this harness threads through 'loop' and reads
-- in 'render'. A single-constructor record, per the
-- @deriving (Generic, ToJSON, FromJSON)@ convention used throughout
-- @haskell/lib/Tidepool@ (structural, no Template Haskell). Note it stores a
-- typed 'Decision' ('lastDecision'), so a typed answer flows
-- @runLLMTurn -> State -> (serialized across the loop boundary) -> render@.
data State = State
  { mode         :: Mode
  , loopCount    :: Int
  , notes        :: [Text]
  , lastDecision :: Maybe Decision
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | An example small, typed State field — the "enum/level/mode" shape
-- 02-runtime.md calls out, not a free-form blob.
data Mode = Observing | Deciding | Acting
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The typed answer 'loop' asks for via @runLLMTurn \@Decision@ — NOT a bare
-- 'Text'. This is the whole point of the shared-code model: the RunLLMTurn /
-- finalize machinery (WS-B) carries any monomorphic @FromJSON a => a@ answer
-- back through GHC-as-validator (an ill-typed answer never consumes the
-- continuation). Defined here alongside 'State'/'Mode', and it NESTS
-- ('Confidence') to show structured answers cross whole, not just flat.
data Decision = Decision
  { action     :: Text        -- ^ the single next thing to do
  , rationale  :: Text        -- ^ why, in one sentence
  , confidence :: Confidence  -- ^ nested typed field — structured answers nest
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | A nested typed field of 'Decision' — proves an ADT-within-an-ADT answer
-- round-trips through the typed yield.
data Confidence = Low | Medium | High
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The runtime's very first loop starts from this 'State' (before any
-- persisted State exists to restore).
initialState :: State
initialState =
  State {mode = Observing, loopCount = 0, notes = [], lastDecision = Nothing}

-- | @render :: State -> Maybe Text -> Text@. LOCKED signature. Plain Haskell
-- conditionals + the @[fmt|]@ quasiquoter over 'State' — no jinja, no effects:
-- this function cannot itself suspend or call 'runLLMTurn' (that's what makes
-- per-turn re-rendering unrepresentable by construction).
render :: State -> Maybe Text -> Text
render st lastCompaction =
  [fmt|You are a self-iterating agent, currently {modeLine}.
Loop count so far: {loopCount st}.
{lastDecisionBlock}
{notesBlock}
{compactionBlock}|]
  where
    modeLine = case mode st of
      Observing -> "observing" :: Text
      Deciding -> "deciding what to do next"
      Acting -> "acting on a prior decision"
    lastDecisionBlock = case lastDecision st of
      Nothing -> "No decision made yet." :: Text
      Just d ->
        "Last decision: " <> action d
          <> " (confidence: " <> T.pack (show (confidence d)) <> ")"
    notesBlock
      | null (notes st) = "No notes carried forward yet."
      | otherwise =
          "Notes carried forward:\n"
            <> T.intercalate "\n" (map ("- " <>) (notes st))
    compactionBlock = case lastCompaction of
      Nothing -> ""
      Just summary -> "Summary of the prior window:\n" <> summary

-- | @loop :: State -> Harness State@. LOCKED signature. One context window's
-- work: ask the calling model for a TYPED 'Decision' via @runLLMTurn
-- \@Decision@ (suspending 'loop' as a hole the driver services — WS-A — by
-- handing it to a nested Agent turn loop that answers via @finalize@, WS-B,
-- with GHC validating the answer AT 'Decision'), fold the typed decision into
-- a fresh 'State', and return it as this loop's durable memory.
loop :: State -> Harness State
loop st = do
  d <-
    runLLMTurn @Decision
      "Given the current state, decide the single next thing to do. Give a \
      \one-sentence rationale and your confidence (Low, Medium, or High)."
  pure
    st
      { mode = nextMode (mode st)
      , loopCount = loopCount st + 1
      , notes = take 5 (action d : notes st)
      , lastDecision = Just d
      }

nextMode :: Mode -> Mode
nextMode Observing = Deciding
nextMode Deciding = Acting
nextMode Acting = Observing
