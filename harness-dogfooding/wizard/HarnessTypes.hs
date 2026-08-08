{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Authored harness (DOGFOOD #1): a FEATURE-BRAINSTORM / PRD-CONSTRUCTION
-- thought-partner. The self-iterating harness threads an agent through a small
-- wizard — frame → diverge → converge → draft — accumulating a change-request
-- draft in 'State' across loops. The agent supplies the per-step cognition; the
-- OPERATOR supplies taste through @askUser@ forms (a 1-of-N area pick, a text
-- constraint, a keep/drop yes-no); the human clicks continue between loops.
--
-- The intended target under examination is TIDEPOOL ITSELF — so the flow is
-- recursive: the harness runs an agent that brainstorms improvements to the
-- harness, and its 'draft'/'ideas' output IS a stream of change requests we can
-- then implement. Distillation-by-hand: authoring + running this is how we find
-- the frictions that become the next harness helpers.
--
-- This module holds the author-facing vocabulary + the pure 'render' with NO
-- reference to @Tidepool.Harness@/@runLLMTurn@ (same split rationale as the
-- reference @examples/harness/HarnessTypes.hs@): the nested answerer imports
-- these types to build a typed @finalize \@Contribution (...)@ reply WITHOUT
-- pulling in 'loop' and its @RunLLMTurn@ dependency. 'Harness' re-exports
-- everything here plus 'loop'.
--
-- Minimal effect surface, deliberately: the agent has only @askUser@ (typed
-- forms) + @finalize@. No fs/exec/http — the substrate it reasons over comes
-- from 'render' (seeded context) and the operator's form answers.
module HarnessTypes
  ( State (..)
  , Phase (..)
  , Area (..)
  , IdeaStatus (..)
  , Idea (..)
  , Contribution (..)
  , initialState
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
-- `render` is this module's own LOCKED export; `Tidepool.Prelude` also exports
-- an unrelated `render` (`Tidepool.Render`), hidden to avoid the clash.
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

-- | The durable memory threaded through 'loop' and read by 'render': the
-- accumulating brainstorm / change-request draft. Small + typed (the
-- "bag of typed values" the runtime contract calls for), not an unbounded log.
data State = State
  { target        :: Text     -- ^ what is under examination this session
  , phase         :: Phase     -- ^ where in the wizard we are
  , loopCount     :: Int
  , ideas         :: [Idea]    -- ^ surfaced ideas, each with an operator-set status
  , draft         :: Text      -- ^ the working change-request / PRD draft
  , openQuestions :: [Text]    -- ^ questions carried into the next loop
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The wizard progression. 'loop' advances it only when a 'Contribution' says
-- to ('advance'), so a phase can span several loops until the operator is happy.
data Phase = Framing | Diverging | Converging | Drafting
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Which corner of the subject an idea touches — the 1-of-N enum the agent
-- proposes and the operator confirms via an @askUser@ choice field. Tuned to
-- tidepool's own shape for the self-examination dogfood.
data Area = Effects | Surface | Runtime | Tooling | Docs | DevEx
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | An idea's operator-set taste status — the whole point of threading a human
-- through: 'Kept'\/'Dropped' is a judgment the agent cannot make for you.
data IdeaStatus = Proposed | Kept | Dropped
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | One surfaced idea. Merged by 'title' across loops (see 'Harness.mergeIdea')
-- so the operator's status sticks and re-proposals update in place.
data Idea = Idea
  { title     :: Text
  , area      :: Area
  , rationale :: Text
  , status    :: IdeaStatus
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The typed value 'loop' asks the answerer for each iteration (via
-- @runLLMTurn \@Contribution@). The answerer builds it AFTER consulting the
-- operator through @askUser@ forms, so the operator's taste is folded in before
-- it ever crosses back. GHC validates it at 'Contribution' — an ill-typed reply
-- never resumes the loop.
data Contribution = Contribution
  { newIdeas     :: [Idea]      -- ^ ideas surfaced or re-statused this loop
  , draftDelta   :: Text        -- ^ text to fold into the running draft
  , nextQuestion :: Maybe Text  -- ^ a question to carry into the next loop
  , advance      :: Bool        -- ^ move to the next 'Phase'?
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The very first loop starts here (before any persisted State restores). The
-- target is seeded to tidepool self-examination; change it to point the
-- thought-partner at anything else.
initialState :: State
initialState =
  State
    { target = "tidepool itself — the self-iterating harness and its runtime"
    , phase = Framing
    , loopCount = 0
    , ideas = []
    , draft = ""
    , openQuestions = []
    }

-- | @render :: State -> Maybe Text -> Text@. LOCKED signature. Plain Haskell +
-- @[fmt|]@ over 'State'; cannot suspend or call 'loop'. Produces the answerer's
-- working brief: the subject, the phase and its instruction, the brainstorm so
-- far, and an explicit nudge to consult the operator with @askUser@.
render :: State -> Maybe Text -> Text
render st lastCompaction =
  [fmt|You are a feature-brainstorming thought-partner in a self-iterating loop.
Subject under examination: {target st}.
Current phase: {phaseLine} (loop {loopCount st}).

{phaseInstruction}

{ideasBlock}

{draftBlock}
{questionsBlock}{compactionBlock}

When you need the operator's direction or taste, ASK THEM with a typed form
(askUser): a 1-of-N choice to pick an Area or to Keep/Drop an idea, a text box
for a constraint or a refinement, a yes/no to confirm advancing the phase. Fold
what they tell you into a Contribution and finalize it.|]
  where
    phaseLine = case phase st of
      Framing -> "framing the problem" :: Text
      Diverging -> "diverging — generating candidate directions"
      Converging -> "converging — culling to the strongest"
      Drafting -> "drafting the change-request"
    phaseInstruction = case phase st of
      Framing ->
        "Establish scope. Ask the operator which corner of the subject to focus \
        \on and any hard constraints, then propose two or three framings." :: Text
      Diverging ->
        "Generate several DISTINCT candidate features or changes — push past the \
        \obvious first answers. Ask the operator which Areas interest them most, \
        \and add each idea as Proposed."
      Converging ->
        "Cull. Present the surfaced ideas and ask the operator which to Keep vs \
        \Drop, then sharpen the rationale of the survivors."
      Drafting ->
        "Fold the Kept ideas into a concrete, actionable change-request draft. \
        \Ask the operator for edits, then advance only when they are satisfied."
    ideasBlock
      | null (ideas st) = "No ideas surfaced yet." :: Text
      | otherwise =
          "Ideas so far:\n"
            <> T.intercalate "\n" (map ideaLine (ideas st))
    -- `show` in Tidepool.Prelude is Text-valued (`show :: a -> Text`), so it
    -- concatenates into the brief directly (same idiom as the reference harness).
    ideaLine i =
      "- [" <> show (status i) <> "/" <> show (area i) <> "] "
        <> title i <> " — " <> rationale i
    draftBlock
      | T.null (draft st) = "Draft: (empty)" :: Text
      | otherwise = "Draft so far:\n" <> draft st
    questionsBlock
      | null (openQuestions st) = ""
      | otherwise =
          "\nOpen questions:\n"
            <> T.intercalate "\n" (map ("- " <>) (openQuestions st))
    compactionBlock = case lastCompaction of
      Nothing -> ""
      Just summary -> "\n\nSummary of the prior window:\n" <> summary
