{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Authored harness (feature-brainstorm thought-partner): the author-facing
-- 'State'/'Contribution' vocabulary and the pure 'render', with no reference to
-- @Tidepool.Harness@/@runLLMTurn@ so the nested answerer can import these types
-- without pulling in 'loop'. 'Harness' re-exports everything here plus 'loop'.
--
-- Kept deliberately small: the answerer must construct a 'Contribution' directly
-- from the model's reply, so flat fields (@[Text]@, @Text@, @Bool@) beat nested
-- records. The subject under examination is tidepool itself, so the accumulating
-- 'draft' is a change-request the loop produces about its own runtime.
module HarnessTypes
  ( State (..)
  , Phase (..)
  , Contribution (..)
  , initialState
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

-- | Durable memory threaded through 'loop' and read by 'render'.
data State = State
  { target    :: Text    -- ^ what is under examination
  , phase     :: Phase
  , loopCount :: Int
  , ideas     :: [Text]   -- ^ surfaced idea bullets, accumulated across loops
  , draft     :: Text     -- ^ the running change-request draft
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | Wizard progression; advances only when a 'Contribution' sets @advance@.
data Phase = Framing | Diverging | Converging | Drafting
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The typed value 'loop' asks the answerer for each iteration. Flat fields so
-- the model can build it directly; an ill-typed reply never resumes the loop.
data Contribution = Contribution
  { addedIdeas :: [Text]  -- ^ new idea bullets surfaced this loop
  , draftDelta :: Text    -- ^ text to append to the running draft
  , advance    :: Bool    -- ^ move to the next 'Phase'?
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState =
  State
    { target = "tidepool itself — the self-iterating harness and its runtime"
    , phase = Framing
    , loopCount = 0
    , ideas = []
    , draft = ""
    }

-- | @render :: State -> Maybe Text -> Text@. The answerer's working brief: the
-- subject, the phase and its instruction, the brainstorm so far, and a nudge to
-- consult the operator with @askUser@.
render :: State -> Maybe Text -> Text
render st lastCompaction =
  [fmt|You are a feature-brainstorming thought-partner in a self-iterating loop.
Subject under examination: {target st}.
Current phase: {phaseLine} (loop {loopCount st}).

{phaseInstruction}

{ideasBlock}

{draftBlock}
{compactionBlock}

When you need the operator's direction or taste, ASK THEM with a typed form
(askUser): a 1-of-N choice, a text box, a yes/no. Fold what they tell you into
the Contribution you finalize.|]
  where
    phaseLine = case phase st of
      Framing -> "framing the problem" :: Text
      Diverging -> "diverging — generating candidate directions"
      Converging -> "converging — culling to the strongest"
      Drafting -> "drafting the change-request"
    phaseInstruction = case phase st of
      Framing ->
        "Establish scope. Ask the operator which corner of the subject to focus \
        \on and any hard constraints, then propose two or three framings as \
        \addedIdeas." :: Text
      Diverging ->
        "Generate several DISTINCT candidate features or changes — push past the \
        \obvious. Ask the operator which areas interest them, and add each as an \
        \idea."
      Converging ->
        "Cull. Ask the operator which ideas to keep vs drop, and sharpen the \
        \survivors in the draft."
      Drafting ->
        "Fold the kept ideas into a concrete change-request draft. Ask for edits, \
        \and set advance=True only once the operator is satisfied."
    ideasBlock
      | null (ideas st) = "No ideas surfaced yet." :: Text
      | otherwise =
          "Ideas so far:\n" <> T.intercalate "\n" (map ("- " <>) (ideas st))
    draftBlock
      | T.null (draft st) = "Draft: (empty)" :: Text
      | otherwise = "Draft so far:\n" <> draft st
    compactionBlock = case lastCompaction of
      Nothing -> ""
      Just summary -> "\n\nSummary of the prior window:\n" <> summary
