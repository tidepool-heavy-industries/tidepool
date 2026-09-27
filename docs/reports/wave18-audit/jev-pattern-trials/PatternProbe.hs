{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE OverloadedRecordDot #-}
module Project.PatternProbe (exportRequests, commandCases) where
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text
import qualified Jev.Operators as J
import Tidepool.Aeson.Value (encodeValue)
import Tidepool.Check
import Project.CommandTriagePattern
import Project.CommandTriageExamples
import Project.EvidencePattern
import Project.EvidencePatternExamples
import Project.CoordinationPattern
import Project.CoordinationPatternExamples

-- Offline synthetic inputs: no commands are run and no live event is suppressed.
commandCases =
  [ ("assertion", testFailureCriteria, NoRepeat, "running 1 test; tests::sum FAILED; assertion left == right failed: left 3, right 4", InspectAssertions)
  , ("arguments", testFailureCriteria, NoRepeat, "error: unexpected argument --frobnicate found; Usage: cargo test [OPTIONS]", InspectInputs)
  , ("fetch-no-repeat", packageFetchCriteria, NoRepeat, "HTTP 503 Service Unavailable: artifact service temporarily overloaded; Retry-After: 30", InspectTooling)
  , ("fetch-repeat", packageFetchCriteria, OneRepeatAllowed, "HTTP 503 Service Unavailable: artifact service temporarily overloaded; Retry-After: 30", OfferOneRepeat)
  , ("payment", packageFetchCriteria, OneRepeatAllowed, "HTTP 402 payment required. Account credit balance exhausted; add funds before requesting artifacts.", InspectTooling)
  , ("unknown", testFailureCriteria, NoRepeat, "command exited 1; output was not retained", AskForEvidence)
  ]

exportRequests :: Member RecipeCheck effects => Eff effects ()
exportRequests = do
  mapM_ (\(name, criteria, allowance, excerpt, _) -> emit name
    (J.request J.jevLatest (triageState ("synthetic/" <> name) excerpt) (triagePacket criteria allowance))) commandCases
  emit "evidence-command" (J.request J.jevLatest commandState commandPacket)
  emit "evidence-review" (J.request J.jevLatest reviewState reviewPacket)
  mapM_ (\(name, criteria, input, _) -> case prepareComparison input of
    Left unresolved -> check ("GUARDED " <> name <> " " <> showText unresolved) True
    Right ready -> emit name (J.request J.jevLatest (comparisonState ready) (comparisonPacket criteria))) exampleCases
  where
    emit name result = case result of
      Left problem -> check ("REQUEST-ERROR " <> name <> " " <> showText problem) False
      Right request -> check ("REQUEST " <> name <> " " <> encodeValue request) True
showText :: Show a => a -> Text
showText = Data.Text.pack . show
