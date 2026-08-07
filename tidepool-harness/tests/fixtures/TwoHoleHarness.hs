{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for W1/C2 (context-window accumulation): a harness whose
-- 'loop' opens TWO @runLLMTurn \@Text@ holes in the SAME loop. The second
-- hole's answerer runs on the SAME per-loop answerer session as the first, so
-- its transcript must already contain the first hole's exchange — that is the
-- accumulating context window the wave wires (each hole was a fresh,
-- context-free node before). The second prompt embeds a sentinel so a test can
-- assert the second answerer turn saw the first prompt too.
module TwoHoleHarness
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
  , answers   :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {loopCount = 0, answers = []}

render :: State -> Maybe Text -> Text
render st _ =
  [fmt|Two-hole harness. Loop count: {loopCount st}.|]

-- | TWO holes in one loop. The first asks for a fruit; the second embeds a
-- sentinel ("SECOND-HOLE") so a test can distinguish the two answerer turns
-- and confirm the second ran with the first's exchange in its transcript.
loop :: State -> Harness State
loop st = do
  first <- runLLMTurn @Text "FIRST-HOLE: name a fruit."
  second <- runLLMTurn @Text "SECOND-HOLE: name a color."
  pure
    st
      { loopCount = loopCount st + 1
      , answers = [first, second]
      }
