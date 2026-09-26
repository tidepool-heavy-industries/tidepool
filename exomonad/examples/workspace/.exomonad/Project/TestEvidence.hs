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
        [bash|set -euo pipefail
scripts/cargo-focused-test --package "$1" --target "$2" --filter "$3" --expect "$4"|]

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

-- Read retained evidence after completion, without a model judgment.
collectFocused :: Member Commands effects => FocusedRun -> Eff effects FocusedResult
collectFocused (FocusedRun spec job) = do
  completed <- Cmd.await job
  let path = evidencePath (Cmd.stderr completed)
  evidence <- case path of
    Nothing -> pure (Left "focused runner did not report an absolute evidence.json path")
    Just file -> do
      loaded <- Cmd.run (Cmd.argv ["cat", file])
      pure $ if Cmd.failure loaded /= Nothing
          || Cmd.commandCleanup (Cmd.commandResult loaded) /= Cmd.CommandClean
        then Left ("cannot read focused evidence; cat receipt: " <>
          Text.pack (show (Cmd.commandResult loaded)))
        else case Cmd.decodeWith (Cmd.asJSON @FocusedRecord) (Cmd.stdout loaded) of
          Left issue -> Left ("cannot decode focused evidence: " <> Text.pack (show issue))
          Right record -> Right record
  pure (FocusedResult spec completed path evidence Nothing)

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

-- | Read at most 8192 bytes from the retained log, then apply the existing
-- code-only reflex table. The original receipt and JSON remain in the packet.
diagnoseFocused :: Member Commands effects => FocusedResult -> Eff effects FocusedDiagnosis
diagnoseFocused result = do
  let branch = failureEvidence result
  excerpt <- case (branch, focusedEvidence result) of
    (NoFailureEvidence, _) -> pure (Right "")
    (_, Left issue) -> pure (Left issue)
    (_, Right record) -> do
      readLog <- Cmd.run (Cmd.argv ["tail", "-c", "8192", recordOutput record])
      pure $ if Cmd.failure readLog /= Nothing
          || Cmd.commandCleanup (Cmd.commandResult readLog) /= Cmd.CommandClean
        then Left ("cannot read retained output log; command receipt: " <>
          Text.pack (show (Cmd.commandResult readLog)))
        else either (Left . Text.pack . show) Right (Cmd.stdout readLog)
  let reflex = case (branch, focusedEvidence result, excerpt) of
        (NoFailureEvidence, _, _) -> Nothing
        (_, Right record, Right output) ->
          case recordExitCode record of
            Just code | code /= 0 -> Reflex.reflexFor code output
            _ -> Nothing
        _ -> Nothing
  pure (FocusedDiagnosis result branch excerpt reflex)

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
    path : _ | "/" `Text.isPrefixOf` path -> Just path
    _ -> Nothing
  where marker = "focused test evidence: "
