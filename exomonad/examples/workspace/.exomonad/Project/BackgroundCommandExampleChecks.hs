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
  success <- turn owner "completionProjection job commandEvidence"
  check "late command completion keeps a compact typed projection"
    (all (`Text.isInfixOf` lastOutput success) ["CommandExited 0", "CompleteCapture"])
  evidence <- turn owner "commandEvidence"
  check "full command evidence remains available after the projection"
    (all (`Text.isInfixOf` lastOutput evidence) ["CaptureComplete", "passed"])

  void $ turn owner $ Text.unlines
    [ "failedJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf failed >&2; exit 7\"]))"
    , "failedEvidence <- awaitCommandEvidence failedJob"
    ]
  failed <- turn owner "completionProjection failedJob failedEvidence"
  check "a failed command retains its outcome and both streams"
    (all (`Text.isInfixOf` lastOutput failed) ["CommandExited 7", "CompleteCapture"])
  fullFailure <- turn owner "failedEvidence"
  check "failed execution evidence stays accessible without rerunning"
    (all (`Text.isInfixOf` lastOutput fullFailure) ["CaptureComplete", "failed"])
