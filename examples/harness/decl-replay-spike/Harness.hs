{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Fixture for the driver-level decl-plane replay regression
-- (@tidepool-harness\/tests\/selfharness_decl_plane_replay.rs@): THREE
-- sequential @runLLMTurn \@(State -> State)@ windows in ONE loop, so a
-- single cycle exercises three distinct answerer holes on the SAME
-- per-loop answerer node — the shape needed to pin "a declaration made in
-- an earlier hole of this cycle is visible to a later one" at the driver
-- level (not just within one hole's own retry rounds).
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (State (..), initialState, render)
import Tidepool.Prelude hiding (render)

import Tidepool.Harness (Harness, runLLMTurn)

loop :: State -> Harness State
loop st = do
  f1 <- runLLMTurn @(State -> State) "Window 1: declare a helper on the shared plane."
  st1 <- pure (f1 st)
  f2 <- runLLMTurn @(State -> State) "Window 2: an unrelated edit."
  st2 <- pure (f2 st1)
  f3 <- runLLMTurn @(State -> State) "Window 3: finalize an edit built from the window-1 helper."
  pure (f3 st2)
