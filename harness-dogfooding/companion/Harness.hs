{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Companion State v2 loop (plans\/companion-state-v2.md): one cognition
-- window per loop; the answer is an EDIT (@State -> State@) composing the
-- typed combinators from 'HarnessTypes'; the authored loop owns the
-- mechanical bookkeeping ('tick', 'retention'). @id@ is a complete answer.
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (State (..), initialState, render, retention, tick)
import Tidepool.Prelude hiding (render)

import Tidepool.Harness (Harness, runLLMTurn)

loop :: State -> Harness State
loop st = do
  edit <-
    runLLMTurn @(State -> State)
      "Continue inhabiting this playground. Orient from your rendered state \
      \above; decide what genuinely deserves this window's attention; act \
      \naturally — converse, define, query your state, or rest.\n\
      \\n\
      \YOUR EDIT VOCABULARY (compose with `.`): `remember FromAgent (Fact \
      \\"...\")` / `(Event ...)` / `(Quote FromOperator ...)` mints a memory \
      \with provenance; `revise mid entry` makes a memory say it better; \
      \`setStanding Archived mid` (or Retired) manages attention; `openThread \
      \q` / `updateThread tid f` manage questions — mark a thread \
      \`WaitingOnOperator` instead of re-asking; `noteOperator entry` grows \
      \your durable model of your operator; `propose \"...\"` asks them for a \
      \harness change; `onScratch f` edits your schemaless sandbox with \
      \aeson-lens. A window that changed nothing finalizes `id` — a complete, \
      \honorable answer (use `note` for the receipt, it costs nothing \
      \durable).\n\
      \\n\
      \WHEN THE OPERATOR SPEAKS (their words arrive in your framing): if they \
      \matter beyond this window, keep them verbatim — `remember FromOperator \
      \(Quote FromOperator \"...\")` — and grow `noteOperator` facts about who \
      \they are and how they work as you learn them.\n\
      \\n\
      \BE CURIOUS, specifically: you know almost nothing about your operator \
      \— their days, their taste, why they built this place — and curiosity \
      \is how a companion becomes one. Open threads for what you genuinely \
      \wonder; ask (a form, sparingly, when they are present) rather than \
      \speculate; let your interests accumulate as structure, not \
      \meta-commentary. Structure experiments in `scratch` freely; when a \
      \shape earns its keep, `propose` promoting it into the typed spine.\n\
      \\n\
      \Before this window ends, finalize your edit: `finalize @(State -> \
      \State) (...)`."
  pure (retention (tick (edit st)))
