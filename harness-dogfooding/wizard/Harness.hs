{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
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
import Tidepool.Prelude hiding (note, render)
import Tidepool.Form (askUser, note)
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.QQ (fmt)

-- | One window of work: FIRST a harness-level steering ask (a 'note' narrates
-- the accumulated state, then the operator says where to push — the harness
-- owns steering, the agent owns cognition), THEN the answerer advances the
-- brainstorm one step under that steer (a typed 'Contribution'), folded into
-- the next 'State'. The prompt spells out the 'Contribution' constructor so
-- the model builds a real one rather than prose.
loop :: State -> Harness State
loop st = do
  note
    [fmt|STEERING — phase: {show (phase st)}, ideas so far: {length (ideas st)}.
{draftContext st}

Where do you want to steer this loop? (Free text — it goes to the model verbatim, above its own framing.)|]
  steer <- askUser @Text
  c <-
    runLLMTurn @Contribution
      [fmt|OPERATOR STEERING for this loop (verbatim, follow it over your own framing): {steer}

Advance the brainstorm one step for the current phase. Consult the operator with askUser forms for direction and taste, then finalize a Contribution. When you present `choose` options, ALWAYS include an escape option — e.g. ("None of these — I'll say it in my own words", Nothing) with the others Just-wrapped — and on that branch gather free text with `askUser @Text` instead of forcing a canned pick. Contribution's type is:
  data Contribution = Contribution
    {{ addedIdeas :: [Text]   -- new idea bullets surfaced this loop
    , draftDelta :: Text     -- text to append to the running draft
    , advance    :: Bool }}   -- move to the next phase?
Reply with exactly:
  finalize @Contribution (Contribution {{ addedIdeas = ["..."], draftDelta = "...", advance = False }})
with your own values filled in.|]
  pure
    st
      { ideas = ideas st ++ addedIdeas c
      , draft = appendDelta (draft st) (draftDelta c)
      , phase = if advance c then nextPhase (phase st) else phase st
      }

-- | Steering-note context for the accumulated draft, blank-tolerant.
draftContext :: State -> Text
draftContext st
  | draft st == "" = "Fresh draft."
  | otherwise =
      [fmt|Draft so far:
{draft st}|]

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
