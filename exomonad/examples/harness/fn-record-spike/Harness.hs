{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Fixture for the record-of-functions acceptance: @loop@ asks for an
-- 'Edits' — a record whose FIELDS are @State -> State@ functions — and
-- applies both. Pins nested-closure handle delivery end to end.
module Harness
  ( State (..)
  , Edits (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (Edits (..), State (..), initialState, render)
import Tidepool.Prelude hiding (note, render)

import Tidepool.Harness (Harness, runLLMTurn)

-- | Apply BOTH fields of the finalized record — proves each nested closure
-- survived delivery individually (a sentinel would case-trap here).
loop :: State -> Harness State
loop st = do
  e <-
    runLLMTurn @Edits
      "Provide the cycle's edits: bump the counter and append a note."
  pure (note e (bump e st))
