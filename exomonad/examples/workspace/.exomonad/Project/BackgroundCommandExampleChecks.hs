{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.BackgroundCommandExampleChecks (completion) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Prelude hiding (readFile)
import Project.BackgroundCommandExample
import qualified Project.Checks as Checks
import Tidepool.Check

-- The first cell starts a bounded command and attaches the reusable watcher.
-- A later cell asks for the compact projection and then the full capture.
completion :: Member RecipeCheck effects => Eff effects ()
completion = do
  owner <- root
  source <- readFile owner (Checks.checkSource "background-command-example")
  let stage name =
        let marker = "-- Stage: " <> name <> "\n"
            (_, suffix) = Text.breakOn marker source
            body = Text.drop (Text.length marker) suffix
         in fst (Text.breakOn "\n-- Stage: " body)
  void $ turn owner (stage "imports")
  void $ turn owner (stage "start")
  void $ turn owner (stage "watch")
  success <- awaitOutput owner
    "readCommandProjection watcher"
    (Text.isInfixOf "CompleteCapture")
  check "late command completion keeps a compact typed projection"
    (all (`Text.isInfixOf` success) ["CommandExited 0", "CompleteCapture"])
  evidence <- turn owner "readCommandEvidence watcher"
  check "full command evidence remains available after the projection"
    (all (`Text.isInfixOf` lastOutput evidence) ["CaptureComplete", "passed"])
  void $ turn owner "finishCommandWatcher watcher"

  void $ turn owner $ Text.unlines
    [ "failedJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf failed >&2; exit 7\"]))"
    , "failedWatcher <- startCommandWatcher failedJob"
    ]
  failed <- awaitOutput owner
    "readCommandProjection failedWatcher"
    (Text.isInfixOf "CompleteCapture")
  check "a failed command retains its outcome and both streams"
    (all (`Text.isInfixOf` failed) ["CommandExited 7", "CompleteCapture"])
  fullFailure <- turn owner "readCommandEvidence failedWatcher"
  check "failed execution evidence stays accessible without rerunning"
    (all (`Text.isInfixOf` lastOutput fullFailure) ["CaptureComplete", "failed"])
  void $ turn owner "finishCommandWatcher failedWatcher"
