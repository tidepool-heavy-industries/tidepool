{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Fixture for the companion-memory answer contract: @loop@ asks for a
-- 'Turn' — data directives beside a @State -> State@ closure — applies the
-- edit, and folds the directives' renderings into state. Pins that BOTH
-- halves of the mixed product survive handle delivery: the data list is
-- readable in-heap (the real loop renders it into the curator brief) and
-- the closure applies.
module Harness
  ( State (..)
  , Turn (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes
  ( Directive (..)
  , State (..)
  , Turn (..)
  , initialState
  , render
  , renderDirective
  )
import Tidepool.Prelude hiding (render)

import Tidepool.Harness (Harness, runLLMTurn)

loop :: State -> Harness State
loop st = do
  t <- runLLMTurn @Turn "Answer with this cycle's Turn: directives plus the edit."
  let st' = edit t st
  pure st' {dlog = dlog st' <> map renderDirective (directives t)}
