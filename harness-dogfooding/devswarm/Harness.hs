{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | DevSwarm's executable compatibility harness.
--
-- One outer loop iteration opens one root owner agent session.  The owner
-- dynamically grows the recursive owner tree with typed 'Tidepool.Fork.fork'
-- calls and uses
-- 'delegateTask' for short-lived repository work.  The driver's mandatory
-- checkpointed 'State' carries only the seed and the last rendered root
-- outcome; it is not the orchestration graph or a continuation snapshot.
module Harness
  ( State (..)
  , initialState
  , render
  , loop
  , resumeLoop
  , DevSwarmHarness
  , rootPrompt
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Effects (say)
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Resume (ResumeFold)

type DevSwarmHarness = Harness

loop :: State -> DevSwarmHarness State
loop st
  | T.strip st.objective == "" = do
      say "What should this DevSwarm run change or investigate?"
      seeded <- askUser @SeedObjective
      case T.strip seeded.seedObjective of
        "" -> loop st
        objective -> loop st {objective = objective}
loop st = do
  outcome <- runLLMTurn @RootOutcome (rootPrompt st)
  let rendered = renderOwnerOutcome outcome
  say [fmt|Root owner completed turn {show (st.turnsCompleted + 1)}.

{rendered}|]
  pure st
    { turnsCompleted = st.turnsCompleted + 1
    , lastOutcome = Just rendered
    }

-- | A restart begins a fresh owner tree from the durable seed and repository
-- state.  Live sessions, handles, and continuations are never decoded.
resumeLoop :: ResumeFold -> State -> DevSwarmHarness State
resumeLoop _ = loop

rootPrompt :: State -> Text
rootPrompt st =
  [fmt|Own the root node for DevSwarm turn {show (st.turnsCompleted + 1)}.

Objective: {st.objective}

Reason about the work before choosing a shape. Fork recursively capable child
owners with `fork @OwnerOutcome (renderNodeBrief brief)` only for independently
owned decomposition; use delegateTask for
short-lived research, candidate implementation, adversarial review, and
revision. You may run several calls concurrently with async/wait.

Finalize one OwnerOutcome that stands on its own for the operator.|]
