{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Editable compositions. See checks/jev-composition.md for complete cells,
-- source prerequisites and the distinction between compiling and calling Jev.
module Examples.JevComposedWorkflow where

import Control.Monad.Freer (Eff, Member)
import Control.Lens (Lens', lens, over, traverseOf, traversed, _2, view)
import Control.Selective (ifS)
import Data.Profunctor (Star (..), lmap)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Jev.Core as C
import qualified Jev.Operators as J
import Tidepool.Aeson.Value (Value)
import Tidepool.Effects.Core (Jev, FsRead, FsWrite, Lookup, Reflect)
import qualified Tidepool.Effects.Authored as Files
import Tidepool.Records (FsError)
import qualified Jev.Recording as Recording
import Project.EvidencePattern
import qualified Project.Lookup as Lookup
import qualified Tidepool.Lookup.Tools as LookupTools
import Tidepool.Inspection.Tree (renderTreeWithNotice)
import Tidepool.Inspection.Display (Display (displayTree))
import qualified Tidepool.Lookup as RawLookup
import Tidepool.Aeson.Value (object, (.=))

-- The original evidence row carries its continuation. Rendering, recording,
-- decoding and policy comparison do not execute it.
type Continuation effects = Evidence (Eff effects Text)
type BatchAnswer effects =
  (J.Chosen (EvidenceAlternatives (Eff effects Text)), J.Yes)

batchQuestions :: Text -> [Continuation effects] -> J.Query (BatchAnswer effects)
batchQuestions = batchQuestionsWith
  "Do the supplied excerpts contain enough evidence to answer the stated intent? Judge only the supplied excerpts."

batchQuestionsWith :: Text -> Text -> [Continuation effects] -> J.Query (BatchAnswer effects)
batchQuestionsWith sufficiencyQuestion intent rows = (,)
  <$> J.question world #best (evidenceQuestion criteria describeEvidence rows)
  <*> J.question world #sufficient
      (J.noul sufficiencyQuestion)
  where
    criteria = defaultCriteria intent
    world = J.state (#intent J.:= intent J.:& #evidence J.:=
      ([(evidenceKey row, [("source", evidenceSource row), ("excerpt", evidenceExcerpt row)]) | row <- rows]
        :: [(Text, [(Text, Text)])]))

-- Equal states are grouped into one provider request with distinct occurrences.
-- Keep the Evaluation: it includes failures and every original observation.
observeBatch :: Member Jev effects
  => Text -> [Continuation effects] -> Eff effects (J.Evaluation (BatchAnswer effects))
observeBatch intent = J.runQuery . batchQuestions intent

-- Includes zero-mass alternatives and the original typed evidence/action rows.
-- The returned weights are reported masses; normalize explicitly if needed.
fullDistribution :: J.Chosen (EvidenceAlternatives a) -> J.Weights (EvidenceSelection a)
fullDistribution = fmap snd . keyedDistribution

-- Heterogeneous static alternatives and a runtime row family become one typed
-- distribution. Handlers are deliberately reordered: labels match them to the
-- offers. The runtime key and original payload travel together, including zero
-- masses; no function is rendered, serialized or sequenced here.
keyedDistribution :: J.Chosen (EvidenceAlternatives a)
  -> J.Weights (Text, EvidenceSelection a)
keyedDistribution answer = J.choiceWeights answer
  (#candidate (\key row -> (key, Selected row))
    J..| #insufficient (\selection -> ("insufficient", selection))
    J..| #none (\selection -> ("none", selection)))

-- Ordinary metadata is safe to display or record while the original action
-- remains in keyedDistribution. A row key is not an execution authority.
distributionMetadata :: J.Chosen (EvidenceAlternatives a)
  -> [(Text, Double, [(Text, Text)])]
distributionMetadata answer =
  [(key, mass, case selection of
      Selected row -> [("kind", "candidate"), ("evidence_key", evidenceKey row),
        ("source", evidenceSource row), ("excerpt", evidenceExcerpt row)]
      NoUsefulEvidence -> [("kind", "none")]
      InsufficientEvidence -> [("kind", "insufficient")])
    | (mass, (key, selection)) <- J.weighted (keyedDistribution answer)]

-- Supply the exact selected source identity and observed execution outcomes.
-- The existing Jev archive owns request/input, occurrences and raw outcomes;
-- this application envelope adds only serializable decision metadata. It does
-- not claim that a selected continuation ran or that writeFile is durable.
decisionEvidence :: Text -> Text -> [Continuation effects] -> [(Text, Text)]
  -> J.Evaluation (BatchAnswer effects) -> Value
decisionEvidence sourceIdentity intent rows outcomes evaluation = object
  [ "source_identity" .= sourceIdentity
  , "input" .= object
      [ "intent" .= intent
      , "evidence" .= [(evidenceKey row, evidenceSource row, evidenceExcerpt row) | row <- rows]
      ]
  , "observation_archive" .= Recording.archiveValue
      (Recording.recordingArchive (J.recordings evaluation))
  , "distribution" .= either (const Nothing)
      (Just . distributionMetadata . fst) (J.result evaluation)
  , "evaluation_failure" .= either (Just . Text.pack . show)
      (const Nothing) (J.result evaluation)
  , "execution_outcomes" .= outcomes
  ]

compareBatchPolicies :: J.Policy first -> J.Policy second
  -> J.Chosen (EvidenceAlternatives a)
  -> (Either J.Doubt (J.Settled first (EvidenceSelection a)),
      Either J.Doubt (J.Settled second (EvidenceSelection a)))
compareBatchPolicies first second answer =
  (settleEvidence first answer, settleEvidence second answer)

-- This separate, explicit effect is the only point that runs a continuation.
runSelection :: EvidenceSelection (Eff effects Text) -> Eff effects Text
runSelection selection = case selection of
  Selected row -> evidenceValue row
  NoUsefulEvidence -> pure "No supplied item helps."
  InsufficientEvidence -> pure "More evidence is needed."

sampleContinuations :: [Continuation effects]
sampleContinuations =
  [ Evidence "call" "fixture:src/main.rs:83"
      "E0061: save was called with one argument; its definition takes two."
      (pure "Inspect the original call at src/main.rs:83.")
  , Evidence "other" "fixture:src/main.rs:136"
      "E0004: a different match omits None."
      (pure "Inspect the original match at src/main.rs:136.")
  ]

-- The archive is ordinary file content. The caller chooses its path, access
-- policy and publication strength (writeFile makes no durability promise).
saveEvaluation :: Member FsWrite effects
  => Text -> J.Evaluation a -> Eff effects (Either FsError ())
saveEvaluation path evaluation = Files.writeFile path
  (Recording.encodeArchive (Recording.recordingArchive (J.recordings evaluation)))

data ArchiveReadError = ArchiveFileError FsError
  | ArchiveFormatError Recording.ArchiveDecodeError
  deriving (Eq, Show)

loadRecordings :: Member FsRead effects
  => Text -> Eff effects (Either ArchiveReadError Recording.RecordingArchive)
loadRecordings path = do
  contents <- Files.readFile path
  pure $ case contents of
    Left failure -> Left (ArchiveFileError failure)
    Right text -> either (Left . ArchiveFormatError) Right (Recording.decodeArchive text)

-- No Jev membership: reconstruction/replay has no provider effect. Rebuilding
-- from changed source may change captured payloads, but the wire model,
-- question and state must still match the recorded request. Occurrences are
-- retained observations; the replay owner checks the request itself.
replayBatch :: J.Model -> Text -> [Continuation effects] -> Recording.RecordingArchive
  -> J.Evaluation (BatchAnswer effects)
-- BatchKnown prepares the lifted independent query exactly as observeBatch's
-- runQuery does. SkipUnused adds frontier scopes, changing its request paths.
replayBatch model intent rows archive = J.replayProgram J.BatchKnown model
  (J.liftQuery (batchQuestions intent rows)) (Recording.archiveRecords archive)

-- Capture owns ranking and bounded, exact view-fenced expansion. Reassessment
-- is a dependent second request over that retained capture, including failures.
-- A confident False means insufficient evidence; transport failure and policy
-- doubt remain distinct in the retained Evaluation and subsequent judge result.
captureThenAssess
  :: (Member Lookup effects, Member Jev effects, Member Reflect effects)
  => Text -> LookupTools.LookupArguments
  -> Eff effects (LookupTools.LookupCapture Lookup.SelectionEvidence, J.Evaluation J.Yes)
captureThenAssess intent arguments = do
  captured <- Lookup.capture arguments
  let (report, _, truncated) = renderTreeWithNotice 65536 (Lookup.report captured)
      contents = LookupTools.renderCapture captured
      primary = LookupTools.capturePrimary captured
      selectionStatus = case LookupTools.captureSelection captured of
        LookupTools.SelectionSkipped LookupTools.EnrichmentDisabled -> "enrichment disabled" :: Text
        LookupTools.SelectionSkipped LookupTools.PrimaryUnavailable -> "primary unavailable"
        LookupTools.SelectionSkipped LookupTools.NoCandidates -> "no candidates"
        LookupTools.SelectionFailed _ -> "selection failed"
        LookupTools.SelectionSucceeded _ _ -> "selection succeeded"
      expansionReceipt = case LookupTools.captureExpansion captured of
        LookupTools.ExpansionNotRequested -> Nothing
        LookupTools.ExpansionRequested _ batch -> Just (receiptStatus (RawLookup.lookupSearchReceipt batch))
      expansionIssue = case LookupTools.captureExpansion captured of
        LookupTools.ExpansionNotRequested -> Nothing
        LookupTools.ExpansionRequested _ batch -> RawLookup.lookupIssue batch
      selectionFailure = case LookupTools.captureSelection captured of
        LookupTools.SelectionFailed evidence -> case J.result (Lookup.selectionEvaluation evidence) of
          Left failure -> let (text, _, _) = renderTreeWithNotice 8192 (displayTree failure) in Just text
          Right _ -> Nothing
        _ -> Nothing
      world = J.rawState (object
        [ "intent" .= intent, "lookup_capture" .= report
        , "capture_report_truncated" .= truncated
        , "primary_receipt_status" .= receiptStatus (RawLookup.lookupSearchReceipt primary)
        , "primary_issue" .= RawLookup.lookupIssue primary
        , "selection_status" .= selectionStatus
        , "selection_failure" .= selectionFailure
        , "expansion_receipt_status" .= expansionReceipt
        , "expansion_issue" .= expansionIssue
        , "declarations" .= Text.take 65536 contents
        , "declarations_truncated" .= (Text.length contents > 65536)
        ])
  assessed <- J.runQuery (J.question world #sufficient
    (J.noul "Does this retained lookup capture contain sufficient evidence to answer the stated intent? An unavailable, refused or incomplete section is not evidence of absence. Judge only this capture."))
  pure (captured, assessed)

captureSummary :: LookupTools.LookupCapture Lookup.SelectionEvidence -> Text
captureSummary captured = let (text, _, _) = renderTreeWithNotice 65536 (Lookup.report captured) in text

-- These compact known statuses precede any bounded report body. Unknown counts
-- remain null; a missing count never becomes evidence of zero omitted matches.
receiptStatus :: RawLookup.LookupSearchReceipt -> Value
receiptStatus receipt = object
  [ "view" .= RawLookup.searchView receipt
  , "disposition" .= (case RawLookup.searchDisposition receipt of
      RawLookup.LookupSearchNotExecuted -> "not executed" :: Text
      RawLookup.LookupSearchCaptured -> "captured"
      RawLookup.LookupSearchInvalidated _ _ -> "invalidated")
  , "matches" .= [object
      [ "query" .= RawLookup.matchQuery row
      , "observed" .= knownCount (RawLookup.matchObservedOccurrences row)
      , "retained" .= knownCount (RawLookup.matchRetainedOccurrences row)
      , "limit" .= RawLookup.matchLimit row
      ] | row <- RawLookup.searchMatches receipt]
  , "discovery" .= (case RawLookup.searchDiscovery receipt of
      RawLookup.LookupDiscoveryObserved row -> object
        [ "status" .= ("observed" :: Text)
        , "eligible_modules" .= knownCount (RawLookup.discoveryEligibleModules row)
        , "submitted_modules" .= knownCount (RawLookup.discoverySubmittedModules row)
        , "enumerated" .= knownCount (RawLookup.discoveryEnumeratedOccurrences row)
        , "retained" .= knownCount (RawLookup.discoveryRetainedCandidates row)
        , "discarded" .= knownCount (RawLookup.discoveryDiscardedOccurrences row)
        , "distinct_discarded" .= knownCount (RawLookup.discoveryDistinctDiscarded row)
        , "unenumerated" .= knownCount (RawLookup.discoveryUnenumeratedCandidates row)
        , "broader_modules" .= knownCount (RawLookup.discoveryBroaderModuleCount row)
        , "issue" .= RawLookup.discoveryIssue row
        ]
      RawLookup.LookupDiscoveryNotRequested -> object ["status" .= ("not requested" :: Text)]
      RawLookup.LookupDiscoveryInvalidated -> object ["status" .= ("invalidated" :: Text)]
      RawLookup.LookupDiscoveryNotRun reason -> object ["status" .= (case reason of
        RawLookup.LookupSearchStaleView -> "stale view" :: Text
        RawLookup.LookupSearchInvalidRequest -> "invalid request"
        RawLookup.LookupSearchUnavailable -> "unavailable"
        RawLookup.LookupSearchZeroBudget -> "zero budget"
        RawLookup.LookupSearchNoEligibleQuery -> "no eligible query")])
  ]
  where
    knownCount (RawLookup.LookupKnownCount count) = Just count
    knownCount RawLookup.LookupUnknownCount = Nothing

data Report = Report { claim :: Text, evidence :: Text }
data Helper = Helper { report :: Report }

helperReport :: Lens' Helper Report
helperReport = lens report (\h r -> h { report = r })

type ReportState = "claim" J.::= Text J.:& "evidence" J.::= Text

reportState :: Report -> J.State ReportState
reportState r = J.state (#claim J.:= claim r J.:& #evidence J.:= evidence r)

assessReport :: Star (C.Query Value) Report (Either J.Doubt (J.Settled J.Careful Bool))
assessReport = J.assessment reportState $ \world ->
  J.judge J.careful <$> J.question world #supported
    (J.noul ("Does " <> J.field #evidence world <> " support " <> J.field #claim world <> "?"))

reviewPending :: [(Text, Helper)] -> J.Query [(Text, Either J.Doubt (J.Settled J.Careful Bool))]
reviewPending = traverseOf (traversed . _2)
  (runStar (lmap (view helperReport) assessReport))

-- If evidence is incomplete, observe which expansion would help. Speculation
-- can ask both questions in one call without executing the carried Eff actions.
inspectOrContinue :: J.State ReportState -> [(Text, Text, Eff effects ())] -> J.Program (Eff effects ())
inspectOrContinue world sources = ifS
  (J.liftQuery (J.holds J.careful <$> J.question world #complete (J.noul "Does the evidence suffice?")))
  (pure (pure ()))
  (J.liftQuery (J.withUniform repertoire $ \offers ->
    J.taken <$> J.question world #expand (J.choice "Which source would resolve what is missing?" offers)))
  where
    repertoire = J.uniform (J.alt #pause "No source resolves the gap" (pure ()) J..|
      over (J.payloads #source) (\(_,_,action) -> action)
        (J.many #source (\(k,_,_) -> k) (\(_,w,_) -> w) sources))
    -- Edit only one payload family: wire keys and source descriptions stay put.
    -- A separate human form can use the original sources and the same actions.


data Hypothesis = MissingEvidence | Contradiction deriving (Eq, Show)
data Next = ReadMore | Recheck deriving (Eq, Show)

-- One observation supports expected-loss choice and several hypotheses to inspect.
-- The caller defines the losses; the library does not select a universal policy.
comparePolicies :: J.Weights Hypothesis
  -> Either J.DistributionError (Next, [(Double, Hypothesis)])
comparePolicies raw = do
  distribution <- J.normalize raw
  explore <- J.covering 0.9 distribution
  let loss ReadMore MissingEvidence = 1
      loss ReadMore Contradiction = 8
      loss Recheck MissingEvidence = 4
      loss Recheck Contradiction = 1
      chosen = if J.expected (loss ReadMore) distribution <= J.expected (loss Recheck) distribution
        then ReadMore else Recheck
  pure (chosen, explore)
