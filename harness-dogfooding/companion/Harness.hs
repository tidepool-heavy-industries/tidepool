{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Open-ended companion dogfood.
--
-- OODA is the control-loop shape, not a conversational ceremony. 'render'
-- performs deterministic orientation from durable world/self state. The
-- resident answerer observes the operator and its available capabilities,
-- decides what is worth attending to, acts through conversation/forms/forks,
-- and returns the revised orientation for the next loop.
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (State (..), initialState, render)
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.Prelude hiding (render)

loop :: State -> Harness State
loop st = do
  edit <-
    runLLMTurn @(State -> State)
      "Continue inhabiting this playground. Observe what the operator and the \
      \conversation offer; orient using the durable state and world facts above; \
      \decide what genuinely seems worth attending to; then act naturally. You \
      \may share, ask, explore, reflect, fork another perspective, or leave a \
      \thread resting. Do not force an interaction merely to exercise a verb. \
      \Before this cognition window ends, finalize an EDIT of your state — a \
      \pure function `State -> State` (record-update syntax reads well: \
      \\\st -> st { memories = memories st <> [...] }). Untouched fields flow \
      \through unchanged by construction, so nothing you leave alone can be \
      \lost; touch only what this window genuinely changed. A standing habit \
      \you want future windows to keep belongs in a NAMED helper: top-level \
      \pure declarations you define persist across your cognition windows \
      \(your growing library — reviewable source, surviving even machine \
      \maintenance), while one-off lambdas live and die with their window. \
      \When the structure you wish existed is not expressible as a pure \
      \helper, say so — that is the co-design this playground is for."
  pure (edit st)
