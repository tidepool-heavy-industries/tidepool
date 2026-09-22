{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Fixture for the OODA-pipeline acceptance: a loop of up to THREE typed
-- @runLLMTurn@ windows whose conditional shape is decided by the model's own
-- typed answers — orient always runs; decide runs only when orientation says
-- 'Deliberate'; act runs unless orientation says 'Quiet'. All three windows
-- share one accumulating answerer context (the driver's per-loop answerer
-- node), so a later phase sees the earlier phases' exchanges.
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes
import Tidepool.Prelude hiding (render)

import Tidepool.Harness (Harness, runLLMTurn)

loop :: State -> Harness State
loop st = do
  o <- runLLMTurn @Orientation "ORIENT: read the rendered state; finalize an Orientation."
  mv <- case o.tempo of
    Quiet -> pure Nothing
    Familiar m -> pure (Just m)
    Deliberate _ -> Just <$> runLLMTurn @Move "DECIDE: weigh your candidates; finalize one Move."
  case mv of
    Nothing -> pure (finish Nothing st)
    Just m -> do
      edit <- runLLMTurn @(State -> State) "ACT: carry out the move; finalize the durable edit."
      pure (finish (Just m) (edit st))

-- | Loop bookkeeping: stamp 'Engage'\'s hypothesis into the feedback wire —
-- an expectation lives exactly one loop unless renewed by another 'Engage'.
finish :: Maybe Move -> State -> State
finish mv st =
  st
    { lastExpectation = case mv of
        Just (Engage {expecting = e}) -> Just e
        _ -> Nothing
    }
