{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Project.CheckResultsChecks
  ( preparedCompletion, completionRouting, managedEvidence, runningCommandCleanup
  , EvidenceProbe (probeStart, probePrepared, probeRead, probeMove), evidenceProbe
  ) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import qualified Tidepool.Command as Cmd
import Tidepool.Check
import Tidepool.Effects.Core (Actor, Commands)
import Tidepool.Effects.Row (knownEffects)
import Tidepool.Worktree (createWorktree, fromCurrentRepository, worktreeId)
import Project.CheckResults

-- Preparation and test execution share the original command. These checks
-- exercise failure classification without making Jev infer a prerequisite.
preparedCompletion :: Member RecipeCheck effects => Eff effects ()
preparedCompletion = do
  owner <- root
  concrete <- turn owner "import SessionHelpers\nlet candidateOid = \"0123456789abcdef0123456789abcdef01234567\" :: GitOid\nconcreteGate <- runCheck me candidateOid (Cmd.MiB 64) (CheckDefinition \"concrete invocation\" \"fixture\" \"lib\" \"one\" 0)\nconcreteGate"
  check "session entrypoint infers effects without a cell annotation" ("NonPositiveExpected 0" `Text.isInfixOf` lastOutput concrete)
  void $ turn owner "let spec = FocusedSpec \"prepared fixture\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
  void $ turn owner "Right failedPreparation <- startFocusedAfter (Cmd.MiB 256) spec [\"sh\", \"-c\", \"echo missing-assets >&2; exit 9\"]"
  void $ awaitOutput owner "Cmd.status (runJob failedPreparation)" (Text.isInfixOf "CommandFinished")
  failure <- turn owner "preparedFailure <- collectFocused failedPreparation\npreparedDiagnosis <- diagnoseFocused preparedFailure\n(focusedPreparation preparedFailure, focusedEvidencePath preparedFailure, diagnosisBranch preparedDiagnosis, focusedExecution preparedFailure)"
  check "failed preparation prevents test execution and retains its own classification"
    (all (`Text.isInfixOf` lastOutput failure) ["PreparationFailed 9", "Nothing", "SetupIncomplete", "ExecutionUnknown"])
  void $ turn owner "Right successfulPreparation <- startFocusedAfter (Cmd.MiB 256) spec [\"sh\", \"-c\", \"printf prepared\"]"
  void $ awaitOutput owner "Cmd.status (runJob successfulPreparation)" (Text.isInfixOf "CommandFinished")
  success <- turn owner "preparedSuccess <- collectFocused successfulPreparation\nfocusedPreparation preparedSuccess"
  check "successful preparation is retained separately from the check result" (lastOutput success == "PreparationPassed")
  void $ turn owner "interrupted <- Cmd.start (Cmd.withStdin (Cmd.argv [\"sh\", \"-c\", \"read line\"]))\nCmd.cancel interrupted"
  void $ awaitOutput owner "Cmd.status interrupted" (Text.isInfixOf "CommandFinished")
  cancelled <- turn owner "cancelledResult <- collectFocused (PreparedFocusedRun spec interrupted)\n(focusedPreparation cancelledResult, focusedPassed cancelledResult)"
  check "interrupted preparation never claims success" (all (`Text.isInfixOf` lastOutput cancelled) ["PreparationUnknown", "False"])

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
    (\text -> Text.count "Just " text == 3)
  check ("late completion, failed exit and missing evidence all settle: " <> observed)
    (all (`Text.isInfixOf` observed) ["Just CheckPassed", "Just CheckFailed", "Just CheckUnknown"])
  details <- turn owner
    "view <- readChecks watcher\n[(Project.CheckResults.checkName e, fmap (Cmd.commandCleanup . checkCompletion) (checkOutcome e), fmap (focusedEvidence . checkFocused) (checkOutcome e)) | e <- checkEntries view]"
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
    (Text.isInfixOf "late: passed")
  check "aggregate policy records one named summary attempt after late completions"
    (all (`Text.isInfixOf` summary) ["late: passed", "failed: failed", "matched 1", "runnable 1", "CommandExited 0", "CommandClean", "; artifact /"])
  void $ turn owner "finishChecks summarizer"

  void $ turn owner
    "dirty <- Cmd.start (fixture \"dirty\")\nRight dirtyWatcher <- watchChecks me NotifyProblems [(\"dirty\", FocusedRun spec dirty)]"
  dirty <- awaitOutput owner
    "dirtyView <- readChecks dirtyWatcher\nchecksSummary dirtyView"
    (Text.isInfixOf "source dirty")
  check "executed pass remains visible when source assurance is dirty"
    (all (`Text.isInfixOf` dirty) ["dirty: unknown", "executed 1 passed", "source dirty"])
  void $ turn owner "finishChecks dirtyWatcher"

  void $ turn owner
    "missing <- Cmd.start (fixture \"missingfile\")\nRight missingWatcher <- watchChecks me NotifyProblems [(\"missing\", FocusedRun spec missing)]"
  missing <- awaitOutput owner
    "missingView <- readChecks missingWatcher\n[(checkVerdict e outcome, focusedEvidence (checkFocused outcome)) | e <- checkEntries missingView, Just outcome <- [checkOutcome e]]"
    (Text.isInfixOf "did not retain its evidence record")
  check "missing embedded evidence stays unknown without a second file read"
    (all (`Text.isInfixOf` missing) ["CheckUnknown", "did not retain its evidence record"])
  void $ turn owner "finishChecks missingWatcher"

  setup <- turn owner
    "zeroJob <- Cmd.start (fixture \"zero\")\nzeroResult <- collectFocused (FocusedRun spec zeroJob)\nzeroDiagnosis <- diagnoseFocused zeroResult\nsetupJob <- Cmd.start (fixture \"setup\")\nsetupResult <- collectFocused (FocusedRun spec setupJob)\nsetupDiagnosis <- diagnoseFocused setupResult\nshortJob <- Cmd.start (fixture \"short\")\nshortResult <- collectFocused (FocusedRun spec shortJob)\nshortDiagnosis <- diagnoseFocused shortResult\n(diagnosisBranch zeroDiagnosis, diagnosisBranch setupDiagnosis, focusedExecution shortResult, diagnosisBranch shortDiagnosis)"
  check "zero selection, incomplete setup, and short execution stay distinct"
    ("(ZeroSelection,SetupIncomplete,ExecutionUnknown,SetupIncomplete)"
      `Text.isInfixOf` Text.filter (/= ' ') (lastOutput setup))

data EvidenceProbe mode = EvidenceProbe
  { probeState :: mode :- State ()
  , probeStart :: mode :- Call () (R.Reply FocusedRun)
  , probePrepared :: mode :- Call () (R.Reply (Either FocusedSetupIssue FocusedRun))
  , probeRead :: mode :- Call Text (R.Reply Text)
  , probeMove :: mode :- Call (Text, Text) (R.Reply Text)
  } deriving Generic

type EvidenceProbeEffects = LocalEffects EvidenceProbe '[Replies, Commands]

evidenceProbe :: FocusedSpec -> ActorSpec EvidenceProbe EvidenceProbeEffects
evidenceProbe spec =
  R.definition "focused-evidence-probe" (Actor.Selected knownEffects) EvidenceProbe
    { probeState = ()
    , probePrepared = \() -> do
        let checks = Text.pack workspaceRoot <> "/checks/"
        setup <- Cmd.run (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv
          ["bash", checks <> "prepared-focused-setup.sh", checks <> "focused-result-fixture.sh"]))
        case Cmd.failure setup of
          Just detail -> pure (Left (FocusedStartRefused (Cmd.CommandInvalid detail)))
          Nothing -> startFocusedAfter (Cmd.MiB 64) spec ["sh", "-c", "printf ready > .prepared-here"]
    , probeStart = \() -> FocusedRun spec <$> Cmd.start
        (Cmd.withMemory (Cmd.MiB 64)
          (Cmd.argv ["bash", Text.pack workspaceRoot <> "/checks/focused-result-fixture.sh", "pass", "managed"]))
    , probeRead = \path -> do
        started <- Cmd.tryStart (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv ["cat", path]))
        case started of
          Left issue -> pure ("start refused: " <> Text.pack (show issue))
          Right job -> do
            observed <- Cmd.await job
            pure (Text.pack (show (Cmd.commandResult observed)))
    , probeMove = \(from, to) -> do
        started <- Cmd.tryStart (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv ["mv", from, to]))
        case started of
          Left issue -> pure ("start refused: " <> Text.pack (show issue))
          Right job -> do
            observed <- Cmd.await job
            pure (Text.pack (show (Cmd.commandResult observed)))
    }

-- The bound actor runs a check in an actual managed writable checkout. The
-- completion watcher uses the record emitted by the original job even after
-- the artifact moves, and an unbound actor cannot mutate that checkout.
managedEvidence :: Member RecipeCheck effects => Eff effects ()
managedEvidence = do
  owner <- root
  void $ turn owner "import Project.CheckResultsChecks"
  created <- turn owner "Right focusedTree <- createWorktree (fromCurrentRepository \"focused-evidence-check\")\nworktreeId focusedTree"
  check "a managed checkout is allocated for focused evidence" ("WorktreeId" `Text.isInfixOf` lastOutput created)
  void $ turn owner $ Text.unlines
    [ "let managedSpec = FocusedSpec \"managed fixture\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
    , "boundProbe <- R.start (R.withWorktree (worktreeId focusedTree) (evidenceProbe managedSpec))"
    , "managedRun <- R.call (probeStart (R.client boundProbe)) ()"
    , "Right managedWatcher <- watchChecks me NotifyAllTerminal [(\"managed\", managedRun)]"
    ]
  void $ awaitOutput owner
    "view <- readChecks managedWatcher\n[fmap (checkVerdict e) (checkOutcome e) | e <- checkEntries view]"
    (Text.isInfixOf "Just Check")
  verified <- turn owner
    "view <- readChecks managedWatcher\n(checksSummary view, [(fmap (Cmd.stderr . focusedCommand . checkFocused) (checkOutcome e), fmap (focusedEvidence . checkFocused) (checkOutcome e), fmap checkCompletion (checkOutcome e)) | e <- checkEntries view])"
  let observed = lastOutput verified
  check ("watcher reports selected and executed facts from the original managed job: "
      <> Text.take 1200 observed)
    (all (`Text.isInfixOf` observed)
      ["managed: passed", "matched 1", "runnable 1", "executed 1 passed", "fixture-source", "CommandExited 0", "CommandClean", "; artifact /"])
  accessible <- turn owner $ Text.unlines
    [ "managedResult <- collectFocused managedRun"
    , "let Just managedPath = focusedEvidencePath managedResult"
    , "R.call (probeRead (R.client boundProbe)) managedPath"
    ]
  check "the artifact exists in the managed owner's writable checkout"
    ("CommandExited 0" `Text.isInfixOf` lastOutput accessible)
  moved <- turn owner $ Text.unlines
    [ "let Just managedPath = focusedEvidencePath managedResult"
    , "R.call (probeMove (R.client boundProbe)) (managedPath, managedPath <> \".retained\")"
    ]
  check "the managed owner moves the artifact after the job retains evidence"
    ("CommandExited 0" `Text.isInfixOf` lastOutput moved)
  refused <- turn owner $ Text.unlines
    [ "let Just managedPath = focusedEvidencePath managedResult"
    , "unboundProbe <- R.start (evidenceProbe managedSpec)"
    , "R.call (probeMove (R.client unboundProbe)) (managedPath <> \".retained\", managedPath)"
    ]
  check "an unbound actor cannot mutate the managed checkout artifact"
    (any (`Text.isInfixOf` lastOutput refused) ["CommandExited 1", "CommandUnauthorized"])
  retained <- turn owner
    "afterMove <- collectFocused managedRun\n(focusedExecution afterMove, focusedEvidence afterMove, focusedEvidencePath afterMove)"
  check "the original retained job still proves execution after its artifact moves"
    (all (`Text.isInfixOf` lastOutput retained) ["ExecutionPassed 1", "fixture-digest", "Just"])
  void $ turn owner "Right preparedRun <- R.call (probePrepared (R.client boundProbe)) ()\nRight preparedWatcher <- watchChecks me NotifyAllTerminal [(\"prepared\", preparedRun)]"
  prepared <- awaitOutput owner "checksSummary <$> readChecks preparedWatcher" (Text.isInfixOf "prepared: passed")
  check "preparation writes and test reads in the same managed checkout"
    (all (`Text.isInfixOf` prepared) ["prepared: passed", "PreparationPassed", "executed 1 passed", "CommandClean"])
  unknown <- turn owner "preparedResult <- collectFocused preparedRun\nfocusedPassed (preparedResult { focusedPreparation = PreparationUnknown })"
  check "unconfirmed preparation cannot be accepted even with passing test evidence" (lastOutput unknown == "False")
  void $ turn owner "finishChecks preparedWatcher"
  void $ turn owner "finishChecks managedWatcher\nR.finish boundProbe\nR.finish unboundProbe"

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
