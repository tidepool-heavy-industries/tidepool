{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Test fixture for PRD 20 S1-L5's boot fold: a harness declaring BOTH entry
-- points — the universal @loop@ and the opt-in @resumeLoop@ — so the driver's
-- entry SELECTION is what is under test.
--
-- The two entries are deliberately asymmetric in the work they cover, which is
-- what makes "appends only the delta" observable:
--
-- * @loop@ (a FRESH boot) walks the first two steps of 'work' and records each
--   — a run a crash caught with one step still to go.
-- * @resumeLoop@ (a RESUMED boot) walks ALL of 'work' against the injected
--   fold: a step with a recorded @\"step\"@ entry is SKIPPED, one without is
--   done and recorded. Over the journal @loop@ left behind, that is exactly one
--   new append.
--
-- Both share one worker ('runSteps'), so the skip decision has a single
-- spelling; @loop@ simply passes 'emptyResume', which is the honest statement
-- that a fresh run has nothing to skip. Nothing here reads a file: the fold
-- arrives already folded, from the driver.
module ResumeHarness
  ( State (..)
  , initialState
  , render
  , loop
  , resumeLoop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Effects (record)
import Tidepool.Harness (Harness)
import Tidepool.Resume (ResumeFold, emptyResume, isResumed, lookupResume)

data State = State
  { runs :: Int
  , recorded :: [Text]
  , skipped :: [Text]
  , sawResume :: Bool
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State {runs = 0, recorded = [], skipped = [], sawResume = False}

render :: State -> Text
render st = [fmt|Resume fixture. Runs: {runs st}.|]

-- | The steps this harness's run consists of. Fixed, so a resumed run's
-- decisions are entirely a function of the injected fold.
work :: [Text]
work = ["alpha", "beta", "gamma"]

-- | The FRESH entry: the first two steps only, standing in for a process that
-- crashed with @gamma@ still to do. Unchanged in arity from every other
-- harness in the tree.
loop :: State -> Harness State
loop st = runSteps emptyResume (take 2 work) st

-- | The RESUMED entry: every step, but only the ones the fold does not already
-- account for actually run.
resumeLoop :: ResumeFold -> State -> Harness State
resumeLoop fold st = runSteps fold work st {sawResume = isResumed fold}

runSteps :: ResumeFold -> [Text] -> State -> Harness State
runSteps fold names st0 = do
  st <- foldM (step fold) st0 names
  pure st {runs = st.runs + 1}

step :: ResumeFold -> State -> Text -> Harness State
step fold st name =
  case lookupResume "step" name fold of
    Just _ -> pure st {skipped = st.skipped <> [name]}
    Nothing -> do
      record "step" name (object ["step" .= name])
      pure st {recorded = st.recorded <> [name]}
