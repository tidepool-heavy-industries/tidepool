{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Fixture for the OODA-pipeline acceptance
-- (@tidepool-harness\/tests\/selfharness_fn_finalize_spike.rs@,
-- @ooda_pipeline_conditional_phases@): the typed phase vocabulary of an
-- observe\/orient\/decide\/act loop. 'Tempo' is Boyd's hinge — 'Familiar'
-- goes straight to the act window (implicit guidance and control),
-- 'Deliberate' inserts a decide window, 'Quiet' ends the loop with no act at
-- all. 'Move' is the GTD-triage sum. 'Familiar'\/'Deliberate' carry
-- POSITIONAL payloads on purpose: this fixture also exercises the generic
-- JSON TaggedObject @contents@ form for positional sum payloads end to end
-- through the typed finalize crossing.
module HarnessTypes
  ( State (..)
  , Orientation (..)
  , Tempo (..)
  , Move (..)
  , initialState
  , render
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)

data State = State
  { counter :: Int
  , lastExpectation :: Maybe Text
  -- ^ Boyd's feedback wire: 'Engage'\'s hypothesis, stamped by the loop and
  -- shown to the NEXT loop's orient window, then cleared unless renewed.
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {counter = 0, lastExpectation = Nothing}

data Tempo
  = Familiar Move
  | Deliberate [Text]
  | Quiet
  deriving (Generic, ToJSON, FromJSON, Show)

data Orientation = Orientation
  { reading :: Text
  , tempo :: Tempo
  }
  deriving (Generic, ToJSON, FromJSON, Show)

data Move
  = Engage {intent :: Text, expecting :: Text}
  | AskFirst {question :: Text}
  | Shelve {what :: Text, revisit :: Text}
  | LetGo {what :: Text}
  deriving (Generic, ToJSON, FromJSON, Show)

render :: State -> Text
render st = "counter: " <> show st.counter <> expectationLine
  where
    expectationLine = case st.lastExpectation of
      Nothing -> ""
      Just e -> "\nLast loop you expected: " <> e
