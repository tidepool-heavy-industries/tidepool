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
  void $ turn owner "import Project.PrepareContinueChecks"
  success <- turn owner $ Text.unlines
    [ "okJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"echo ready\"]))"
    , "awaitPrepared okJob (\\_ -> pure (Right \"ready\" :: Either Text Text))"
    ]
  check "preparation resumes with typed readiness" ("Right \"ready\"" `Text.isInfixOf` lastOutput success)
  failed <- turn owner $ Text.unlines
    [ "badJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf failure-diagnostic >&2; exit 7\"]))"
    , "awaitPrepared badJob (\\_ -> pure (Right \"ready\" :: Either Text Text))"
    ]
  check "failed preparation retains original command receipt" ("CommandExited 7" `Text.isInfixOf` lastOutput failed)

  mismatch <- turn owner "badReceipt <- Cmd.commandResult <$> Cmd.quiet (Cmd.await badJob)\nverifyPrepared okJob badReceipt (\\_ -> pure (Right () :: Either Text ()))"
  check "observed preparation rejects a receipt from another command"
    ("PreparationReceiptMismatch" `Text.isInfixOf` lastOutput mismatch)

  budget <- turn owner "evidenceBudget 0"
  check "retained read refuses zero byte budget" ("InvalidEvidenceBudget 0" `Text.isInfixOf` lastOutput budget)
  recovered <- turn owner
    "Right allowance <- pure (evidenceBudget 64)\nrecoverRetained allowance badJob"
  check "bounded pages recover the failed job without resubmission"
    (all (`Text.isInfixOf` lastOutput recovered)
      ["CommandExited 7", "failure-diagnostic", "StreamComplete"])
  bounded <- turn owner $ Text.unlines
    [ "largeResult <- Cmd.quiet (Cmd.run (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf abcdefghij\"])))"
    , "Right tiny <- pure (evidenceBudget 4)"
    , "small <- recoverRetained tiny (Cmd.job largeResult)"
    , "(streamStop (retainedStdout small), map Cmd.pageText (streamPages (retainedStdout small)))"
    ]
  check "retained page reading stops at the requested byte budget"
    (all (`Text.isInfixOf` lastOutput bounded) ["StreamBudgetReached", "abcd"])

  missing <- turn owner "awaitPrepared okJob (\\_ -> pure (Left \"prerequisite missing\" :: Either Text Text))"
  check "readiness failure stays separate from successful command"
    (all (`Text.isInfixOf` lastOutput missing) ["prerequisite missing", "CommandExited 0"])
