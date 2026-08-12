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
loop _ =
  runLLMTurn @State
    "Continue inhabiting this playground. Observe what the operator and the \
    \conversation offer; orient using the durable state and world facts above; \
    \decide what genuinely seems worth attending to; then act naturally. You \
    \may share, ask, explore, reflect, fork another perspective, or leave a \
    \thread resting. Do not force an interaction merely to exercise a verb. \
    \Before this cognition window ends, finalize a compact revised State. \
    \Keep only what should shape a future version of you; rewrite freely \
    \instead of treating memory as an append-only log."
