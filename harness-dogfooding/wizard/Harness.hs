{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Authored harness (feature-brainstorm thought-partner): re-exports the
-- vocabulary + 'render' from 'HarnessTypes' and defines 'loop'. 'loop' asks the
-- answerer for a typed 'Contribution' via @runLLMTurn@; the answerer builds it
-- after consulting the operator with @askUser@ forms, so this file never
-- mentions @askUser@ — the form-threading is the answerer's, guided by the
-- prompt.
module Harness
  ( State (..)
  , Phase (..)
  , Contribution (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (Contribution (..), Phase (..), State (..), initialState,
                     render)
import Tidepool.Prelude hiding (render)
import Tidepool.Harness (Harness, runLLMTurn)

-- | One window of work: ask the answerer to advance the brainstorm one step (a
-- typed 'Contribution', operator taste already folded in via @askUser@), then
-- fold it into the next 'State'. The prompt spells out the 'Contribution'
-- constructor so the model builds a real one rather than prose.
loop :: State -> Harness State
loop st = do
  c <-
    runLLMTurn @Contribution
      "Advance the brainstorm one step for the current phase. Consult the \
      \operator with askUser forms for direction and taste, then finalize a \
      \Contribution. Its type is:\n\
      \  data Contribution = Contribution\n\
      \    { addedIdeas :: [Text]   -- new idea bullets surfaced this loop\n\
      \    , draftDelta :: Text     -- text to append to the running draft\n\
      \    , advance    :: Bool }   -- move to the next phase?\n\
      \Reply with exactly:\n\
      \  finalize @Contribution (Contribution { addedIdeas = [\"...\"], \
      \draftDelta = \"...\", advance = False })\n\
      \with your own values filled in."
  pure
    st
      { ideas = ideas st ++ addedIdeas c
      , draft = appendDelta (draft st) (draftDelta c)
      , phase = if advance c then nextPhase (phase st) else phase st
      }

-- | Append a draft delta, blank-tolerant on both sides.
appendDelta :: Text -> Text -> Text
appendDelta d delta
  | delta == "" = d
  | d == "" = delta
  | otherwise = d <> "\n\n" <> delta

-- | The wizard progression; terminal at 'Drafting'.
nextPhase :: Phase -> Phase
nextPhase Framing = Diverging
nextPhase Diverging = Converging
nextPhase Converging = Drafting
nextPhase Drafting = Drafting
