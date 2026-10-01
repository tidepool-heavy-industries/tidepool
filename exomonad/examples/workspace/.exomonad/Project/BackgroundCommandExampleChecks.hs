{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.BackgroundCommandExampleChecks (completion) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Prelude hiding (readFile)
import qualified Project.Checks as Checks
import Tidepool.Check

-- The awaiting cell retains its continuation until the original command settles.
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
  void $ turn owner (stage "await")
  assertCell owner "late command completion keeps a compact typed projection"
    "let projected = completionProjection job commandEvidence in projectedJob projected == job && projectedOutcome projected == Cmd.CommandExited 0 && projectedCleanup projected == Cmd.CommandClean && projectedStdout projected == CompleteCapture && projectedStderr projected == CompleteCapture"
  assertCell owner "full command evidence remains available after the projection"
    "case completionCapture commandEvidence of { Right capture -> Cmd.capturedJob capture == job && Cmd.capturedResult capture == completionReceipt commandEvidence && Cmd.capturedStdout capture == Cmd.CaptureComplete \"passed\" && Cmd.capturedStderr capture == Cmd.CaptureComplete \"\"; _ -> False }"

  void $ turn owner $ Text.unlines
    [ "failedJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf failed >&2; exit 7\"]))"
    , "failedEvidence <- awaitCommandEvidence failedJob"
    ]
  assertCell owner "a failed command retains its outcome and both streams"
    "let projected = completionProjection failedJob failedEvidence in projectedJob projected == failedJob && projectedOutcome projected == Cmd.CommandExited 7 && projectedCleanup projected == Cmd.CommandClean && projectedStdout projected == CompleteCapture && projectedStderr projected == CompleteCapture"
  assertCell owner "failed execution evidence stays accessible without rerunning"
    "case completionCapture failedEvidence of { Right capture -> Cmd.capturedJob capture == failedJob && Cmd.capturedResult capture == completionReceipt failedEvidence && Cmd.capturedStdout capture == Cmd.CaptureComplete \"\" && Cmd.capturedStderr capture == Cmd.CaptureComplete \"failed\"; _ -> False }"
