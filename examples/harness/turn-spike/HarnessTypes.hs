{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Fixture for the companion-memory answer contract
-- (@plans\/companion-memory.md@, exercised by
-- @tidepool-harness\/tests\/selfharness_fn_finalize_spike.rs@): the answer
-- type is 'Turn' — a LIST OF DATA DIRECTIVES beside a @State -> State@
-- closure. Pins that a product mixing a pure-data list field with a closure
-- field routes whole through handle delivery: the loop reads BOTH halves
-- in-heap (the directives feed the curator brief, the edit applies to
-- state).
module HarnessTypes
  ( State (..)
  , Turn (..)
  , Directive (..)
  , renderDirective
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
  , dlog :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The memory verbs: typed INTENT, prose PAYLOAD (the curator agent is the
-- parser). Plain data — serializable, loggable.
data Directive = Remember Text | Modify Text | Forget Text
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The act window's answer: outward instructions beside the inward edit.
-- No serialization instances and none possible (the closure field): the
-- whole record crosses in-heap by handle.
data Turn = Turn
  { directives :: [Directive]
  , edit :: State -> State
  }

renderDirective :: Directive -> Text
renderDirective (Remember t) = "remember: " <> t
renderDirective (Modify t) = "modify: " <> t
renderDirective (Forget t) = "forget: " <> t

initialState :: State
initialState = State {counter = 0, dlog = []}

render :: State -> Text
render st =
  [fmt|Turn-spike state: counter={counter st}
{dlogBlock}|]
  where
    dlogBlock
      | null (dlog st) = "directives seen: none" :: Text
      | otherwise = "directives seen: " <> T.intercalate ", " (dlog st)
