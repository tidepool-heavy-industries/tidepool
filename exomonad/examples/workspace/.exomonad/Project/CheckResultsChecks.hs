{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.CheckResultsChecks (completionRouting, runningCommandCleanup) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check

-- A completed job is attached late; two other jobs settle through the same
-- record actor. The fixture uses the focused runner's retained JSON shape.
completionRouting :: Member RecipeCheck effects => Eff effects ()
completionRouting = do
  owner <- root
  let fixture = Text.pack workspaceRoot <> "/checks/focused-result-fixture.sh"
      setup = Text.unlines
        [ "let spec = FocusedSpec \"fixture check\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
        , "let fixture kind = Cmd.withMemory (Cmd.MiB 256) (Cmd.argv [\"bash\", " <> Text.pack (show fixture) <> ", kind])"
        , "late <- Cmd.start (fixture \"pass\")"
        ]
  void $ turn owner setup
  invalidCount <- turn owner
    "bad <- startFocused (Cmd.MiB 256) (spec { focusedExpected = 0 })\ncase bad of { Left (NonPositiveExpected 0) -> True; _ -> False }"
  check "zero expected tests are rejected before command submission" (lastOutput invalidCount == "True")
  invalidCheckout <- turn owner
    "bad <- startFocusedIn \"relative\" (Cmd.MiB 256) spec\ncase bad of { Left (NonAbsoluteCheckout \"relative\") -> True; _ -> False }"
  check "focused run refuses a relative checkout before submission" (lastOutput invalidCheckout == "True")
  invalid <- turn owner "invalid <- watchChecks me NotifySummary []\ncase invalid of { Left NoFocusedChecks -> True; _ -> False }"
  check "empty watcher has a typed refusal" (lastOutput invalid == "True")
  duplicate <- turn owner
    "duplicate <- watchChecks me NotifySummary [(\"same\", FocusedRun spec late), (\"same\", FocusedRun spec late)]\ncase duplicate of { Left (DuplicateCheckName \"same\") -> True; _ -> False }"
  check "duplicate watcher names have a typed refusal" (lastOutput duplicate == "True")
  void $ awaitOutput owner "Cmd.status late" (Text.isInfixOf "CommandFinished")
  void $ turn owner
    "failed <- Cmd.start (fixture \"fail\")\nunknown <- Cmd.start (fixture \"unknown\")\nRight watcher <- watchChecks me NotifyProblems [(\"late\", FocusedRun spec late), (\"failed\", FocusedRun spec failed), (\"unknown\", FocusedRun spec unknown)]"
  observed <- awaitOutput owner
    "view <- readChecks watcher\nmap (\\entry -> fmap (checkVerdict entry) (checkOutcome entry)) (checkEntries view)"
    (Text.isInfixOf "Just CheckUnknown")
  check "late completion, failed exit and missing evidence all settle"
    (all (`Text.isInfixOf` observed) ["Just CheckPassed", "Just CheckFailed", "Just CheckUnknown"])
  details <- turn owner
    "view <- readChecks watcher\n[(checkName e, fmap (Cmd.commandCleanup . checkCompletion) (checkOutcome e), fmap (focusedEvidence . checkFocused) (checkOutcome e)) | e <- checkEntries view]"
  check "completion keeps cleanup and parsed evidence separately"
    (all (`Text.isInfixOf` output details) ["CommandClean", "fixture-digest", "focused runner did not report"])
  mismatch <- turn owner
    "view <- readChecks watcher\nlet [passEntry, failEntry, _] = checkEntries view\nlet Just passOutcome = checkOutcome passEntry\nlet Just failOutcome = checkOutcome failEntry\nlet invalid = passOutcome { checkCompletion = checkCompletion failOutcome }\nlet FocusedRun _ wrongJob = checkRun failEntry\nlet Cmd.Finished _ result output = focusedCommand (checkFocused passOutcome)\nlet forged = (checkFocused passOutcome) { focusedCommand = Cmd.Finished wrongJob result output }\nlet invalidJob = passOutcome { checkFocused = forged }\n(checkExecution passEntry invalid, checkSourceAssurance passEntry invalid, checkExecution passEntry invalidJob, checkSourceAssurance passEntry invalidJob)"
  check "mismatched terminal receipt or job cannot verify execution or source"
    ("(ExecutionUnknown,SourceUnrecorded,ExecutionUnknown,SourceUnrecorded)"
      `Text.isInfixOf` Text.filter (/= ' ') (lastOutput mismatch))
  productFailure <- turn owner
    "view <- readChecks watcher\nlet [_, failedEntry, _] = checkEntries view\nlet Just failedOutcome = checkOutcome failedEntry\ndiagnosis <- diagnoseFocused (checkFocused failedOutcome)\n(diagnosisBranch diagnosis, diagnosisExcerpt diagnosis)"
  check "assertion failure diagnosis retains a bounded output excerpt"
    (all (`Text.isInfixOf` lastOutput productFailure) ["AssertionsFailed 0 1", "fixture diagnostic"])
  notices <- turn owner "length . checkNotices <$> readChecks watcher"
  -- The offline driver refuses delivery. These prove attempted, retained
  -- notifications and policy selection, not successful inbox delivery.
  check "problem policy records only failed and unknown notice attempts" (output notices == "2")
  void $ turn owner "finishChecks watcher"

  void $ turn owner
    "Right summarizer <- watchChecks me NotifySummary [(\"late\", FocusedRun spec late), (\"failed\", FocusedRun spec failed)]"
  summary <- awaitOutput owner
    "view <- readChecks summarizer\nif length (checkNotices view) == 1 then checksSummary view else \"pending\""
    (Text.isInfixOf "late passed")
  check "aggregate policy records one named summary attempt after late completions"
    (all (`Text.isInfixOf` summary) ["late passed", "failed failed"])
  void $ turn owner "finishChecks summarizer"

  void $ turn owner
    "dirty <- Cmd.start (fixture \"dirty\")\nRight dirtyWatcher <- watchChecks me NotifyProblems [(\"dirty\", FocusedRun spec dirty)]"
  dirty <- awaitOutput owner
    "dirtyView <- readChecks dirtyWatcher\nchecksSummary dirtyView"
    (Text.isInfixOf "source dirty")
  check "executed pass remains visible when source assurance is dirty"
    (all (`Text.isInfixOf` dirty) ["dirty unknown", "executed 1 passed", "source dirty"])
  void $ turn owner "finishChecks dirtyWatcher"

  void $ turn owner
    "missing <- Cmd.start (fixture \"missingfile\")\nRight missingWatcher <- watchChecks me NotifyProblems [(\"missing\", FocusedRun spec missing)]"
  missing <- awaitOutput owner
    "missingView <- readChecks missingWatcher\n[(checkVerdict e outcome, focusedEvidence (checkFocused outcome)) | e <- checkEntries missingView, Just outcome <- [checkOutcome e]]"
    (Text.isInfixOf "cat receipt")
  check "failed evidence read remains unknown with its command receipt"
    (all (`Text.isInfixOf` missing) ["CheckUnknown", "cat receipt"])
  void $ turn owner "finishChecks missingWatcher"

  setup <- turn owner
    "zeroJob <- Cmd.start (fixture \"zero\")\nzeroResult <- collectFocused (FocusedRun spec zeroJob)\nzeroDiagnosis <- diagnoseFocused zeroResult\nsetupJob <- Cmd.start (fixture \"setup\")\nsetupResult <- collectFocused (FocusedRun spec setupJob)\nsetupDiagnosis <- diagnoseFocused setupResult\nshortJob <- Cmd.start (fixture \"short\")\nshortResult <- collectFocused (FocusedRun spec shortJob)\nshortDiagnosis <- diagnoseFocused shortResult\n(diagnosisBranch zeroDiagnosis, diagnosisBranch setupDiagnosis, focusedExecution shortResult, diagnosisBranch shortDiagnosis)"
  check "zero selection, incomplete setup, and short execution stay distinct"
    ("(ZeroSelection,SetupIncomplete,ExecutionUnknown,SetupIncomplete)"
      `Text.isInfixOf` Text.filter (/= ' ') (lastOutput setup))

-- The driver must service a live command's cleanup receipt while its resident
-- forest stops. A successful restart proves the old producer was sealed.
runningCommandCleanup :: Member RecipeCheck effects => Eff effects ()
runningCommandCleanup = do
  owner <- root
  void $ turn owner
    "live <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sleep\", \"30\"]))"
  running <- awaitOutput owner "Cmd.status live" (Text.isInfixOf "CommandRunning")
  check "command is running before resident shutdown" ("CommandRunning" `Text.isInfixOf` running)
  identity <- restart
  check "live command retirement sealed its producer" (not (Text.null identity))
