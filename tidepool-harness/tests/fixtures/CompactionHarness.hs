{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Test fixture for W2 (mid-loop, in-place compaction): a harness whose
-- 'loop' opens TWO @runLLMTurn \@Text@ holes in the SAME loop, so the runtime
-- can trip its emergency compaction BETWEEN the two holes and the second hole
-- observes the COMPACTED context. Unlike @TwoHoleHarness@, this 'render'
-- SURFACES the @Maybe Text@ compaction argument (prints a "Summary of the
-- prior window:" block when it is @Just@), so a test can assert the mid-loop
-- summary reaches the NEXT render's @lastCompaction@.
module CompactionHarness
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
render st mcomp =
  case mcomp of
    Nothing -> [fmt|Compaction harness. Loop count: {loopCount st}.|]
    Just s  -> [fmt|Compaction harness. Loop count: {loopCount st}.
Summary of the prior window: {s}|]

-- | TWO holes in one loop. The runtime's mid-loop compaction check runs
-- BETWEEN them; when it trips, the second hole's answerer drives under the
-- replaced (summarized) context.
loop :: State -> Harness State
loop st = do
  first <- runLLMTurn @Text "FIRST-HOLE: name a fruit."
  second <- runLLMTurn @Text "SECOND-HOLE: name a color."
  pure
    st
      { loopCount = loopCount st + 1
      , answers = [first, second]
      }
