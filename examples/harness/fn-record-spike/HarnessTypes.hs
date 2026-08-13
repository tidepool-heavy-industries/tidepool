{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Fixture for the record-of-functions acceptance
-- (@tidepool-harness\/tests\/selfharness_fn_finalize_spike.rs@): like the
-- fn-finalize-spike sibling, but the answer type is 'Edits' — a RECORD whose
-- fields are @State -> State@ functions. Pins that closures NESTED inside a
-- finalized product route through handle delivery (deep sentinel scan), not
-- the lossy bridge — the record-of-lenses/policies surface the one-session
-- plan exists for.
module HarnessTypes
  ( State (..)
  , Edits (..)
  , initialState
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

-- | Same tiny shape as the fn-finalize-spike fixture (field named 'counter',
-- not 'count' — see that fixture's note on the Prelude collision).
data State = State
  { counter :: Int
  , history :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | A RECORD OF FUNCTIONS — the answer type. No serialization instances and
-- none possible: its whole point is that it crosses in-heap by handle.
data Edits = Edits
  { bump :: State -> State
  , note :: State -> State
  }

initialState :: State
initialState = State {counter = 0, history = []}

render :: State -> Text
render st =
  [fmt|Record-spike state: counter={counter st}
{historyBlock}|]
  where
    historyBlock
      | null (history st) = "history: none" :: Text
      | otherwise = "history: " <> T.intercalate ", " (history st)
