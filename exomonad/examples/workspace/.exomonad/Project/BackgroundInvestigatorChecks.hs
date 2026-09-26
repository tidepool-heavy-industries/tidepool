{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.BackgroundInvestigatorChecks (terminalFailure, pendingProbe) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Char (isAlphaNum)
import qualified Data.Text as Text
import Tidepool.Check

terminalFailure :: Member RecipeCheck effects => Eff effects ()
terminalFailure = do
  owner <- root
  directory <- Text.strip <$> git owner ["rev-parse", "--show-toplevel"]
  void $ turn owner $ Text.unlines
    [ "import Project.BackgroundInvestigator"
    , "import Project.ParallelInvestigate"
    , "let directory = " <> literal directory
    , "let spec = FocusedSpec \"investigate fixture\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
    , "let probe name command = CommandProbe name name directory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", command])"
    , "let choose _ choices = pure (Right (case choices of { [] -> Nothing; first : _ -> Just first }))"
    , "failed <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"exit 7\"]))"
    , "Cmd.await failed"
    , "investigator <- watchFailedCheck me \"known failed original\" (PreparedFocusedRun spec failed) (const [probe \"first\" \"printf first\"]) (\\_ choices -> pure (Right (case choices of { [] -> Nothing; first : _ -> Just first })))"
    ]
  settled <- awaitOutput owner
    "state <- readInvestigation investigator\n(case (investigationResult state, investigationReport state) of { (Just focused, Just report) -> (focusedPreparation focused, followupStop report, length (diagnosticObservations report), investigationSummary (investigationRun state) focused report); _ -> error \"pending\" })"
    (Text.isInfixOf "NoProbeNeeded")
  check ("terminal failure gathers one diagnostic and retains failed preparation: " <> Text.take 320 settled)
    (all (`Text.isInfixOf` settled)
      ["PreparationUnknown", "NoProbeNeeded", "first", "check not accepted", "CommandExited 7", "CommandExited 0"])
  finish <- turn owner "finishInvestigation investigator"
  check "completed investigation releases its report owner"
    ("Right" `Text.isInfixOf` lastOutput finish)
  void $ turn owner $ Text.unlines
    [ "succeeded <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"true\"]))"
    , "Cmd.await succeeded"
    , "successWatcher <- watchFailedCheck me \"already successful command\" (FocusedRun spec succeeded) (const [probe \"unrun\" \"printf never\"]) (\\_ choices -> pure (Right (case choices of { [] -> Nothing; first : _ -> Just first })))"
    ]
  success <- awaitOutput owner
    "state <- readInvestigation successWatcher\nfmap followupStop (investigationReport state)"
    (Text.isInfixOf "OriginalNotFailed")
  check "successful original skips probes" ("OriginalNotFailed" `Text.isInfixOf` success)
  void $ turn owner "finishInvestigation successWatcher"
  void $ turn owner
    "invalidWatcher <- watchFailedCheck me \"invalid probes\" (FocusedRun spec failed) (const [probe \"same\" \"printf one\", probe \"same\" \"printf two\"]) (\\_ choices -> pure (Right (case choices of { [] -> Nothing; first : _ -> Just first })))"
  invalid <- awaitOutput owner
    "state <- readInvestigation invalidWatcher\nfmap followupStop (investigationReport state)"
    (Text.isInfixOf "InvalidFollowupProbes")
  check "invalid probe selection stops without hiding original failure"
    ("DuplicateAvailableProbe" `Text.isInfixOf` invalid)
  void $ turn owner "finishInvestigation invalidWatcher"
  void $ turn owner
    "cancelled <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sleep\", \"30\"]))\nCmd.cancel cancelled\nCmd.await cancelled\ncancelWatcher <- watchFailedCheck me \"cancelled original\" (PreparedFocusedRun spec cancelled) (const [probe \"unrun\" \"printf never\"]) (\\_ choices -> pure (Right (case choices of { [] -> Nothing; first : _ -> Just first })))"
  cancelledResult <- awaitOutput owner
    "state <- readInvestigation cancelWatcher\nfmap followupStop (investigationReport state)"
    (Text.isInfixOf "OriginalNotDiagnosable")
  check "cancelled original starts no diagnostic probe"
    ("OriginalNotDiagnosable" `Text.isInfixOf` cancelledResult)
  void $ turn owner "finishInvestigation cancelWatcher"

pendingProbe :: Member RecipeCheck effects => Eff effects ()
pendingProbe = do
  owner <- root
  directory <- Text.strip <$> git owner ["rev-parse", "--show-toplevel"]
  let barrier = "/tmp/tidepool-investigation-" <> Text.filter isAlphaNum (Text.pack (show owner))
  void $ turn owner $ Text.unlines
    [ "import Project.BackgroundInvestigator"
    , "import Project.ParallelInvestigate"
    , "let directory = " <> literal directory
    , "let barrier = " <> literal barrier
    , "let spec = FocusedSpec \"pending fixture\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
    , "let probe name command = CommandProbe name name directory (Cmd.MiB 64) command"
    , "let choose _ choices = pure (Right (case choices of { [] -> Nothing; first : _ -> Just first }))"
    , "barrierJob <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"mkfifo\", barrier]))"
    , "Cmd.await barrierJob"
    , "failed <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"exit 9\"]))"
    , "Cmd.await failed"
    , "investigator <- watchFailedCheck me \"pending diagnostic\" (FocusedRun spec failed) (const [probe \"waiting\" (Cmd.argv [\"sh\", \"-c\", \"read token < \\\"$1\\\"; printf done\", \"sh\", barrier]), probe \"second\" (Cmd.argv [\"printf\", \"second\"]), probe \"third\" (Cmd.argv [\"printf\", \"never\"])]) (\\_ choices -> pure (Right (case choices of { [] -> Nothing; first : _ -> Just first })))"
    ]
  pending <- awaitOutput owner
    "state <- readInvestigation investigator\nfmap followupStop (investigationReport state)"
    (Text.isInfixOf "FollowupStillRunning")
  check "pending diagnostic retains its exact original job" ("FollowupStillRunning" `Text.isInfixOf` pending)
  held <- turn owner "finishInvestigation investigator"
  check "cleanup refuses while an exact diagnostic job is pending"
    ("Left (InvestigationPending" `Text.isInfixOf` lastOutput held)
  wrong <- turn owner
    "state <- readInvestigation investigator\nlet Just report = investigationReport state\nlet FollowupStillRunning pendingJob = followupStop report\ninvalid <- resumeFollowupWith choose \"pending diagnostic\" pendingJob (investigationAvailable state) report\nfollowupStop invalid"
  check "recovery refuses a different original handle without probe submission"
    ("FollowupRecoveryMismatch" `Text.isInfixOf` lastOutput wrong)
  void $ turn owner
    "release <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"printf 'release\\n' > \\\"$1\\\"\", \"sh\", barrier]))\nCmd.await release"
  finished <- awaitOutput owner
    "state <- readInvestigation investigator\n(fmap followupStop (investigationReport state), fmap (length . diagnosticObservations) (investigationReport state), length (investigationFollowers state))"
    (Text.isInfixOf "FollowupBudgetSpent")
  check ("pending completion continues automatically and spends only two probes: " <> Text.take 260 finished)
    ("FollowupBudgetSpent" `Text.isInfixOf` finished && "Just 2" `Text.isInfixOf` finished)
  finish <- turn owner "finishInvestigation investigator"
  check "follower and report owner retire after terminal diagnosis"
    ("Right" `Text.isInfixOf` lastOutput finish)
  void $ turn owner
    "removed <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"rm\", \"--\", barrier]))\nCmd.await removed"
