{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for fork-subsumes-split step 3's GUI lane: an ordinary
-- 'runLLMTurn' hole whose answerer forks ONE child ('Tidepool.Answerer.Fork.fork').
-- The child asks the operator one question, then finalizes; the parent
-- resumes (in the SAME compiled block — no extra model round) and finalizes
-- with the child's typed answer. Unlike
-- 'tidepool-harness/tests/fixtures/LabeledBranchHarness.hs''s
-- 'runLLMTurnBranchLabeled' child, nothing on the wire hands this child a
-- label — 'SelfHarnessDriver::fork_child_label' derives one — so this
-- fixture exercises that derived label routing the child's own ask to its
-- own gate and reaching 'node_finalized' at its fold. See
-- 'tidepool-harness/tests/fork_child_gui.rs'.
module ForkChildGuiHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness, runLLMTurn)

data State = State
  { loopCount :: Int
  , lastValue :: Int
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, lastValue = 0}

render :: State -> Text
render st =
  [fmt|Fork-child-gui harness. Loop count: {loopCount st}.|]

loop :: State -> Harness State
loop st = do
  n <- runLLMTurn @Int "Fork off a child to check something, then finalize with its answer."
  pure st {loopCount = loopCount st + 1, lastValue = n}
