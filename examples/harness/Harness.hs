{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | REFERENCE ARTIFACT (self-iterating-harness S3 scaffold,
-- 07-impl-orchestration.md). This is the TARGET authored-harness contract —
-- an author writes a module shaped exactly like this one to drive the
-- self-iterating harness (`tidepool-selfharness`). Loaded at RUNTIME by
-- 'tidepool_harness::load_harness_source' (WS-D), NOT compiled by cargo —
-- it references 'Harness'/'runLLMTurn' (the 'RunLLMTurn' effect WS-B adds).
--
-- Contract this freezes (see @plans/self-iterating-harness/02-runtime.md@,
-- @03-agent-surface.md@, and the runtime-context refactor recorded in
-- @plans/self-iterating-harness/15-generic-surface-wave.md@):
--
--   * 'State' — any author-defined @(ToJSON s, FromJSON s) => s@. LOCKED.
--     Small + typed ("bag of typed values": enums, levels, tag lists,
--     per-loop notes, prior typed answers) — not an unbounded log; whether it
--     accumulates history is an author choice. Carries no loop-iteration
--     counter — that is a runtime fact, tracked in the checkpoint envelope.
--   * 'render' @:: State -> Text@ — LOCKED signature. Invoked by the runtime
--     at loop boundaries ONLY, never per-turn. Domain policy only: the
--     driver composes this output with the prior compaction summary, the
--     loop-iteration count, and capability/finalization instructions into
--     the full system message.
--   * 'loop' @:: State -> Harness State@ — LOCKED signature. One context
--     window's worth of orchestration, ending at a compaction boundary; the
--     returned 'State' IS the durable memory carried into the next loop.
--
-- The typed yield is the point: 'loop' asks for a 'Decision' (an ADT defined
-- in 'HarnessTypes'), NOT a 'Text' — proving the shared RunLLMTurn/finalize
-- machinery (WS-B) carries any monomorphic @FromJSON a => a@ answer back
-- through GHC-as-validator, so authors thread typed values, not strings.
--
-- 'State'\/'Mode'\/'Decision'\/'Confidence'\/'initialState'\/'render' live in
-- 'HarnessTypes', a sibling module with NO reference to 'Tidepool.Harness'\/
-- @runLLMTurn@ (see that module's haddock for why: the self-iterating
-- harness's nested answerer imports the answer types WITHOUT pulling in
-- 'loop' and its @RunLLMTurn@ dependency — the harness\/agent structural
-- split). This module re-exports them unchanged, so the OUTER harness
-- compile (@Eff '[RunLLMTurn]@, which DOES need 'loop') sees the identical
-- contract it always has under the single name @Harness@.
module Harness
  ( State (..)
  , Mode (..)
  , Decision (..)
  , Confidence (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (Confidence (..), Decision (..), Mode (..), State (..),
                      initialState, render)
-- `render` comes from `HarnessTypes` above; `Tidepool.Prelude` also exports
-- an unrelated `render` (`Tidepool.Render`) — hidden to avoid the clash.
import Tidepool.Prelude hiding (render)

-- The self-iterating harness's own orchestration monad ('Eff \'[RunLLMTurn]'
-- for v1, per 07-impl-orchestration.md's locked decisions) and its typed
-- yield.
import Tidepool.Harness (Harness, runLLMTurn)

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
      , notes = take 5 (action d : notes st)
      , lastDecision = Just d
      }

nextMode :: Mode -> Mode
nextMode Observing = Deciding
nextMode Deciding = Acting
nextMode Acting = Observing
