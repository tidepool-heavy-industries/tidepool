{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Exact source, artifact and test-count evidence for a focused Cargo check.
module Exomonad.Contrib.Check.Cargo
  ( FocusedSpec (..), FocusedSetupIssue (..), FocusedRun (..), FocusedRecord (..)
  , FocusedResult (..), CheckExecution (..), SourceAssurance (..)
  , FailureEvidence (..), FocusedDiagnosis (..)
  , PreparationEvidence (..), startFocusedAfterWith
  , startFocusedWith, startFocusedInWith, startFocusedScopedInWith, collectFocused, diagnoseFocused
  , focusedPassed, focusedEvidenceComplete, focusedExecution, focusedSourceAssurance, focusedResultSummary
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.List (nub, sort)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Command as Cmd
import Tidepool.Aeson (FromJSON (..), (.:), (.:?), withObject)
import Tidepool.Effects.Core (Commands)

data FocusedSpec = FocusedSpec
  { focusedIntent :: Text
  , focusedSource :: Text
  , focusedPackage :: Text
  , focusedTarget :: Text
  , focusedFilter :: Text
  , focusedExpected :: Int
  } deriving (Show, Eq)

data FocusedRun
  = FocusedRun { runSpec :: FocusedSpec, runJob :: Cmd.Job }
  | PreparedFocusedRun { runSpec :: FocusedSpec, runJob :: Cmd.Job }
  deriving (Show)

data FocusedSetupIssue
  = EmptyRunner
  | EmptyPreparation
  | NonPositiveExpected Int
  | NonAbsoluteCheckout Text
  | FocusedStartRefused Cmd.CommandError
  deriving (Show, Eq)

-- The runner writes these fields to evidence.json before test execution and
-- fills counts as they become known. Missing fields stay missing evidence.
data FocusedRecord = FocusedRecord
  { recordSource :: Maybe Text
  , recordWorkingTree :: Maybe Text
  , recordExecutable :: Text
  , recordDigest :: Text
  , recordOutput :: Text
  , recordMatched :: Maybe [Text]
  , recordRunnable :: Maybe [Text]
  , recordSummaries :: Maybe [[Int]]
  , recordExitCode :: Maybe Int
  } deriving (Show, Eq)

instance FromJSON FocusedRecord where
  parseJSON = withObject "focused test evidence" $ \fields ->
    FocusedRecord <$> fields .: "source"
      <*> fields .: "working_tree_status"
      <*> fields .: "executable"
      <*> fields .: "sha256"
      <*> fields .: "output"
      <*> fields .:? "matched"
      <*> fields .:? "runnable"
      <*> fields .:? "summaries"
      <*> fields .:? "exit_code"

data PreparationEvidence = NoPreparation | PreparationPassed | PreparationFailed Int | PreparationUnknown
  deriving (Show, Eq)

data FocusedResult = FocusedResult
  { focusedSpec :: FocusedSpec
  , focusedCommand :: Cmd.RunResult
  , focusedEvidencePath :: Maybe Text
  , focusedEvidence :: Either Text FocusedRecord
  , focusedPreparation :: PreparationEvidence
  } deriving (Show)

data CheckExecution = ExecutionPassed Int | ExecutionFailed Int Int | ExecutionUnknown
  deriving (Eq, Show)

data SourceAssurance = SourceVerified | SourceModified Text | SourceDifferent (Maybe Text) | SourceUnrecorded
  deriving (Eq, Show)

-- Observable failure shape; this does not guess its cause.
data FailureEvidence
  = NoFailureEvidence
  | EvidenceUnavailable
  | ZeroSelection
  | SetupIncomplete
  | AssertionsFailed Int Int
  | RunnerFailed
  | SourceUnverified
  deriving (Show, Eq)

data FocusedDiagnosis = FocusedDiagnosis
  { diagnosisResult :: FocusedResult
  , diagnosisBranch :: FailureEvidence
  , diagnosisExcerpt :: Either Text Text
  } deriving (Show)

-- Run literal runner argv in the actor's checkout, appending --package,
-- --target, --filter and --expect. The runner emits an absolute evidence.json
-- path in the focused-test protocol; this job retains its record before exit.
startFocusedWith :: Member Commands effects => [Text] -> Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedWith runner memory spec
  | null runner = pure (Left EmptyRunner)
  | focusedExpected spec <= 0 = pure (Left (NonPositiveExpected (focusedExpected spec)))
  | otherwise = fmap (either (Left . FocusedStartRefused) (Right . FocusedRun spec)) $
      Cmd.tryBackground (focusedCommandAfter runner memory spec [])

-- | Bind a focused run to an absolute checkout when a completion actor may
-- live outside the caller's working directory.
startFocusedInWith :: Member Commands effects => [Text] -> Text -> Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedInWith = startFocusedInUsing Cmd.tryBackground

-- | Start invocation-owned work in an exact checkout. Await the returned run
-- before leaving the invocation; it is never detached to an actor lifetime.
startFocusedScopedInWith :: Member Commands effects => [Text] -> Text -> Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedScopedInWith = startFocusedInUsing Cmd.tryStart

startFocusedInUsing
  :: Member Commands effects
  => (Cmd.Command -> Eff effects (Either Cmd.CommandError Cmd.Job))
  -> [Text] -> Text -> Cmd.Memory -> FocusedSpec
  -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedInUsing startCommand runner checkout memory spec
  | null runner = pure (Left EmptyRunner)
  | focusedExpected spec <= 0 = pure (Left (NonPositiveExpected (focusedExpected spec)))
  | not ("/" `Text.isPrefixOf` checkout) = pure (Left (NonAbsoluteCheckout checkout))
  | otherwise = fmap (either (Left . FocusedStartRefused) (Right . FocusedRun spec)) $
      startCommand (Cmd.inDirectory checkout (focusedCommandAfter runner memory spec []))

-- Preparation and the check execute under one job's checkout authority. The
-- argv is executed literally; a failed prerequisite never starts the runner.
startFocusedAfterWith :: Member Commands effects => [Text] -> Cmd.Memory -> FocusedSpec -> [Text] -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedAfterWith runner memory spec preparation
  | null runner = pure (Left EmptyRunner)
  | null preparation = pure (Left EmptyPreparation)
  | focusedExpected spec <= 0 = pure (Left (NonPositiveExpected (focusedExpected spec)))
  | otherwise = fmap (either (Left . FocusedStartRefused) (Right . PreparedFocusedRun spec)) $
      Cmd.tryBackground (focusedCommandAfter runner memory spec preparation)

focusedCommandAfter :: [Text] -> Cmd.Memory -> FocusedSpec -> [Text] -> Cmd.Command
focusedCommandAfter runner memory spec preparation = Cmd.withMemory memory $
      Cmd.withArguments
        ([ focusedPackage spec, focusedTarget spec, focusedFilter spec
        , Text.pack (show (focusedExpected spec)), Text.pack (show (length preparation))
        ] ++ preparation ++ runner)
        (Cmd.bashCommand (Text.intercalate "\n"
          [ "set -uo pipefail"
          , "runner_output=$(mktemp) || { printf 'focused runner cannot allocate output capture\\n' >&2; exit 125; }"
          , "trap 'rm -f -- \"$runner_output\"' EXIT"
          , "preparation_count=$5"
          , "if (( preparation_count > 0 )); then"
          , "  \"${@:6:preparation_count}\" > \"$runner_output\" 2>&1"
          , "  preparation_exit=$?"
          , "  tail -c 8192 \"$runner_output\" >&2"
          , "  printf '\\nfocused preparation exit: %s\\n' \"$preparation_exit\" >&2"
          , "  if (( preparation_exit != 0 )); then exit \"$preparation_exit\"; fi"
          , "fi"
          , "runner_start=$((6 + preparation_count))"
          , "\"${@:runner_start}\" --package \"$1\" --target \"$2\" --filter \"$3\" --expect \"$4\" > \"$runner_output\" 2>&1"
          , "runner_exit=$?"
          , "tail -c 8192 \"$runner_output\" >&2"
          , "evidence_file="
          , "while IFS= read -r line; do"
          , "  case \"$line\" in"
          , "    \"focused test evidence: \"*) evidence_file=${line#\"focused test evidence: \"} ;;"
          , "  esac"
          , "done < \"$runner_output\""
          , "if [[ -n \"$evidence_file\" ]]; then"
          , "  printf 'focused test evidence: %s\\n' \"$evidence_file\" >&2"
          , "fi"
          , "if [[ -n \"$evidence_file\" && -f \"$evidence_file\" && $(wc -c < \"$evidence_file\") -le 65536 ]]; then"
          , "  printf '\\nfocused test record begin\\n' >&2"
          , "  cat \"$evidence_file\" >&2"
          , "  printf '\\nfocused test record end\\n' >&2"
          , "  printf 'focused test record status: available\\n' >&2"
          , "elif [[ -n \"$evidence_file\" && -f \"$evidence_file\" ]]; then"
          , "  printf 'focused test record exceeds 65536 bytes; artifact remains at %s\\n' \"$evidence_file\" >&2"
          , "  printf 'focused test record status: unavailable\\n' >&2"
          , "else"
          , "  printf 'focused test record status: unavailable\\n' >&2"
          , "fi"
          , "exit \"$runner_exit\""
          ]))

focusedExecution :: FocusedResult -> CheckExecution
focusedExecution result
  | focusedExpected spec <= 0 = ExecutionUnknown
  | otherwise = case focusedEvidence result of
      Left _ -> ExecutionUnknown
      Right record -> case (recordRunnable record, recordSummaries record) of
        (Just runnable, Just [counts@[passed, failed, _, _, _]])
          | length runnable == focusedExpected spec
              && all (>= 0) counts
              && passed + failed == focusedExpected spec ->
                if failed == 0 then ExecutionPassed passed else ExecutionFailed passed failed
        _ -> ExecutionUnknown
  where spec = focusedSpec result

focusedSourceAssurance :: FocusedResult -> SourceAssurance
focusedSourceAssurance result = case focusedEvidence result of
  Left _ -> SourceUnrecorded
  Right record
    | recordSource record /= Just (focusedSource spec) -> SourceDifferent (recordSource record)
    | recordWorkingTree record == Just "" -> SourceVerified
    | Just dirty <- recordWorkingTree record -> SourceModified dirty
    | otherwise -> SourceUnrecorded
  where spec = focusedSpec result

-- | A handoff view of the original job and its retained facts. The job is
-- also the handle for 'Cmd.readOutput' on either stream; the artifact path is
-- evidence provenance, not a requirement to reopen a child's checkout.
focusedResultSummary :: FocusedRun -> FocusedResult -> Text
focusedResultSummary run result =
  "original job " <> jobText
    <> "; requested source " <> focusedSource (runSpec run)
    <> "; recorded source " <> maybe "unknown" id recordedSource
    <> "; source " <> assuranceText assurance
    <> "; preparation " <> Text.pack (show (focusedPreparation result))
    <> "; terminal " <> Text.pack (show (Cmd.commandOutcome receipt))
    <> "; cleanup " <> Text.pack (show (Cmd.commandCleanup receipt))
    <> "; test phase " <> testPhase
    <> "; " <> selected
    <> "; executed " <> execution
    <> "; runner counts " <> maybe "unknown" (Text.pack . show . recordSummaries) record
    <> "; runner exit " <> maybe "unknown" (Text.pack . show . recordExitCode) record
    <> "; executable " <> maybe "unknown" recordExecutable record
    <> "; digest " <> maybe "unknown" recordDigest record
    <> "; runner output " <> maybe "unknown" recordOutput record
    <> "; artifact " <> maybe "unknown" id (focusedEvidencePath result)
    <> "; output refs Stdout " <> jobText <> ", Stderr " <> jobText
    <> either ("; evidence unknown: " <>) (const "; evidence recorded") (focusedEvidence result)
    <> if sameRun then "" else "; original/result identity mismatch"
  where
    jobText = Text.pack (show (runJob run))
    receipt = Cmd.commandResult (focusedCommand result)
    sameRun = runSpec run == focusedSpec result
      && runJob run == Cmd.completedJob (focusedCommand result)
    assurance = if sameRun then focusedSourceAssurance result else SourceUnrecorded
    record = either (const Nothing) Just (focusedEvidence result)
    recordedSource = recordSource =<< record
    testPhase = case focusedPreparation result of
      PreparationFailed _ -> "not entered"
      _ -> case record of
        Just _ -> "runner reported"
        Nothing -> "not established"
    selected = case focusedEvidence result of
      Left _ -> "matched unknown, runnable unknown"
      Right record -> "matched " <> count (recordMatched record)
        <> ", runnable " <> count (recordRunnable record)
    count = maybe "unknown" (Text.pack . show . length)
    assuranceText SourceVerified = "verified"
    assuranceText (SourceModified _) = "dirty"
    assuranceText (SourceDifferent _) = "different"
    assuranceText SourceUnrecorded = "unrecorded"
    execution = case if sameRun then focusedExecution result else ExecutionUnknown of
      ExecutionPassed passed -> Text.pack (show passed) <> " passed"
      ExecutionFailed passed failed -> Text.pack (show passed) <> " passed, "
        <> Text.pack (show failed) <> " failed"
      ExecutionUnknown -> "unknown"

-- The originating command reads its own artifact before exit. A completion
-- actor may observe the job without being able to open the originating
-- actor's checkout. The terminal output must be complete to prove the JSON.
collectFocused :: Member Commands effects => FocusedRun -> Eff effects FocusedResult
collectFocused run = do
  let spec = runSpec run
      job = runJob run
  completed <- Cmd.await job
  let stderr = completeStderr completed
      path = either (const Nothing) evidencePath (Cmd.stderr completed)
      evidence = do
        retained <- stderr
        case path of
          Nothing -> Left "focused runner did not report an absolute evidence.json path"
          Just _ -> case evidenceRecord retained of
            Nothing -> Left "focused command did not retain its evidence record"
            Just encoded -> case Cmd.asJSON @FocusedRecord encoded of
              Left issue -> Left ("cannot decode focused evidence: " <> issue)
              Right record -> Right record
      preparation = case stderr of
        Left _ -> PreparationUnknown
        Right retained ->
          case [Text.strip suffix | line <- Text.lines retained, Just suffix <- [Text.stripPrefix "focused preparation exit: " line]] of
            [] -> case run of
              FocusedRun {} -> NoPreparation
              PreparedFocusedRun {} -> PreparationUnknown
            ["0"] -> PreparationPassed
            [code] -> case reads (Text.unpack code) of
              [(value, "")] | value > 0 -> PreparationFailed value
              _ -> PreparationUnknown
            _ -> PreparationUnknown
  pure (FocusedResult spec completed path evidence preparation)

completeStderr :: Cmd.RunResult -> Either Text Text
completeStderr completed = case Cmd.capturedOutput completed of
  Left issue -> Left ("focused command output is unavailable: " <> Cmd.renderCommandError issue)
  Right captured ->
    let page = Cmd.commandStderr captured
    in if Cmd.outputStart page == 0
        && Cmd.outputEnd page == Cmd.outputAvailableEnd page
        && Cmd.outputLostBytes page == 0
        && Cmd.outputFinished page
        && not (Cmd.outputLossy page)
      then Right (Cmd.outputText page)
      else Left "focused command output is incomplete; evidence record is unproved"

failureEvidence :: FocusedResult -> FailureEvidence
failureEvidence result
  | PreparationFailed _ <- focusedPreparation result = SetupIncomplete
  | otherwise = case focusedEvidence result of
    Left _ -> EvidenceUnavailable
    Right record
      | recordRunnable record == Just [] -> ZeroSelection
      | ExecutionFailed passed failed <- focusedExecution result ->
          AssertionsFailed passed failed
      | ExecutionUnknown <- focusedExecution result -> SetupIncomplete
      | Cmd.failure (focusedCommand result) /= Nothing
          || Cmd.commandCleanup (Cmd.commandResult (focusedCommand result)) /= Cmd.CommandClean
          || recordExitCode record /= Just 0 -> RunnerFailed
      | focusedSourceAssurance result /= SourceVerified -> SourceUnverified
      | otherwise -> NoFailureEvidence

-- | The originating job already emitted a bounded diagnostic tail before its
-- evidence record. Reading that terminal output does not require authority to
-- the originating checkout. The original receipt and JSON remain in packet.
diagnoseFocused :: FocusedResult -> Eff effects FocusedDiagnosis
diagnoseFocused result = do
  let branch = failureEvidence result
      excerpt = if branch == NoFailureEvidence
        then Right ""
        else diagnosticExcerpt (focusedCommand result)
  pure (FocusedDiagnosis result branch excerpt)

diagnosticExcerpt :: Cmd.RunResult -> Either Text Text
diagnosticExcerpt completed = do
  stderr <- completeStderr completed
  let beforeRecord = case Text.breakOnEnd "focused test record begin\n" stderr of
        (prefix, _) | not (Text.null prefix) ->
          Text.dropEnd (Text.length "focused test record begin\n") prefix
        _ -> stderr
  pure (Text.takeEnd 8192 beforeRecord)

-- Exit, checkout identity, selected count and executed count are code facts.
-- A Jev classification is never an input to this predicate.
focusedPassed :: FocusedResult -> Bool
focusedPassed result =
  focusedEvidenceComplete result
    && focusedExecution result == ExecutionPassed (focusedExpected (focusedSpec result))

-- | Prove either a passing check or a counted assertion failure. Runner,
-- preparation and infrastructure failures remain observable without claiming
-- that the requested tests established a result against the requested source.
focusedEvidenceComplete :: FocusedResult -> Bool
focusedEvidenceComplete result =
  focusedPreparation result `elem` [NoPreparation, PreparationPassed]
    && focusedSourceAssurance result == SourceVerified
    && Cmd.commandCleanup receipt == Cmd.CommandClean
    && case focusedEvidence result of
      Right record -> selectionComplete record && case Cmd.commandOutcome receipt of
        Cmd.CommandExited code | recordExitCode record == Just code ->
          case focusedExecution result of
            ExecutionPassed passed -> passed == expected && code == 0
            ExecutionFailed _ failed -> failed > 0 && code /= 0
            ExecutionUnknown -> False
        _ -> False
      Left _ -> False
  where
    expected = focusedExpected (focusedSpec result)
    receipt = Cmd.commandResult (focusedCommand result)
    selectionComplete record = case (recordMatched record, recordRunnable record) of
      (Just matched, Just runnable) ->
        length matched == expected && length (nub matched) == expected
          && sort matched == sort runnable
      _ -> False

evidencePath :: Text -> Maybe Text
evidencePath stderr = case
  [ Text.strip (Text.drop (Text.length marker) line)
  | line <- Text.lines stderr, marker `Text.isPrefixOf` line ] of
    paths | path : _ <- reverse paths, "/" `Text.isPrefixOf` path -> Just path
    _ -> Nothing
  where marker = "focused test evidence: "

evidenceRecord :: Text -> Maybe Text
evidenceRecord stderr = case reverse (Text.lines stderr) of
  "focused test record status: available" : _ ->
    case Text.breakOnEnd begin stderr of
      (prefix, _) | Text.null prefix -> Nothing
      (_, rest) -> case Text.breakOn end rest of
        (_, remaining) | Text.null remaining -> Nothing
        (encoded, _) -> Just (Text.strip encoded)
  _ -> Nothing
  where
    begin = "focused test record begin\n"
    end = "\nfocused test record end"
