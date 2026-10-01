{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.PrepareContinueChecks (preparationCompletion) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

preparationCompletion :: Member RecipeCheck effects => Eff effects ()
preparationCompletion = do
  owner <- root
  void $ turn owner "import Exomonad.Contrib.PrepareContinue\nimport Exomonad.Contrib.RetainedEvidence"
  void $ turn owner $ Text.unlines
    [ "okJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"echo ready\"]))"
    , "prepared <- awaitPrepared okJob (\\_ -> pure (Right \"ready\" :: Either Text Text))"
    ]
  assertCell owner "preparation resumes with typed readiness"
    "case prepared of { Right ready -> ready == \"ready\"; _ -> False }"
  void $ turn owner $ Text.unlines
    [ "badJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf failure-diagnostic >&2; exit 7\"]))"
    , "failed <- awaitPrepared badJob (\\_ -> pure (Right \"ready\" :: Either Text Text))"
    ]
  assertCell owner "failed preparation retains original command receipt"
    "case failed of { Left (PreparationCommandFailed result) -> Cmd.job result == badJob && Cmd.commandOutcome (Cmd.commandResult result) == Cmd.CommandExited 7 && Cmd.commandCleanup (Cmd.commandResult result) == Cmd.CommandClean; _ -> False }"

  void $ turn owner "badReceipt <- Cmd.commandResult <$> Cmd.quiet (Cmd.await badJob)\nmismatch <- verifyPrepared okJob badReceipt (\\_ -> pure (Right () :: Either Text ()))"
  assertCell owner "observed preparation rejects a receipt from another command"
    "case mismatch of { Left (PreparationReceiptMismatch supplied actual) -> supplied == badReceipt && Cmd.job actual == okJob && Cmd.commandOutcome supplied == Cmd.CommandExited 7 && Cmd.commandOutcome (Cmd.commandResult actual) == Cmd.CommandExited 0; _ -> False }"

  assertCell owner "retained read refuses zero byte budget"
    "evidenceBudget 0 == Left (InvalidEvidenceBudget 0)"
  void $ turn owner "Right allowance <- pure (evidenceBudget 64)\nrecovered <- recoverRetained allowance badJob\nRight expectedStderr <- Cmd.tryPage badJob Cmd.Stderr (Cmd.OutputSlice 0 64)"
  assertCell owner "bounded pages recover the failed job without resubmission"
    "retainedJob recovered == badJob && retainedStatus recovered == Cmd.CommandFinished badReceipt && streamStop (retainedStdout recovered) == StreamComplete && streamStop (retainedStderr recovered) == StreamComplete && T.concat (map Cmd.pageText (streamPages (retainedStderr recovered))) == \"failure-diagnostic\" && streamPages (retainedStderr recovered) == [expectedStderr] && all (\\page -> not (Cmd.outputLossy (Cmd.pageDetails page)) && Cmd.outputLostBytes (Cmd.pageDetails page) == 0) (streamPages (retainedStderr recovered))"
  void $ turn owner $ Text.unlines
    [ "largeResult <- Cmd.quiet (Cmd.run (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf abcdefghij\"])))"
    , "Right tiny <- pure (evidenceBudget 4)"
    , "small <- recoverRetained tiny (Cmd.job largeResult)"
    , "Right expectedSmall <- Cmd.tryPage (Cmd.job largeResult) Cmd.Stdout (Cmd.OutputSlice 0 4)"
    ]
  assertCell owner "retained page reading stops at the requested byte budget"
    "retainedJob small == Cmd.job largeResult && streamStop (retainedStdout small) == StreamBudgetReached && case streamPages (retainedStdout small) of { [page] -> page == expectedSmall && Cmd.pageText page == \"abcd\" && Cmd.outputStart (Cmd.pageDetails page) == 0 && Cmd.outputEnd (Cmd.pageDetails page) == 4 && Cmd.outputAvailableEnd (Cmd.pageDetails page) == 10; _ -> False }"

  void $ turn owner "missing <- awaitPrepared okJob (\\_ -> pure (Left \"prerequisite missing\" :: Either Text Text))"
  assertCell owner "readiness failure stays separate from successful command"
    "case missing of { Left (PreparationReadinessFailed issue result) -> issue == \"prerequisite missing\" && Cmd.job result == okJob && Cmd.commandOutcome (Cmd.commandResult result) == Cmd.CommandExited 0 && Cmd.commandCleanup (Cmd.commandResult result) == Cmd.CommandClean; _ -> False }"
