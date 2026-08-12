{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Fixture for the fn-finalize feasibility spike
-- (@tidepool-harness\/tests\/selfharness_fn_finalize_spike.rs@). Same
-- author-facing contract as @examples\/harness\/Harness.hs@, but @loop@ asks
-- for a whole @State -> State@ EDIT function via @runLLMTurn@
-- instead of a plain data 'Decision' — the shape the spike is testing:
-- does a `finalize`d closure cross in-heap into the outer loop's suspended
-- continuation and get APPLIED there.
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (State (..), initialState, render)
import Tidepool.Prelude hiding (render)

-- The self-iterating harness's own orchestration monad and its typed yield.
import Tidepool.Harness (Harness, runLLMTurn)

-- | @loop :: State -> Harness State@. LOCKED signature. Ask the
-- calling model for a whole @State -> State@ EDIT (suspending
-- @loop@ as a hole the driver services by handing it to a nested Agent turn
-- loop that answers via @finalize@), then APPLY it to the incoming state —
-- an edit, not a replace, so untouched fields flow through by construction.
loop :: State -> Harness State
loop st = do
  f <-
    runLLMTurn @(State -> State)
      "Revise the state for this cycle: bump the counter and append a note."
  pure (f st)
