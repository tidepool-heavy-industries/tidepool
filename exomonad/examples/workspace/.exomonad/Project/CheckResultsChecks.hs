{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Project.CheckResultsChecks
  ( preparedCompletion, completionRouting, retainedRecovery, managedEvidence, runningCommandCleanup
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
import Exomonad.Contrib.CheckResults

-- Preparation and test execution share the original command. These checks
-- exercise failure classification without making Jev infer a prerequisite.
preparedCompletion :: Member RecipeCheck effects => Eff effects ()
preparedCompletion = do
  owner <- root
  void $ turn owner "import SessionHelpers\nlet candidateOid = \"0123456789abcdef0123456789abcdef01234567\" :: GitOid\nconcreteGate <- runCheck me candidateOid (Cmd.MiB 64) (CheckDefinition \"concrete invocation\" \"fixture\" \"lib\" \"one\" 0)\nconcreteGate"
  assertCell owner "session entrypoint infers effects without cell annotation"
    "case concreteGate of { GateSetupRefused (NonPositiveExpected 0) -> True; _ -> False }"
  void $ turn owner "let spec = FocusedSpec \"prepared fixture\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
  void $ turn owner "Right failedPreparation <- startFocusedAfterWith [\"scripts/cargo-focused-test\"] (Cmd.MiB 256) spec [\"sh\", \"-c\", \"echo missing-assets >&2; exit 9\"]"
  void $ turn owner "Cmd.await (runJob failedPreparation)"
  void $ turn owner "preparedFailure <- collectFocused failedPreparation\npreparedDiagnosis <- diagnoseFocused preparedFailure\n(focusedPreparation preparedFailure, focusedEvidencePath preparedFailure, diagnosisBranch preparedDiagnosis, focusedExecution preparedFailure)"
  assertCell owner "failed preparation prevents tests retaining exact classification"
    "focusedPreparation preparedFailure == PreparationFailed 9 && focusedEvidencePath preparedFailure == Nothing && diagnosisBranch preparedDiagnosis == SetupIncomplete && focusedExecution preparedFailure == ExecutionUnknown && Cmd.completedJob (focusedCommand preparedFailure) == runJob failedPreparation"
  void $ turn owner "Right successfulPreparation <- startFocusedAfterWith [\"scripts/cargo-focused-test\"] (Cmd.MiB 256) spec [\"sh\", \"-c\", \"printf prepared\"]"
  void $ turn owner "Cmd.await (runJob successfulPreparation)"
  void $ turn owner "preparedSuccess <- collectFocused successfulPreparation\nfocusedPreparation preparedSuccess"
  assertCell owner "successful preparation remains separate from check result"
    "focusedPreparation preparedSuccess == PreparationPassed && Cmd.completedJob (focusedCommand preparedSuccess) == runJob successfulPreparation"
  void $ turn owner "interrupted <- Cmd.background (Cmd.withStdin (Cmd.argv [\"sh\", \"-c\", \"read line\"]))\nCmd.cancel interrupted"
  void $ turn owner "Cmd.await interrupted"
  void $ turn owner "cancelledResult <- collectFocused (PreparedFocusedRun spec interrupted)\n(focusedPreparation cancelledResult, focusedPassed cancelledResult)"
  assertCell owner "interrupted preparation never claims success"
    "focusedPreparation cancelledResult == PreparationUnknown && not (focusedPassed cancelledResult) && Cmd.completedJob (focusedCommand cancelledResult) == interrupted"
-- A completed job is attached late; two other jobs settle through the same
-- record actor. The fixture uses the focused runner's retained JSON shape.
completionRouting :: Member RecipeCheck effects => Eff effects ()
completionRouting = do
  owner <- root
  let fixture = Text.pack workspaceRoot <> "/checks/focused-result-fixture.sh"
      setup = Text.unlines
        [ "let spec = FocusedSpec \"fixture check\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
        , "let fixture kind = Cmd.withMemory (Cmd.MiB 256) (Cmd.argv [\"bash\", " <> Text.pack (show fixture) <> ", kind])"
        , "late <- Cmd.background (fixture \"pass\")"
        ]
  void $ turn owner setup
  void $ turn owner
    "let zero = PlanCheck { planName = \"zero\", planRunner = [\"scripts/cargo-focused-test\"], planSpec = const (spec { focusedExpected = 0 }), planMemory = Cmd.MiB 64, planPreparation = WithoutPreparation }\nlet empty = PlanCheck { planName = \"empty preparation\", planRunner = [\"scripts/cargo-focused-test\"], planSpec = const spec, planMemory = Cmd.MiB 64, planPreparation = PrepareWith (const []) }\nRight refusedPlan <- startCheckPlan me (\"0123456789abcdef0123456789abcdef01234567\" :: GitOid) [zero, empty]\nrefusedReport <- readCheckPlan refusedPlan\n(planPassed refusedReport, planSummary refusedReport, planWatcher refusedPlan)"
  assertCell owner "plan keeps all setup refusals admitting no command or watcher"
    "not (planPassed refusedReport) && case (planStarts refusedPlan, planWatcher refusedPlan, planState refusedReport) of { ([(\"zero\", Left (NonPositiveExpected 0)), (\"empty preparation\", Left EmptyPreparation)], Nothing, Nothing) -> True; _ -> False }"
  void $ turn owner
    "duplicate <- startCheckPlan me (\"0123456789abcdef0123456789abcdef01234567\" :: GitOid) [zero, zero]\ncase duplicate of { Left (DuplicateCheckName \"zero\") -> True; _ -> False }"
  assertCell owner "duplicate plan refuses before submission"
    "case duplicate of { Left (DuplicateCheckName \"zero\") -> True; _ -> False }"
  void $ turn owner
    "bad <- startFocusedWith [\"scripts/cargo-focused-test\"] (Cmd.MiB 256) (spec { focusedExpected = 0 })\ncase bad of { Left (NonPositiveExpected 0) -> True; _ -> False }"
  assertCell owner "zero expected tests refuse before submission"
    "case bad of { Left (NonPositiveExpected 0) -> True; _ -> False }"
  void $ turn owner
    "bad <- startFocusedWith [] (Cmd.MiB 256) spec\ncase bad of { Left EmptyRunner -> True; _ -> False }"
  assertCell owner "empty runner refuses before submission"
    "case bad of { Left EmptyRunner -> True; _ -> False }"
  void $ turn owner $ Text.unlines
    [ "let runner = [\"bash\", " <> Text.pack (show fixture) <> ", \"argv\", \"literal runner ; $(false) \\\"quotes\\\"\"]"
    , "let preparation = [\"sh\", \"-c\", \"test \\\"$1\\\" = 'literal preparation ; $(false)'\", \"focused-preparation\", \"literal preparation ; $(false)\"]"
    , "Right configured <- startFocusedAfterWith runner (Cmd.MiB 256) spec preparation"
    , "configuredResult <- collectFocused configured"
    , "(focusedPassed configuredResult, focusedPreparation configuredResult)"
    ]
  assertCell owner "runner and preparation argv retain literal spaces, quotes and shell syntax"
    "focusedPassed configuredResult && focusedPreparation configuredResult == PreparationPassed"
  void $ turn owner
    "bad <- startFocusedInWith [\"scripts/cargo-focused-test\"] \"relative\" (Cmd.MiB 256) spec\ncase bad of { Left (NonAbsoluteCheckout \"relative\") -> True; _ -> False }"
  assertCell owner "relative checkout refuses before submission"
    "case bad of { Left (NonAbsoluteCheckout \"relative\") -> True; _ -> False }"
  void $ turn owner "invalid <- watchChecks me NotifySummary []\ncase invalid of { Left NoFocusedChecks -> True; _ -> False }"
  assertCell owner "empty watcher returns typed refusal"
    "case invalid of { Left NoFocusedChecks -> True; _ -> False }"
  void $ turn owner
    "duplicate <- watchChecks me NotifySummary [(\"same\", FocusedRun spec late), (\"same\", FocusedRun spec late)]\ncase duplicate of { Left (DuplicateCheckName \"same\") -> True; _ -> False }"
  assertCell owner "duplicate watcher names return typed refusal"
    "case duplicate of { Left (DuplicateCheckName \"same\") -> True; _ -> False }"
  void $ turn owner "Cmd.await late"
  void $ turn owner
    "recovered <- reopenGate me \"recovered\" (FocusedRun spec late)\ncase recovered of { GateWatching run _ -> runJob run == late; _ -> False }"
  assertCell owner "later cell reattaches original job without resubmission"
    "case recovered of { GateWatching run _ -> runJob run == late && runSpec run == spec; _ -> False }"
  awaitCell owner "reopened completed job keeps its terminal evidence"
    "case recovered of { GateWatching run handle -> do { state <- readChecks handle; pure (case checkEntries state of { [entry] -> runJob (checkRun entry) == runJob run && case checkOutcome entry of { Just outcome -> checkVerdict entry outcome == CheckPassed && checkEvidenceComplete entry outcome; _ -> False }; _ -> False }) }; _ -> pure False }"
  void $ turn owner
    "original <- collectFocused (FocusedRun spec late)\n(focusedPassed original, Cmd.completedJob (focusedCommand original) == late, focusedPreparation original)"
  assertCell owner "direct recovery reads original completion and preparation state"
    "focusedPassed original && Cmd.completedJob (focusedCommand original) == late && focusedPreparation original == NoPreparation"
  void $ turn owner
    "original <- collectFocused (FocusedRun spec late)\nfocusedResultSummary (FocusedRun spec late) original"
  assertCell owner "text: recovered packet names original job and both retained streams"
    "all (\\part -> part `T.isInfixOf` (focusedResultSummary (FocusedRun spec late) original)) [\"original job\", \"requested source fixture-source\", \"recorded source fixture-source\", \"output refs Stdout\", \"Stderr\", \"executed 1 passed\"]"
  void $ turn owner
    "original <- collectFocused (FocusedRun spec late)\nlet Cmd.Finished job _ output = focusedCommand original\nlet retained = original { focusedCommand = Cmd.Finished job (Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandRetained) output }\n(focusedExecution retained, focusedPassed retained)"
  assertCell owner "passing original check with retained cleanup cannot be accepted"
    "focusedExecution retained == ExecutionPassed 1 && not (focusedPassed retained)"
  void $ turn owner
    "original <- collectFocused (PreparedFocusedRun spec late)\n(focusedPreparation original, focusedPassed original)"
  assertCell owner "recovery never claims unrecorded preparation passed"
    "focusedPreparation original == PreparationUnknown && not (focusedPassed original)"
  void $ turn owner
    "wrong <- collectFocused (FocusedRun (spec { focusedSource = \"other-source\" }) late)\n(focusedSourceAssurance wrong, focusedPassed wrong)"
  assertCell owner "original job cannot prove another source"
    "focusedSourceAssurance wrong == SourceDifferent (Just \"fixture-source\") && not (focusedPassed wrong) && Cmd.completedJob (focusedCommand wrong) == late"
  void $ turn owner
    "wrong <- collectFocused (FocusedRun (spec { focusedSource = \"other-source\" }) late)\nfocusedResultSummary (FocusedRun (spec { focusedSource = \"other-source\" }) late) wrong"
  assertCell owner "text: wrong-source packet names requested and recorded source"
    "all (\\part -> part `T.isInfixOf` (focusedResultSummary (FocusedRun (spec { focusedSource = \"other-source\" }) late) wrong)) [\"requested source other-source\", \"recorded source fixture-source\", \"source different\"]"
  void $ turn owner
    "Right partialWatcher <- watchChecksWithRefusals me NotifySummary [(\"refused\", NonPositiveExpected 0)] [(\"late\", FocusedRun spec late)]"
  awaitCell owner "partial admission retains original pass while refusing whole plan"
    "do { partialState <- readChecks partialWatcher; let { partialPlan = PlanStart [(\"refused\", Left (NonPositiveExpected 0)), (\"late\", Right (FocusedRun spec late))] (Just (Right partialWatcher)); partialReport = PlanReport partialPlan (Just partialState) }; pure (not (planPassed partialReport) && length (checkNotices partialState) == 1 && case checkEntries partialState of { [entry] -> runJob (checkRun entry) == late && case checkOutcome entry of { Just outcome -> checkVerdict entry outcome == CheckPassed && checkEvidenceComplete entry outcome; _ -> False }; _ -> False }) }"
  void $ turn owner "finishChecks partialWatcher"
  void $ turn owner "case recovered of { GateWatching _ handle -> finishChecks handle >> pure (); _ -> pure () }"
  void $ turn owner
    "failed <- Cmd.background (fixture \"fail\")\nunknown <- Cmd.background (fixture \"unknown\")\nRight watcher <- watchChecks me NotifyProblems [(\"late\", FocusedRun spec late), (\"failed\", FocusedRun spec failed), (\"unknown\", FocusedRun spec unknown)]"
  awaitCell owner "late completion, failed exit and missing evidence all settle"
    "do { view <- readChecks watcher; pure (map (\\entry -> fmap (checkVerdict entry) (checkOutcome entry)) (checkEntries view) == [Just CheckPassed, Just CheckFailed, Just CheckUnknown]) }"
  void $ turn owner
    "view <- readChecks watcher\n[(Exomonad.Contrib.CheckResults.checkName e, fmap (Cmd.commandCleanup . checkCompletion) (checkOutcome e), fmap (focusedEvidence . checkFocused) (checkOutcome e)) | e <- checkEntries view]"
  assertCell owner "completion keeps cleanup and parsed evidence separately"
    "case checkEntries view of { [passed, failed, unknown] -> all (\\entry -> case checkOutcome entry of { Just outcome -> Cmd.commandCleanup (checkCompletion outcome) == Cmd.CommandClean; _ -> False }) [passed,failed,unknown] && all (\\entry -> case checkOutcome entry of { Just outcome -> case focusedEvidence (checkFocused outcome) of { Right record -> recordDigest record == \"fixture-digest\"; _ -> False }; _ -> False }) [passed,failed] && case checkOutcome unknown of { Just outcome -> case focusedEvidence (checkFocused outcome) of { Left _ -> True; _ -> False }; _ -> False }; _ -> False }"
  void $ turn owner
    "view <- readChecks watcher\nlet [passEntry, failEntry, _] = checkEntries view\nlet Just passOutcome = checkOutcome passEntry\nlet Just failOutcome = checkOutcome failEntry\nlet invalid = passOutcome { checkCompletion = checkCompletion failOutcome }\nlet FocusedRun _ wrongJob = checkRun failEntry\nlet Cmd.Finished _ result output = focusedCommand (checkFocused passOutcome)\nlet forged = (checkFocused passOutcome) { focusedCommand = Cmd.Finished wrongJob result output }\nlet invalidJob = passOutcome { checkFocused = forged }\n(checkExecution passEntry invalid, checkSourceAssurance passEntry invalid, checkExecution passEntry invalidJob, checkSourceAssurance passEntry invalidJob)"
  assertCell owner "mismatched terminal receipt or job cannot verify execution or source"
    "all (\\outcome -> checkExecution passEntry outcome == ExecutionUnknown && checkSourceAssurance passEntry outcome == SourceUnrecorded) [invalid, invalidJob]"
  void $ turn owner
    "view <- readChecks watcher\nlet [passEntry, failEntry, unknownEntry] = checkEntries view\nlet Just passOutcome = checkOutcome passEntry\nlet Just failOutcome = checkOutcome failEntry\nlet Just unknownOutcome = checkOutcome unknownEntry\n(checkEvidenceComplete passEntry passOutcome, checkEvidenceComplete failEntry failOutcome, checkEvidenceComplete unknownEntry unknownOutcome, checkEvidenceComplete passEntry failOutcome, checkEvidenceComplete failEntry (failOutcome { checkCompletion = checkCompletion passOutcome }))"
  assertCell owner "counted success and assertion failure prove only original jobs"
    "checkEvidenceComplete passEntry passOutcome && checkEvidenceComplete failEntry failOutcome && not (checkEvidenceComplete unknownEntry unknownOutcome) && not (checkEvidenceComplete passEntry failOutcome) && not (checkEvidenceComplete failEntry (failOutcome { checkCompletion = checkCompletion passOutcome }))"
  void $ turn owner
    "original <- collectFocused (FocusedRun spec late)\nlet Right record = focusedEvidence original\nlet changed record = original { focusedEvidence = Right record }\nlet selections = [record { recordMatched = Nothing }, record { recordMatched = Just [\"other::one\"] }, record { recordMatched = Just [\"fixture::one\", \"fixture::one\"] }]\n(map (focusedExecution . changed) selections, map (focusedPassed . changed) selections)"
  assertCell owner "execution counts remain separate from malformed selection proof"
    "map (focusedExecution . changed) selections == replicate 3 (ExecutionPassed 1) && all (not . focusedPassed . changed) selections"
  void $ turn owner
    "original <- collectFocused (FocusedRun spec late)\nlet Right record = focusedEvidence original\nlet duplicate = original { focusedSpec = spec { focusedExpected = 2 }, focusedEvidence = Right (record { recordMatched = Just [\"fixture::one\", \"fixture::one\"], recordRunnable = Just [\"fixture::one\", \"fixture::one\"], recordSummaries = Just [[2,0,0,0,0]] }) }\n(focusedExecution duplicate, focusedEvidenceComplete duplicate, focusedPassed duplicate)"
  assertCell owner "duplicate names and counts cannot prove two distinct requested tests"
    "focusedExecution duplicate == ExecutionPassed 2 && not (focusedEvidenceComplete duplicate) && not (focusedPassed duplicate)"
  void $ turn owner
    "view <- readChecks watcher\nlet [_, entry, _] = checkEntries view\nlet Just outcome = checkOutcome entry\nlet focused = checkFocused outcome\nlet Right record = focusedEvidence focused\nlet Cmd.Finished job receipt output = focusedCommand focused\nlet retainedReceipt = receipt { Cmd.commandCleanup = Cmd.CommandRetained }\nlet changed record = outcome { checkFocused = focused { focusedEvidence = Right record } }\nlet retained = outcome { checkCompletion = retainedReceipt, checkFocused = focused { focusedCommand = Cmd.Finished job retainedReceipt output } }\nlet unknownPreparation = outcome { checkFocused = focused { focusedPreparation = PreparationUnknown } }\nmap (checkEvidenceComplete entry) [changed (record { recordMatched = Just [\"other::one\"] }), changed (record { recordExitCode = Just 2 }), changed (record { recordSource = Just \"other-source\" }), changed (record { recordSummaries = Just [[0,0,0,0,0]] }), retained, unknownPreparation]"
  assertCell owner "failure proof needs selection, exit, exact source, counts, cleanup and preparation"
    "all (not . checkEvidenceComplete entry) [changed (record { recordMatched = Just [\"other::one\"] }), changed (record { recordExitCode = Just 2 }), changed (record { recordSource = Just \"other-source\" }), changed (record { recordSummaries = Just [[0,0,0,0,0]] }), retained, unknownPreparation]"
  void $ turn owner
    "view <- readChecks watcher\nlet [_, entry, _] = checkEntries view\nlet Just outcome = checkOutcome entry\nlet focused = checkFocused outcome\nlet Cmd.Finished job receipt _ = focusedCommand focused\nlet unavailable = focused { focusedCommand = Cmd.Finished job receipt (Left (Cmd.CommandUnavailable \"fixture capture refused\")) }\ndiagnosis <- diagnoseFocused unavailable\n(diagnosisBranch diagnosis, diagnosisExcerpt diagnosis, Cmd.stderr (focusedCommand unavailable))"
  assertCell owner "unavailable command capture retains refusal independently of counted failure"
    "diagnosisBranch diagnosis == AssertionsFailed 0 1 && case (diagnosisExcerpt diagnosis, Cmd.stderr (focusedCommand unavailable)) of { (Left _, Left (Cmd.OutputUnavailable refusedJob (Cmd.CommandUnavailable detail))) -> refusedJob == job && detail == \"fixture capture refused\"; _ -> False }"
  void $ turn owner
    "view <- readChecks watcher\nlet [_, failedEntry, _] = checkEntries view\nlet Just failedOutcome = checkOutcome failedEntry\ndiagnosis <- diagnoseFocused (checkFocused failedOutcome)\n(diagnosisBranch diagnosis, diagnosisExcerpt diagnosis)"
  assertCell owner "text: assertion failure diagnosis retains bounded diagnostic excerpt"
    "diagnosisBranch diagnosis == AssertionsFailed 0 1 && case diagnosisExcerpt diagnosis of { Right excerpt -> T.length excerpt <= 8192 && \"fixture diagnostic\" `T.isInfixOf` excerpt; _ -> False }"
  awaitCell owner "problem policy keeps only failed and unknown notice attempts"
    "do { view <- readChecks watcher; pure (map checkNoticeName (checkNotices view) == [Just \"failed\", Just \"unknown\"] || map checkNoticeName (checkNotices view) == [Just \"unknown\", Just \"failed\"]) }"
  void $ turn owner "finishChecks watcher"

  void $ turn owner
    "Right summarizer <- watchChecks me NotifySummary [(\"late\", FocusedRun spec late), (\"failed\", FocusedRun spec failed)]"
  awaitCell owner "aggregate policy keeps one summary attempt after original completions"
    "do { view <- readChecks summarizer; pure (length (checkNotices view) == 1 && map checkNoticeName (checkNotices view) == [Nothing] && map (\\entry -> fmap (checkVerdict entry) (checkOutcome entry)) (checkEntries view) == [Just CheckPassed, Just CheckFailed] && all (\\entry -> case checkOutcome entry of { Just outcome -> checkEvidenceComplete entry outcome; _ -> False }) (checkEntries view)) }"
  void $ turn owner
    "import Project.HandoffExamples\nview <- readChecks summarizer\nhandoffProposal (Blocked \"pending handoff\" []) Nothing view []"
  assertCell owner "text: parent handoff includes original check packets"
    "all (\\part -> part `T.isInfixOf` (handoffProposal (Blocked \"pending handoff\" []) Nothing view [])) [\"Observed checks:\", \"late: passed\", \"failed: failed\", \"original job\", \"output refs Stdout\", \"runner counts\"]"
  void $ turn owner "finishChecks summarizer"

  void $ turn owner
    "preparedFailedJob <- Cmd.background (fixture \"preparedfail\")\npreparedFailed <- collectFocused (PreparedFocusedRun spec preparedFailedJob)\n(focusedPassed preparedFailed, focusedResultSummary (PreparedFocusedRun spec preparedFailedJob) preparedFailed)"
  assertCell owner "passed preparation stays separate from failed terminal test"
    "not (focusedPassed preparedFailed) && focusedPreparation preparedFailed == PreparationPassed && focusedExecution preparedFailed == ExecutionFailed 0 1 && Cmd.commandOutcome (Cmd.commandResult (focusedCommand preparedFailed)) == Cmd.CommandExited 1 && case focusedEvidence preparedFailed of { Right record -> recordExitCode record == Just 1; _ -> False }"
  void $ turn owner
    "dirty <- Cmd.background (fixture \"dirty\")\nRight dirtyWatcher <- watchChecks me NotifyProblems [(\"dirty\", FocusedRun spec dirty)]"
  awaitCell owner "executed pass stays visible when source assurance is dirty"
    "do { view <- readChecks dirtyWatcher; pure (case checkEntries view of { [entry] -> case checkOutcome entry of { Just outcome -> checkVerdict entry outcome == CheckUnknown && checkExecution entry outcome == ExecutionPassed 1 && case checkSourceAssurance entry outcome of { SourceModified status -> not (T.null status); _ -> False }; _ -> False }; _ -> False }) }"
  void $ turn owner "finishChecks dirtyWatcher"

  void $ turn owner
    "missing <- Cmd.background (fixture \"missingfile\")\nRight missingWatcher <- watchChecks me NotifyProblems [(\"missing\", FocusedRun spec missing)]"
  awaitCell owner "missing embedded evidence stays unknown"
    "do { view <- readChecks missingWatcher; pure (case checkEntries view of { [entry] -> runJob (checkRun entry) == missing && case checkOutcome entry of { Just outcome -> checkVerdict entry outcome == CheckUnknown && checkExecution entry outcome == ExecutionUnknown && case focusedEvidence (checkFocused outcome) of { Left _ -> True; _ -> False }; _ -> False }; _ -> False }) }"
  void $ turn owner
    "missingView <- readChecks missingWatcher\nchecksSummary missingView"
  assertCell owner "text: missing packet names original stream references and unknown counts"
    "all (\\part -> part `T.isInfixOf` (checksSummary missingView)) [\"missing: unknown\", \"matched unknown\", \"executed unknown\", \"evidence unknown\", \"output refs Stdout\"]"
  void $ turn owner "finishChecks missingWatcher"

  void $ turn owner
    "zeroJob <- Cmd.background (fixture \"zero\")\nzeroResult <- collectFocused (FocusedRun spec zeroJob)\nzeroDiagnosis <- diagnoseFocused zeroResult\nsetupJob <- Cmd.background (fixture \"setup\")\nsetupResult <- collectFocused (FocusedRun spec setupJob)\nsetupDiagnosis <- diagnoseFocused setupResult\nshortJob <- Cmd.background (fixture \"short\")\nshortResult <- collectFocused (FocusedRun spec shortJob)\nshortDiagnosis <- diagnoseFocused shortResult\n(diagnosisBranch zeroDiagnosis, diagnosisBranch setupDiagnosis, focusedExecution shortResult, diagnosisBranch shortDiagnosis)"
  assertCell owner "zero selection, incomplete setup and short execution stay distinct"
    "diagnosisBranch zeroDiagnosis == ZeroSelection && diagnosisBranch setupDiagnosis == SetupIncomplete && focusedExecution shortResult == ExecutionUnknown && diagnosisBranch shortDiagnosis == SetupIncomplete"
retainedRecovery :: Member RecipeCheck effects => Eff effects ()
retainedRecovery = do
  owner <- root
  let fixture = Text.pack workspaceRoot <> "/checks/focused-result-fixture.sh"
  void $ turn owner $ Text.unlines
    [ "let spec = FocusedSpec \"fixture check\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
    , "let fixture kind = Cmd.withMemory (Cmd.MiB 256) (Cmd.argv [\"bash\", " <> Text.pack (show fixture) <> ", kind])"
    ]
  void $ turn owner
    "expiredJob <- Cmd.background (fixture \"expired\")\nexpiredResult <- Cmd.quiet (collectFocused (FocusedRun spec expiredJob))\n(focusedExecution expiredResult, focusedPassed expiredResult, focusedEvidence expiredResult)"
  assertCell owner "incomplete retained output cannot prove pass"
    "focusedExecution expiredResult == ExecutionUnknown && not (focusedPassed expiredResult) && case focusedEvidence expiredResult of { Left _ -> case Cmd.capturedOutput (focusedCommand expiredResult) of { Right captured -> let page = Cmd.commandStderr captured in Cmd.outputStart page /= 0 || Cmd.outputEnd page /= Cmd.outputAvailableEnd page || Cmd.outputLostBytes page /= 0 || not (Cmd.outputFinished page) || Cmd.outputLossy page; _ -> False }; _ -> False }"
  void $ turn owner
    "cancelledJob <- Cmd.background (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sleep\", \"30\"]))\nCmd.cancel cancelledJob\ncancelledResult <- Cmd.quiet (collectFocused (PreparedFocusedRun spec cancelledJob))\n(focusedPreparation cancelledResult, focusedPassed cancelledResult, Cmd.commandCleanup (Cmd.commandResult (focusedCommand cancelledResult)))"
  assertCell owner "cancelled original proves neither preparation nor whole-check pass"
    "focusedPreparation cancelledResult == PreparationUnknown && not (focusedPassed cancelledResult) && Cmd.completedJob (focusedCommand cancelledResult) == cancelledJob && Cmd.commandOutcome (Cmd.commandResult (focusedCommand cancelledResult)) == Cmd.CommandCancelled"
data EvidenceProbe mode = EvidenceProbe
  { probeState :: mode :- State ()
  , probeStart :: mode :- Call () (R.Reply FocusedRun)
  , probePrepared :: mode :- Call () (R.Reply (Either FocusedSetupIssue FocusedRun))
  , probeRead :: mode :- Call Text (R.Reply (Either Cmd.CommandError Cmd.CommandResult))
  , probeMove :: mode :- Call (Text, Text) (R.Reply (Either Cmd.CommandError Cmd.CommandResult))
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
          Nothing -> startFocusedAfterWith ["scripts/cargo-focused-test"] (Cmd.MiB 64) spec ["sh", "-c", "printf ready > .prepared-here"]
    , probeStart = \() -> FocusedRun spec <$> Cmd.start
        (Cmd.withMemory (Cmd.MiB 64)
          (Cmd.argv ["bash", Text.pack workspaceRoot <> "/checks/focused-result-fixture.sh", "pass", "managed"]))
    , probeRead = \path -> do
        started <- Cmd.tryStart (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv ["cat", path]))
        case started of
          Left issue -> pure (Left issue)
          Right job -> do
            observed <- Cmd.await job
            pure (Right (Cmd.commandResult observed))
    , probeMove = \(from, to) -> do
        started <- Cmd.tryStart (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv ["mv", from, to]))
        case started of
          Left issue -> pure (Left issue)
          Right job -> do
            observed <- Cmd.await job
            pure (Right (Cmd.commandResult observed))
    }

-- The bound actor runs a check in an actual managed writable checkout. The
-- completion watcher uses the record emitted by the original job even after
-- the artifact moves, and an unbound actor cannot mutate that checkout.
managedEvidence :: Member RecipeCheck effects => Eff effects ()
managedEvidence = do
  owner <- root
  void $ turn owner "import Project.CheckResultsChecks"
  void $ turn owner "Right focusedTree <- createWorktree (fromCurrentRepository \"focused-evidence-check\")\nworktreeId focusedTree"
  assertCell owner "managed focused evidence checkout has typed allocated identity"
    "not (T.null (renderWorktreeId (worktreeId focusedTree)))"
  void $ turn owner $ Text.unlines
    [ "let managedSpec = FocusedSpec \"managed fixture\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
    , "boundProbe <- R.start (R.withWorktree (worktreeId focusedTree) (evidenceProbe managedSpec))"
    , "managedRun <- R.call (probeStart (R.client boundProbe)) ()"
    , "Right managedWatcher <- watchChecks me NotifyAllTerminal [(\"managed\", managedRun)]"
    ]
  awaitCell owner "managed watcher settles its original entry"
    "do { view <- readChecks managedWatcher; pure (case checkEntries view of { [entry] -> case checkOutcome entry of { Just _ -> True; _ -> False }; _ -> False }) }"
  void $ turn owner
    "view <- readChecks managedWatcher\n(checksSummary view, [(fmap (Cmd.stderr . focusedCommand . checkFocused) (checkOutcome e), fmap (focusedEvidence . checkFocused) (checkOutcome e), fmap checkCompletion (checkOutcome e)) | e <- checkEntries view])"
  assertCell owner "watcher keeps counted proof and original managed job receipt"
    "case checkEntries view of { [entry] -> runJob (checkRun entry) == runJob managedRun && runSpec (checkRun entry) == managedSpec && case checkOutcome entry of { Just outcome -> checkVerdict entry outcome == CheckPassed && checkExecution entry outcome == ExecutionPassed 1 && checkSourceAssurance entry outcome == SourceVerified && checkEvidenceComplete entry outcome && Cmd.commandResult (focusedCommand (checkFocused outcome)) == Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean && case (focusedEvidence (checkFocused outcome), focusedEvidencePath (checkFocused outcome)) of { (Right record, Just path) -> recordMatched record == Just [\"fixture::one\"] && recordRunnable record == Just [\"fixture::one\"] && recordSource record == Just \"fixture-source\" && \"/\" `T.isPrefixOf` path; _ -> False }; _ -> False }; _ -> False }"
  void $ turn owner $ Text.unlines
    [ "managedResult <- collectFocused managedRun"
    , "let Just managedPath = focusedEvidencePath managedResult"
    , "accessible <- R.call (probeRead (R.client boundProbe)) managedPath"
    ]
  assertCell owner "artifact exists under managed owner writable authority"
    "accessible == Right (Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean)"
  void $ turn owner $ Text.unlines
    [ "let Just managedPath = focusedEvidencePath managedResult"
    , "moved <- R.call (probeMove (R.client boundProbe)) (managedPath, managedPath <> \".retained\")"
    ]
  assertCell owner "managed owner moves artifact after retained job evidence"
    "moved == Right (Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean)"
  void $ turn owner $ Text.unlines
    [ "let Just managedPath = focusedEvidencePath managedResult"
    , "unboundProbe <- R.start (evidenceProbe managedSpec)"
    , "refused <- R.call (probeMove (R.client unboundProbe)) (managedPath <> \".retained\", managedPath)"
    ]
  assertCell owner "unbound actor cannot mutate managed artifact"
    "case refused of { Left (Cmd.CommandUnauthorized) -> True; Right (Cmd.CommandResult (Cmd.CommandExited 1) Cmd.CommandClean) -> True; _ -> False }"
  void $ turn owner
    "afterMove <- collectFocused managedRun\n(focusedExecution afterMove, focusedEvidence afterMove, focusedEvidencePath afterMove)"
  assertCell owner "original retained job still proves execution after artifact moves"
    "focusedExecution afterMove == ExecutionPassed 1 && Cmd.completedJob (focusedCommand afterMove) == runJob managedRun && case (focusedEvidence afterMove, focusedEvidencePath afterMove) of { (Right record, Just path) -> recordDigest record == \"fixture-digest\" && Just path == focusedEvidencePath managedResult; _ -> False }"
  void $ turn owner "Right preparedRun <- R.call (probePrepared (R.client boundProbe)) ()\nRight preparedWatcher <- watchChecks me NotifyAllTerminal [(\"prepared\", preparedRun)]"
  awaitCell owner "preparation and test share managed checkout retaining counted proof"
    "do { view <- readChecks preparedWatcher; pure (case checkEntries view of { [entry] -> runJob (checkRun entry) == runJob preparedRun && case checkOutcome entry of { Just outcome -> checkVerdict entry outcome == CheckPassed && checkEvidenceComplete entry outcome && focusedPreparation (checkFocused outcome) == PreparationPassed && checkExecution entry outcome == ExecutionPassed 1 && Cmd.commandCleanup (checkCompletion outcome) == Cmd.CommandClean; _ -> False }; _ -> False }) }"
  void $ turn owner "preparedResult <- collectFocused preparedRun\nfocusedPassed (preparedResult { focusedPreparation = PreparationUnknown })"
  assertCell owner "unconfirmed preparation refuses even passing test evidence"
    "not (focusedPassed (preparedResult { focusedPreparation = PreparationUnknown }))"
  void $ turn owner "finishChecks preparedWatcher"
  void $ turn owner "finishChecks managedWatcher\nR.finish boundProbe\nR.finish unboundProbe"

-- The driver must service a live command's cleanup receipt while its resident
-- forest stops. A successful restart proves the old producer was sealed.
runningCommandCleanup :: Member RecipeCheck effects => Eff effects ()
runningCommandCleanup = do
  owner <- root
  void $ turn owner
    "live <- Cmd.background (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sleep\", \"30\"]))"
  awaitCell owner "command is running before resident shutdown"
    "do { status <- Cmd.status live; pure (case status of { Cmd.CommandRunning -> True; _ -> False }) }"
  identity <- restart
  check "live command retirement sealed its producer" (not (Text.null identity))
