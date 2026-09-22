{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Test fixture for the CRASH path of the boot fold.
-- Where @ResumeHarness.hs@ makes entry SELECTION observable (its two entries
-- cover deliberately different amounts of work), this one makes ABNORMAL
-- TERMINATION observable: both entries walk the SAME four steps, and what a
-- run gets through is decided by where it dies.
--
-- == The crash seam
--
-- After recording 'crashSeamStep', the loop calls @say@ — a Console verb. A
-- Console suspension against a driver with no Console handler wired aborts the
-- cycle right there ('SelfHarnessDriver' has no handler to dispatch into, and
-- says so), AFTER the preceding @record@ calls have already flushed to disk.
--
-- That is why the seam is a Console call and not, say, an @error@: whether a
-- cycle survives it is entirely the DRIVER's wiring, not this file's code, so
-- the crashed process and the resumed process run the same harness source. The
-- durable state it leaves — some flushed journal lines, no committed
-- checkpoint, an unretired run lease — is exactly what a @kill -9@ between two
-- appends leaves. The Rust side documents the equivalence in full.
--
-- == The steps
--
-- Both entries share one worker ('runSteps'), so the skip decision has a
-- single spelling; @loop@ passes 'emptyResume', the honest statement that a
-- fresh run has nothing to skip. A step whose @\"step\"@ record is already in
-- the injected fold is SKIPPED; one that is not is done and recorded. Nothing
-- here reads a file: the fold arrives already folded, from the driver.
module CrashResumeHarness
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

import Tidepool.Effects (record, say)
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
render st = [fmt|Crash-resume fixture. Runs: {runs st}.|]

-- | The steps one run of this harness consists of, so a resumed run's
-- decisions are entirely a function of the injected fold.
work :: [Text]
work = ["alpha", "beta", "gamma", "delta"]

-- | The step after which the loop reaches the crash seam. A driver with no
-- Console handler dies here; one with a Console handler walks straight past.
crashSeamStep :: Text
crashSeamStep = "beta"

-- | The universal entry, unchanged in arity from every other harness in the
-- tree.
loop :: State -> Harness State
loop = runSteps emptyResume

-- | The opt-in resumed entry: the same steps, against the fold the driver
-- built out of the crashed process's journal.
resumeLoop :: ResumeFold -> State -> Harness State
resumeLoop fold st = runSteps fold st {sawResume = isResumed fold}

runSteps :: ResumeFold -> State -> Harness State
runSteps fold st0 = do
  st <- foldM (step fold) st0 work
  pure st {runs = st.runs + 1}

step :: ResumeFold -> State -> Text -> Harness State
step fold st name = do
  st' <- case lookupResume "step" name fold of
    Just _ -> pure st {skipped = st.skipped <> [name]}
    Nothing -> do
      record "step" name (object ["step" .= name])
      pure st {recorded = st.recorded <> [name]}
  when (name == crashSeamStep) (say "crash-resume fixture: reached the crash seam")
  pure st'
