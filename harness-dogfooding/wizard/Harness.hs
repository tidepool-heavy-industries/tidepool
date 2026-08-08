{-# LANGUAGE DataKinds #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Authored harness (DOGFOOD #1, feature-brainstorm thought-partner):
-- re-exports the vocabulary + 'render' from 'HarnessTypes' and defines the one
-- effectful piece, 'loop'. See 'HarnessTypes' for the wizard design and the
-- tidepool-self-examination rationale.
--
-- 'loop' asks the answerer for a typed 'Contribution' via @runLLMTurn@ (the
-- OUTER harness's only effect). The answerer produces that 'Contribution' by
-- reasoning under 'render'\'s brief and consulting the operator with @askUser@
-- forms — so this file never mentions @askUser@ itself; the form-threading is
-- the answerer's, guided by the prompt. That is why this harness runs against
-- the current tree and simply gains real forms once the @AskUser@ effect lands.
module Harness
  ( State (..)
  , Phase (..)
  , Area (..)
  , IdeaStatus (..)
  , Idea (..)
  , Contribution (..)
  , initialState
  , render
  , loop
  ) where

import HarnessTypes (Area (..), Contribution (..), Idea (..), IdeaStatus (..),
                     Phase (..), State (..), initialState, render)
-- `render` comes from `HarnessTypes`; `Tidepool.Prelude` also exports an
-- unrelated `render` (`Tidepool.Render`), hidden to avoid the clash.
import Tidepool.Prelude hiding (render)
import Tidepool.Harness (Harness, runLLMTurn)

-- | @loop :: State -> Harness State@. LOCKED signature. One context window's
-- work: ask the answerer to advance the brainstorm one step (a typed
-- 'Contribution', taste already folded in via the operator's @askUser@ forms),
-- then fold it into a fresh 'State' — the durable memory carried to the next
-- loop.
loop :: State -> Harness State
loop st = do
  c <-
    runLLMTurn @Contribution
      "Advance the brainstorm one step for the current phase. Consult the \
      \operator with askUser forms for direction and taste (area choice, \
      \keep/drop, constraints, edits), then finalize a Contribution that folds \
      \in what you learned."
  pure
    st
      { loopCount = loopCount st + 1
      , ideas = foldl mergeIdea (ideas st) (newIdeas c)
      , draft = appendDelta (draft st) (draftDelta c)
      , openQuestions =
          maybe
            (openQuestions st)
            (\q -> take 5 (q : openQuestions st))
            (nextQuestion c)
      , phase = if advance c then nextPhase (phase st) else phase st
      }

-- | Merge a contributed idea into the accumulator BY TITLE: a same-title idea
-- updates in place (so the operator's Keep/Drop status and any re-statused
-- rationale replace the prior entry), otherwise it appends. Keeps the idea list
-- stable across loops rather than duplicating on every re-proposal.
mergeIdea :: [Idea] -> Idea -> [Idea]
mergeIdea acc i
  | any ((== title i) . title) acc =
      map (\x -> if title x == title i then i else x) acc
  | otherwise = acc ++ [i]

-- | Fold a draft delta onto the running draft, blank-tolerant on both sides.
appendDelta :: Text -> Text -> Text
appendDelta d delta
  | delta == "" = d
  | d == "" = delta
  | otherwise = d <> "\n\n" <> delta

-- | The wizard progression. Terminal at 'Drafting' — further loops keep
-- refining the draft rather than falling off the end.
nextPhase :: Phase -> Phase
nextPhase Framing = Diverging
nextPhase Diverging = Converging
nextPhase Converging = Drafting
nextPhase Drafting = Drafting
