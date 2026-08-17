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
--
-- It ALSO carries PRD 21 locked decision 6 for this verb: a branch child is a
-- BRANCH POSITION, so `runLLMTurnBranch @T` answers
-- @Either InvocationExit (T, ContextRef)@ and a window that exits without an
-- answer folds as data at its own position instead of aborting the turn. The
-- state keeps both projections — `answers` (what arrived) and `outcomes` (one
-- entry per branch position) — so one fixture serves both the all-success
-- scenario and the starved-branch one.
module ContextRefHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Effects
  ( ContextRef
  , InvocationExit
  , freezeContext
  , renderInvocationExit
  , runLLMTurn
  , runLLMTurnBranch
  )
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { loopCount :: Int
  , answers   :: [Int]
  , outcomes  :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, answers = [], outcomes = []}

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
  -- NOT `Right (a, _) <- ...`: a refutable bind would turn a branch exit back
  -- into an abort, which is exactly what decision 6 forbids. Each branch's
  -- outcome is kept AT ITS OWN POSITION and projected below.
  eA <- runLLMTurnBranch @Int ref "Branch A: use the shared helper, do not redefine it"
  eB <- runLLMTurnBranch @Int ref "Branch B: define your OWN local helper, then use it"
  rootAfter <- runLLMTurn @Int "check the shared helper is unchanged after both branches"
  pure st { loopCount = loopCount st + 1
          , answers = answered [eA, eB] ++ [rootAfter]
          , outcomes = map renderOutcome [eA, eB]
          }

-- | The branch answers that arrived, in branch order — a window that exited
-- without one contributes nothing and does NOT displace its sibling.
answered :: [Either InvocationExit (Int, ContextRef)] -> [Int]
answered = foldr keep []
  where
    keep (Right (n, _)) acc = n : acc
    keep (Left _)       acc = acc

-- | One line per BRANCH POSITION, so a failure is legible where it happened.
renderOutcome :: Either InvocationExit (Int, ContextRef) -> Text
renderOutcome (Right (n, _)) = "ok:" <> show n
renderOutcome (Left e)       = "exit:" <> renderInvocationExit e
