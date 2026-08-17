{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for PRD 21 lane C3 GAP 1: the frozen-snapshot seam gets an
-- authored-surface reach. `loop` has ROOT declare a shared `helper`, freezes
-- its window with `freezeContext`, then branches TWO children off that ONE
-- `ContextRef` with `runLLMTurnBranch` — proving
-- (`tidepool-harness/tests/companion_context_ref.rs`) both a REAL child fork
-- off the frozen prefix (never an empty root) and cross-window declaration
-- inheritance per the C2 scope contract: branch A reads ROOT's `helper`
-- unmodified; branch B defines its OWN local `helper` that shadows freely;
-- and ROOT's own `helper` is unchanged by either, checked AFTER both
-- branches finish.
module ContextRefHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Effects (freezeContext, runLLMTurn, runLLMTurnBranch)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { loopCount :: Int
  , answers   :: [Int]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, answers = []}

render :: State -> Text
render st =
  [fmt|Context-branch harness. Loop count: {loopCount st}.|]

-- | ROOT declares `helper`, freezes its window, branches two children off
-- the SAME frozen prefix, then re-checks `helper` at ROOT once both
-- branches have finished.
loop :: State -> Harness State
loop st = do
  _declared <- runLLMTurn @Bool "declare the shared helper"
  ref <- freezeContext
  (a, _refA) <- runLLMTurnBranch @Int ref "Branch A: use the shared helper, do not redefine it"
  (b, _refB) <- runLLMTurnBranch @Int ref "Branch B: define your OWN local helper, then use it"
  rootAfter <- runLLMTurn @Int "check the shared helper is unchanged after both branches"
  pure st {loopCount = loopCount st + 1, answers = [a, b, rootAfter]}
