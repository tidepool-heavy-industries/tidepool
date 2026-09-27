{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE OverloadedRecordDot #-}
module Project.PatternReplay (replay) where
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Jev.Operators as J
import Tidepool.Aeson (eitherDecode)
import Tidepool.Aeson.Value (Value)
import Tidepool.Check
import Project.CommandTriagePattern
import Project.CommandTriageExamples
import Project.EvidencePattern
import Project.EvidencePatternExamples
import Project.CoordinationPattern
import Project.CoordinationPatternExamples
import Project.PatternProbe (commandCases)
replay :: Member RecipeCheck effects => Eff effects ()
replay = do
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"fault\":{\"type\":\"choice\",\"choice\":\"invocation\",\"confidence\":0.91,\"probabilities\":{\"tooling\":0.06,\"assertion\":0.0,\"invocation\":0.9400000000000001,\"unknown\":0.0}}},\"usage\":{\"input_tokens\":432,\"output_tokens\":52}}" :: Either Text Value)
  case [(criteria, allowance, expected) | (name, criteria, allowance, _, expected) <- commandCases, name == "arguments"] of
    [(criteria, allowance, expected)] -> case J.decode (triagePacket criteria allowance) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let a = J.answers response
            verdict = chooseFollowup J.careful allowance a.fault a.transient
        check ("OBSERVATION arguments: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing command fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"fault\":{\"type\":\"choice\",\"choice\":\"assertion\",\"confidence\":1.0,\"probabilities\":{\"invocation\":0.0,\"assertion\":1.0,\"tooling\":0.0,\"unknown\":0.0}}},\"usage\":{\"input_tokens\":439,\"output_tokens\":52}}" :: Either Text Value)
  case [(criteria, allowance, expected) | (name, criteria, allowance, _, expected) <- commandCases, name == "assertion"] of
    [(criteria, allowance, expected)] -> case J.decode (triagePacket criteria allowance) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let a = J.answers response
            verdict = chooseFollowup J.careful allowance a.fault a.transient
        check ("OBSERVATION assertion: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing command fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"update\":{\"type\":\"choice\",\"choice\":\"attention\",\"confidence\":1.0,\"probabilities\":{\"unresolved\":0.0,\"repetition\":0.0,\"attention\":1.0}}},\"usage\":{\"input_tokens\":613,\"output_tokens\":44}}" :: Either Text Value)
  case [(criteria, expected) | (name, criteria, _, expected) <- exampleCases, name == "consumer-changed"] of
    [(criteria, expected)] -> case J.decode (comparisonPacket criteria) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let verdict = settleComparison J.careful response.update
        check ("OBSERVATION consumer-changed: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing comparison fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"update\":{\"type\":\"choice\",\"choice\":\"attention\",\"confidence\":0.3,\"probabilities\":{\"attention\":0.53,\"unresolved\":0.45,\"repetition\":0.02}}},\"usage\":{\"input_tokens\":599,\"output_tokens\":44}}" :: Either Text Value)
  case [(criteria, expected) | (name, criteria, _, expected) <- exampleCases, name == "consumer-silence"] of
    [(criteria, expected)] -> case J.decode (comparisonPacket criteria) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let verdict = settleComparison J.careful response.update
        check ("OBSERVATION consumer-silence: " <> T.pack (show verdict)) (case verdict of
          Left _ -> True
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing comparison fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"best\":{\"type\":\"choice\",\"choice\":\"E0061-site\",\"confidence\":0.97,\"probabilities\":{\"E0004-site\":0.0,\"E0061-definition\":0.02,\"none\":0.0,\"E0061-site\":0.98,\"insufficient\":0.0}},\"coverage\":{\"type\":\"noul\",\"noul\":0.92}},\"usage\":{\"input_tokens\":731,\"output_tokens\":92}}" :: Either Text Value)
  let decoded = J.decode commandPacket raw
  case decoded of
    Left problem -> check ("decode evidence-command: " <> T.pack (show problem)) False
    Right response -> do
      check "coverage establishes the stated location/bound facts" (J.holds J.lenient response.coverage)
      let verdict = settleEvidence J.lenient response.best
      check ("OBSERVATION evidence-command: " <> T.pack (show verdict)) (case verdict of
        Right (J.Settled (Selected row)) -> evidenceKey row == "E0061-site"
        _ -> False)
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"best\":{\"type\":\"choice\",\"choice\":\"budget\",\"confidence\":0.58,\"probabilities\":{\"page\":0.32,\"insufficient\":0.01,\"budget\":0.66,\"none\":0.01,\"result\":0.0}},\"coverage\":{\"type\":\"noul\",\"noul\":0.93}},\"usage\":{\"input_tokens\":829,\"output_tokens\":72}}" :: Either Text Value)
  let decoded = J.decode reviewPacket raw
  case decoded of
    Left problem -> check ("decode evidence-review: " <> T.pack (show problem)) False
    Right response -> do
      check "coverage establishes the stated location/bound facts" (J.holds J.lenient response.coverage)
      let verdict = settleEvidence J.lenient response.best
      check ("OBSERVATION evidence-review: " <> T.pack (show verdict)) (case verdict of
        Right (J.Settled (Selected row)) -> evidenceKey row == "budget"
        _ -> False)
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"fault\":{\"type\":\"choice\",\"choice\":\"tooling\",\"confidence\":0.99,\"probabilities\":{\"invocation\":0.0,\"assertion\":0.0,\"tooling\":0.99,\"unknown\":0.01}}},\"usage\":{\"input_tokens\":445,\"output_tokens\":51}}" :: Either Text Value)
  case [(criteria, allowance, expected) | (name, criteria, allowance, _, expected) <- commandCases, name == "fetch-no-repeat"] of
    [(criteria, allowance, expected)] -> case J.decode (triagePacket criteria allowance) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let a = J.answers response
            verdict = chooseFollowup J.careful allowance a.fault a.transient
        check ("OBSERVATION fetch-no-repeat: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing command fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"fault\":{\"type\":\"choice\",\"choice\":\"tooling\",\"confidence\":0.99,\"probabilities\":{\"invocation\":0.0,\"tooling\":1.0,\"unknown\":0.0,\"assertion\":0.0}},\"transient\":{\"type\":\"noul\",\"noul\":0.94}},\"usage\":{\"input_tokens\":510,\"output_tokens\":69}}" :: Either Text Value)
  case [(criteria, allowance, expected) | (name, criteria, allowance, _, expected) <- commandCases, name == "fetch-repeat"] of
    [(criteria, allowance, expected)] -> case J.decode (triagePacket criteria allowance) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let a = J.answers response
            verdict = chooseFollowup J.careful allowance a.fault a.transient
        check ("OBSERVATION fetch-repeat: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing command fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"fault\":{\"type\":\"choice\",\"choice\":\"tooling\",\"confidence\":1.0,\"probabilities\":{\"tooling\":1.0,\"unknown\":0.0,\"assertion\":0.0,\"invocation\":0.0}},\"transient\":{\"type\":\"noul\",\"noul\":0.02}},\"usage\":{\"input_tokens\":504,\"output_tokens\":69}}" :: Either Text Value)
  case [(criteria, allowance, expected) | (name, criteria, allowance, _, expected) <- commandCases, name == "payment"] of
    [(criteria, allowance, expected)] -> case J.decode (triagePacket criteria allowance) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let a = J.answers response
            verdict = chooseFollowup J.careful allowance a.fault a.transient
        check ("OBSERVATION payment: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing command fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"update\":{\"type\":\"choice\",\"choice\":\"attention\",\"confidence\":1.0,\"probabilities\":{\"repetition\":0.0,\"unresolved\":0.0,\"attention\":1.0}}},\"usage\":{\"input_tokens\":696,\"output_tokens\":44}}" :: Either Text Value)
  case [(criteria, expected) | (name, criteria, _, expected) <- exampleCases, name == "review-mixed"] of
    [(criteria, expected)] -> case J.decode (comparisonPacket criteria) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let verdict = settleComparison J.careful response.update
        check ("OBSERVATION review-mixed: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing comparison fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"update\":{\"type\":\"choice\",\"choice\":\"repetition\",\"confidence\":0.85,\"probabilities\":{\"unresolved\":0.02,\"repetition\":0.9,\"attention\":0.08}}},\"usage\":{\"input_tokens\":640,\"output_tokens\":47}}" :: Either Text Value)
  case [(criteria, expected) | (name, criteria, _, expected) <- exampleCases, name == "review-repeat"] of
    [(criteria, expected)] -> case J.decode (comparisonPacket criteria) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let verdict = settleComparison J.careful response.update
        check ("OBSERVATION review-repeat: " <> T.pack (show verdict)) (case verdict of
          Left _ -> False
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing comparison fixture" False
  let raw = either (error . T.unpack) id (eitherDecode "{\"model\":\"jev-1.13.0\",\"answers\":{\"fault\":{\"type\":\"choice\",\"choice\":\"unknown\",\"confidence\":0.36,\"probabilities\":{\"tooling\":0.47,\"assertion\":0.01,\"invocation\":0.0,\"unknown\":0.52}}},\"usage\":{\"input_tokens\":424,\"output_tokens\":50}}" :: Either Text Value)
  case [(criteria, allowance, expected) | (name, criteria, allowance, _, expected) <- commandCases, name == "unknown"] of
    [(criteria, allowance, expected)] -> case J.decode (triagePacket criteria allowance) raw of
      Left problem -> check (T.pack (show problem)) False
      Right response -> do
        let a = J.answers response
            verdict = chooseFollowup J.careful allowance a.fault a.transient
        check ("OBSERVATION unknown: " <> T.pack (show verdict)) (case verdict of
          Left _ -> True
          Right (J.Settled observed) -> observed == expected)
    _ -> check "missing command fixture" False
