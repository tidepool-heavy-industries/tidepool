{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for PRD 21 lane C5's GUI lane: a single
-- 'runLLMTurnBranchLabeled' child, labeled structurally (never parsed out of
-- the prompt). Its answerer window asks the operator ONE 'askUser' question,
-- then — since this fixture's script never finalizes it — exhausts its round
-- budget, so the outcome is always the typed 'InvocationExit' folded at the
-- branch position (PRD 21 locked decision 6). This is enough to exercise the
-- label's routing (the operator gate a child's asks/notes reach) and its
-- retirement (marked done once the window folds) without needing the
-- answer-type site to resolve — see
-- `tidepool-harness/tests/labeled_branch.rs`.
module LabeledBranchHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Effects
  ( InvocationExit
  , freezeContext
  , renderInvocationExit
  , runLLMTurnBranchLabeled
  )
import Tidepool.Form (askUser)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { loopCount :: Int
  , outcome   :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, outcome = ""}

render :: State -> Text
render st =
  [fmt|Labeled-branch harness. Loop count: {loopCount st}.|]

-- | Freeze ROOT's window immediately, then branch ONE labeled child off it.
loop :: State -> Harness State
loop st = do
  ref <- freezeContext
  result <-
    runLLMTurnBranchLabeled
      @Int
      "root/1-child"
      ref
      "Branch: ask the operator something, then keep going"
  pure st { loopCount = loopCount st + 1, outcome = renderOutcome result }

renderOutcome :: Either InvocationExit (Int, a) -> Text
renderOutcome (Right (n, _)) = "ok:" <> show n
renderOutcome (Left e)       = "exit:" <> renderInvocationExit e
