{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Exact source, artifact and test-count evidence for a focused Cargo check.
module Project.TestEvidence
  ( FocusedSpec (..), FocusedSetupIssue (..), FocusedRun (..), FocusedRecord (..), FailureKind (..)
  , FocusedResult (..), CheckExecution (..), SourceAssurance (..)
  , FailureEvidence (..), FocusedDiagnosis (..)
  , startFocused, startFocusedIn, collectFocused, diagnoseFocused, finishFocused
  , focusedPassed, focusedExecution, focusedSourceAssurance
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))
import qualified Tidepool.Command as Cmd
import qualified Project.Reflex as Reflex
import Tidepool.Aeson (FromJSON (..), (.:), (.:?), withObject)
import Tidepool.Effects.Core (Commands, Jev)
import Tidepool.QQ.Bash (bash)

data FocusedSpec = FocusedSpec
  { focusedIntent :: Text
  , focusedSource :: Text
  , focusedPackage :: Text
  , focusedTarget :: Text
  , focusedFilter :: Text
  , focusedExpected :: Int
  } deriving (Show, Eq)

data FocusedRun = FocusedRun FocusedSpec Cmd.Job deriving (Show)

data FocusedSetupIssue
  = NonPositiveExpected Int
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

data FailureKind
  = ImplementationFailure
  | FixtureFailure
  | MissingPrerequisite
  | InsufficientEvidence
  deriving (Show, Eq)

data FocusedResult = FocusedResult
  { focusedSpec :: FocusedSpec
  , focusedCommand :: Cmd.RunResult
  , focusedEvidencePath :: Maybe Text
  , focusedEvidence :: Either Text FocusedRecord
  , focusedFailure :: Maybe (Either Text FailureKind)
  } deriving (Show)

data CheckExecution = ExecutionPassed Int | ExecutionFailed Int Int | ExecutionUnknown
  deriving (Eq, Show)

data SourceAssurance = SourceVerified | SourceModified Text | SourceDifferent (Maybe Text) | SourceUnrecorded
  deriving (Eq, Show)

-- Observable failure shape. The causal judgment in FailureKind is separate.
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
  , diagnosisReflex :: Maybe Reflex.Reflex
  } deriving (Show)

-- Run in the actor's checkout. The caller chooses a realistic memory limit for
-- this package; the background job retains its source and terminal receipt.
startFocused :: Member Commands effects => Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocused memory spec
  | focusedExpected spec <= 0 = pure (Left (NonPositiveExpected (focusedExpected spec)))
  | otherwise = fmap (either (Left . FocusedStartRefused) (Right . FocusedRun spec)) $
      Cmd.tryBackground (focusedCommandFor memory spec)

-- | Bind a focused run to an absolute checkout when a completion actor may
-- live outside the caller's working directory.
startFocusedIn :: Member Commands effects => Text -> Cmd.Memory -> FocusedSpec -> Eff effects (Either FocusedSetupIssue FocusedRun)
startFocusedIn checkout memory spec
  | focusedExpected spec <= 0 = pure (Left (NonPositiveExpected (focusedExpected spec)))
  | not ("/" `Text.isPrefixOf` checkout) = pure (Left (NonAbsoluteCheckout checkout))
  | otherwise = fmap (either (Left . FocusedStartRefused) (Right . FocusedRun spec)) $
      Cmd.tryBackground (Cmd.inDirectory checkout (focusedCommandFor memory spec))

focusedCommandFor :: Cmd.Memory -> FocusedSpec -> Cmd.Command
focusedCommandFor memory spec = Cmd.withMemory memory $
      Cmd.withArguments
        [ focusedPackage spec, focusedTarget spec, focusedFilter spec
        , Text.pack (show (focusedExpected spec)) ]
        [bash|set -uo pipefail
runner_output=$(mktemp) || { printf 'focused runner cannot allocate output capture\n' >&2; exit 125; }
trap 'rm -f -- "$runner_output"' EXIT
scripts/cargo-focused-test --package "$1" --target "$2" --filter "$3" --expect "$4" > "$runner_output" 2>&1
runner_exit=$?
tail -c 8192 "$runner_output" >&2
evidence_file=
while IFS= read -r line; do
  case "$line" in
    "focused test evidence: "*) evidence_file=${line#"focused test evidence: "} ;;
  esac
done < "$runner_output"
if [[ -n "$evidence_file" ]]; then
  printf 'focused test evidence: %s\n' "$evidence_file" >&2
fi
if [[ -n "$evidence_file" && -f "$evidence_file" && $(wc -c < "$evidence_file") -le 65536 ]]; then
  printf '\nfocused test record begin\n' >&2
  cat "$evidence_file" >&2
  printf '\nfocused test record end\n' >&2
  printf 'focused test record status: available\n' >&2
elif [[ -n "$evidence_file" && -f "$evidence_file" ]]; then
  printf 'focused test record exceeds 65536 bytes; artifact remains at %s\n' "$evidence_file" >&2
  printf 'focused test record status: unavailable\n' >&2
else
  printf 'focused test record status: unavailable\n' >&2
fi
exit "$runner_exit"|]

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

-- The originating command reads its own artifact before exit. A completion
-- actor may observe the job without being able to open the originating
-- actor's checkout. The terminal output must be complete to prove the JSON.
collectFocused :: Member Commands effects => FocusedRun -> Eff effects FocusedResult
collectFocused (FocusedRun spec job) = do
  completed <- Cmd.await job
  let stderr = Cmd.stderr completed
      path = evidencePath stderr
  let evidence = case path of
        Nothing -> Left "focused runner did not report an absolute evidence.json path"
        Just _ | not (completeStderr completed) -> Left "focused command output is incomplete; evidence record is unproved"
        Just _ -> case evidenceRecord stderr of
          Nothing -> Left "focused command did not retain its evidence record"
          Just encoded -> case Cmd.asJSON @FocusedRecord encoded of
            Left issue -> Left ("cannot decode focused evidence: " <> issue)
            Right record -> Right record
  pure (FocusedResult spec completed path evidence Nothing)

completeStderr :: Cmd.RunResult -> Bool
completeStderr completed =
  let page = Cmd.commandStderr (Cmd.capturedOutput completed)
  in Cmd.outputStart page == 0
    && Cmd.outputEnd page == Cmd.outputAvailableEnd page
    && Cmd.outputLostBytes page == 0
    && Cmd.outputFinished page
    && not (Cmd.outputLossy page)

failureEvidence :: FocusedResult -> FailureEvidence
failureEvidence result = case focusedEvidence result of
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
  let reflex = case (branch, focusedEvidence result, excerpt) of
        (NoFailureEvidence, _, _) -> Nothing
        (_, Right record, Right output) ->
          case recordExitCode record of
            Just code | code /= 0 -> Reflex.reflexFor code output
            _ -> Nothing
        _ -> Nothing
  pure (FocusedDiagnosis result branch excerpt reflex)

diagnosticExcerpt :: Cmd.RunResult -> Either Text Text
diagnosticExcerpt completed
  | not (completeStderr completed) = Left "focused command diagnostic output is incomplete"
  | otherwise = Right (Text.takeEnd 8192 beforeRecord)
  where
    stderr = Cmd.stderr completed
    beforeRecord = case Text.breakOnEnd "focused test record begin\n" stderr of
      (prefix, rest) | not (Text.null rest) ->
        Text.dropEnd (Text.length "focused test record begin\n") prefix
      _ -> stderr

-- An optional diagnosis of a failed check never changes its pass rule.
finishFocused
  :: (Member Commands effects, Member Jev effects)
  => FocusedRun -> Eff effects FocusedResult
finishFocused run = do
  result <- collectFocused run
  diagnosis <- diagnoseFocused result
  let spec = focusedSpec result
  judgment <- case (Cmd.failure (focusedCommand result), focusedEvidence result) of
    (Nothing, _) -> pure Nothing
    (_, Left _) -> pure Nothing
    (Just _, Right _) -> do
      let diagnostic = Text.takeEnd 8000 $ Cmd.stderr (focusedCommand result) <> "\n" <>
            either (const "output log unavailable") id (diagnosisExcerpt diagnosis)
      answer <- J.ask1
        (J.state (#intent := focusedIntent spec :& #diagnostic := diagnostic))
        (J.choice "Which explanation best fits this failed focused check?"
          (J.alt #implementation "The assertion or compiler diagnostic points to the implementation" ImplementationFailure
            J..| J.alt #fixture "The failure points to test setup or fixture data" FixtureFailure
            J..| J.alt #prerequisite "A missing tool, dependency or environment condition prevented the check" MissingPrerequisite
            J..| J.alt #insufficient "The retained diagnostic does not establish any of those causes" InsufficientEvidence))
      pure $ Just $ case answer of
        Left issue -> Left (Text.pack (show issue))
        Right choice -> case J.takenUnder J.lenient choice of
          Left doubt -> Left doubt.why
          Right (J.Settled kind) -> Right kind
  pure result {focusedFailure = judgment}

-- Exit, checkout identity, selected count and executed count are code facts.
-- A Jev classification is never an input to this predicate.
focusedPassed :: FocusedResult -> Bool
focusedPassed result =
  focusedExecution result == ExecutionPassed (focusedExpected (focusedSpec result))
    && focusedSourceAssurance result == SourceVerified
    && Cmd.failure (focusedCommand result) == Nothing
    && Cmd.commandCleanup (Cmd.commandResult (focusedCommand result)) == Cmd.CommandClean
    && case focusedEvidence result of
      Right record -> recordExitCode record == Just 0
      Left _ -> False

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
      (_, rest) | Text.null rest -> Nothing
      (_, rest) -> case Text.breakOn end rest of
        (_, remaining) | Text.null remaining -> Nothing
        (encoded, _) -> Just (Text.strip encoded)
  _ -> Nothing
  where
    begin = "focused test record begin\n"
    end = "\nfocused test record end"
