{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Fixture for the fn-finalize feasibility spike
-- (@tidepool-harness\/tests\/selfharness_fn_finalize_spike.rs@): the minimal
-- author-facing 'State'\/'render' half of a self-iterating harness whose
-- @loop@ (the sibling 'Harness' module) finalizes a @State -> State@
-- FUNCTION rather than a plain data value. Same split as
-- @examples\/harness\/HarnessTypes.hs@ — no reference to 'Tidepool.Harness'\/
-- @runLLMTurn@, so the nested answerer can import it without pulling @loop@
-- (and its 'RunLLMTurn' dependency) in.
module HarnessTypes
  ( State (..)
  , initialState
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

-- | Deliberately tiny: a counter plus an accumulating history, so a
-- two-cycle run can prove a finalized EDIT function composed with the
-- incoming state (both fields keep growing) rather than replacing it
-- wholesale. Field named 'counter', not 'count' — 'Tidepool.Prelude'
-- re-exports 'Tidepool.Records.UpdateAllOutcome' with its own 'count'
-- field, which every turn compile (this module's own AND the answerer's
-- generated preamble, which unconditionally imports 'Tidepool.Prelude'
-- unqualified) would make ambiguous.
data State = State
  { counter :: Int
  , history :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {counter = 0, history = []}

render :: State -> Text
render st =
  [fmt|Spike state: counter={counter st}
{historyBlock}|]
  where
    historyBlock
      | null (history st) = "history: none" :: Text
      | otherwise = "history: " <> T.intercalate ", " (history st)
