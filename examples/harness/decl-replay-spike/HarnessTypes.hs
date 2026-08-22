{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Fixture for the driver-level decl-plane replay regression
-- (@tidepool-harness\/tests\/selfharness_decl_plane_replay.rs@): same tiny
-- @State@\/@render@ split as @examples\/harness\/fn-finalize-spike@, kept
-- independent of that fixture so this test's three-window @loop@ (below)
-- cannot drift the single-window spike's shape.
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
