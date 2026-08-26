{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}
module JournalRoundTripProbe where
import Tidepool.Prelude hiding (render)
import Tidepool.Effects
import Harness
import DevTreeJournal
import HarnessTypes (OnFailure (..), DevPlan (..), FoldReceipt (..), Outcome (..), Failure (..), FailureKind (..), CheckResult (..), ReplanDecision (..), RebaseNote (..), RebaseTier (..))
import Tidepool.Aeson (Value, object, toJSON, (.=))
import qualified Data.Text as T

verify :: Text -> Bool -> Text
verify name ok = (if ok then "PASS: " else "FAIL: ") <> name

childPlan1, childPlan2 :: DevPlan
childPlan1 = DevPlan { nodeName = "c1", nodeTask = "t1", nodeChecks = [], nodeBoundary = [], nodeTolerated = [], nodeOnFailure = Retry, nodeSplit = Nothing, childPlans = [], nodeCycles = Nothing }
childPlan2 = DevPlan { nodeName = "c2", nodeTask = "t2", nodeChecks = [], nodeBoundary = [], nodeTolerated = [], nodeOnFailure = Retry, nodeSplit = Nothing, childPlans = [], nodeCycles = Nothing }

parentPlan :: DevPlan
parentPlan = DevPlan { nodeName = "parent", nodeTask = "t", nodeChecks = [], nodeBoundary = [], nodeTolerated = [], nodeOnFailure = Retry, nodeSplit = Nothing, childPlans = [childPlan1, childPlan2], nodeCycles = Nothing }

splitEv1 :: JournalEvent
splitEv1 = SplitEvent (JournalKey "dev-tree/parent") parentPlan "scaffoldHead0" Nothing

expectedSplitPayload1 :: Value
expectedSplitPayload1 = object [ "node" .= ("parent" :: Text), "scaffoldHead" .= ("scaffoldHead0" :: Text), "children" .= (["c1", "c2"] :: [Text]), "plan" .= toJSON parentPlan ]

caseSplitPayloadFirstAppend :: Text
caseSplitPayloadFirstAppend = verify "split-payload-first-append-matches-legacy-wire" (payloadOf splitEv1 == expectedSplitPayload1)

caseSplitRoundTripFirstAppend :: Text
caseSplitRoundTripFirstAppend = verify "split-round-trips-first-append" (decodeEvent "split" "dev-tree/parent" (payloadOf splitEv1) == Just splitEv1)

splitEv2 :: JournalEvent
splitEv2 = SplitEvent (JournalKey "dev-tree/parent") parentPlan "scaffoldHead0" (Just [("c1", "dev-tree/parent/c1"), ("c2", "dev-tree/parent/c2")])

expectedSplitPayload2 :: Value
expectedSplitPayload2 = object [ "node" .= ("parent" :: Text), "scaffoldHead" .= ("scaffoldHead0" :: Text), "children" .= (["c1", "c2"] :: [Text]), "plan" .= toJSON parentPlan, "childTrees" .= [ object ["name" .= ("c1" :: Text), "branch" .= ("dev-tree/parent/c1" :: Text)], object ["name" .= ("c2" :: Text), "branch" .= ("dev-tree/parent/c2" :: Text)] ] ]

caseSplitPayloadSecondAppend :: Text
caseSplitPayloadSecondAppend = verify "split-payload-second-append-matches-legacy-wire" (payloadOf splitEv2 == expectedSplitPayload2)

caseSplitRoundTripSecondAppend :: Text
caseSplitRoundTripSecondAppend = verify "split-round-trips-second-append" (decodeEvent "split" "dev-tree/parent" (payloadOf splitEv2) == Just splitEv2)

microtaskNames :: [Text]
microtaskNames = ["one", "two"]

microSplitEv :: JournalEvent
microSplitEv = MicroSplitEvent (JournalKey "dev-tree/leaf") microtaskNames

expectedMicroSplitPayload :: Value
expectedMicroSplitPayload = object ["microtasks" .= microtaskNames]

caseMicroSplitPayload :: Text
caseMicroSplitPayload = verify "micro-split-payload-records-accepted-names" (payloadOf microSplitEv == expectedMicroSplitPayload)

caseMicroSplitRoundTrips :: Text
caseMicroSplitRoundTrips = verify "micro-split-round-trips" (decodeEvent "micro-split" "dev-tree/leaf" (payloadOf microSplitEv) == Just microSplitEv)

microCompleteEv :: JournalEvent
microCompleteEv = MicroCompleteEvent (JournalKey "dev-tree/leaf") microtaskNames

caseMicroCompletePayload :: Text
caseMicroCompletePayload = verify "micro-complete-payload-records-accepted-names" (payloadOf microCompleteEv == expectedMicroSplitPayload)

caseMicroCompleteRoundTrips :: Text
caseMicroCompleteRoundTrips = verify "micro-complete-round-trips" (decodeEvent "micro-complete" "dev-tree/leaf" (payloadOf microCompleteEv) == Just microCompleteEv)

mkReceipt :: FoldReceipt
mkReceipt = FoldReceipt { receiptNode = "leaf", receiptBranch = "dev-tree/leaf", receiptSeedHead = "seed0000", receiptHead = "head1111", receiptHeadMoved = True, receiptChecks = [CheckResult "cargo check" 0 ""], receiptRebases = [], receiptOutside = [], receiptCycles = 1, receiptAgentRan = True, receiptReviewed = False, receiptSummary = "done", receiptEvidence = [] }

doneOutcome :: Outcome
doneOutcome = Done { outcomeNode = "leaf", outcomeTrail = [], doneReceipt = mkReceipt }

outcomeEvDone :: JournalEvent
outcomeEvDone = OutcomeEvent (JournalKey "dev-tree/leaf") doneOutcome

caseOutcomeDonePayloadIsBareReceipt :: Text
caseOutcomeDonePayloadIsBareReceipt = verify "outcome-done-payload-is-bare-receipt" (payloadOf outcomeEvDone == toJSON mkReceipt)

caseOutcomeDoneRoundTrips :: Text
caseOutcomeDoneRoundTrips = verify "outcome-done-round-trips" (decodeEvent "outcome" "dev-tree/leaf" (payloadOf outcomeEvDone) == Just outcomeEvDone)

failure1 :: Failure
failure1 = Failure { failureKind = ChecksFailed, failureDetail = "boom", failurePaths = [] }

failedWithReceipt :: Outcome
failedWithReceipt = Failed { outcomeNode = "leaf", outcomeTrail = [], outcomeFailure = failure1, partialReceipt = Just mkReceipt }

outcomeEvFailedWithReceipt :: JournalEvent
outcomeEvFailedWithReceipt = OutcomeEvent (JournalKey "dev-tree/leaf") failedWithReceipt

expectedFailedWithReceiptPayload :: Value
expectedFailedWithReceiptPayload = object ["node" .= ("leaf" :: Text), "failure" .= toJSON failure1, "receipt" .= toJSON mkReceipt]

caseOutcomeFailedWithReceiptPayload :: Text
caseOutcomeFailedWithReceiptPayload = verify "outcome-failed-with-receipt-payload-matches-legacy-wire" (payloadOf outcomeEvFailedWithReceipt == expectedFailedWithReceiptPayload)

caseOutcomeFailedWithReceiptRoundTrips :: Text
caseOutcomeFailedWithReceiptRoundTrips = verify "outcome-failed-with-receipt-round-trips" (decodeEvent "outcome" "dev-tree/leaf" (payloadOf outcomeEvFailedWithReceipt) == Just outcomeEvFailedWithReceipt)

failedNoReceipt :: Outcome
failedNoReceipt = Failed { outcomeNode = "leaf", outcomeTrail = [], outcomeFailure = failure1, partialReceipt = Nothing }

outcomeEvFailedNoReceipt :: JournalEvent
outcomeEvFailedNoReceipt = OutcomeEvent (JournalKey "leaf") failedNoReceipt

expectedFailedNoReceiptPayload :: Value
expectedFailedNoReceiptPayload = object ["node" .= ("leaf" :: Text), "failure" .= toJSON failure1]

caseOutcomeFailedNoReceiptPayload :: Text
caseOutcomeFailedNoReceiptPayload = verify "outcome-failed-no-receipt-payload-matches-legacy-wire" (payloadOf outcomeEvFailedNoReceipt == expectedFailedNoReceiptPayload)

caseOutcomeFailedNoReceiptRoundTrips :: Text
caseOutcomeFailedNoReceiptRoundTrips = verify "outcome-failed-no-receipt-round-trips" (decodeEvent "outcome" "leaf" (payloadOf outcomeEvFailedNoReceipt) == Just outcomeEvFailedNoReceipt)

-- ChildrenPending is the appended partial-delivery verdict (sprint-25 fix):
-- an interior fold with unmerged children journals a Failed wire whose kind
-- CARRIES the per-child ledger.  Round-tripping it here pins the appended
-- constructor's tag stability.
childrenPendingFailure :: Failure
childrenPendingFailure =
  Failure
    { failureKind = ChildrenPending { pendingChildren = ["c1", "c2"], mergedChildren = ["c3"] }
    , failureDetail = "2 of 3 children unmerged — pending amendment/resume"
    , failurePaths = []
    }

childrenPendingOutcome :: Outcome
childrenPendingOutcome = Failed { outcomeNode = "parent", outcomeTrail = [], outcomeFailure = childrenPendingFailure, partialReceipt = Just mkReceipt }

outcomeEvChildrenPending :: JournalEvent
outcomeEvChildrenPending = OutcomeEvent (JournalKey "dev-tree/integration") childrenPendingOutcome

caseChildrenPendingRoundTrips :: Text
caseChildrenPendingRoundTrips = verify "outcome-children-pending-round-trips" (decodeEvent "outcome" "dev-tree/integration" (payloadOf outcomeEvChildrenPending) == Just outcomeEvChildrenPending)

skippedOutcome :: Outcome
skippedOutcome = Skipped { outcomeNode = "leaf", outcomeTrail = [], skipReason = "not merged (subtree abandoned)" }

outcomeEvSkipped :: JournalEvent
outcomeEvSkipped = OutcomeEvent (JournalKey "leaf") skippedOutcome

expectedSkippedPayload :: Value
expectedSkippedPayload = object ["node" .= ("leaf" :: Text), "skipped" .= ("not merged (subtree abandoned)" :: Text)]

caseOutcomeSkippedPayload :: Text
caseOutcomeSkippedPayload = verify "outcome-skipped-payload-matches-legacy-wire" (payloadOf outcomeEvSkipped == expectedSkippedPayload)

caseOutcomeSkippedRoundTrips :: Text
caseOutcomeSkippedRoundTrips = verify "outcome-skipped-round-trips" (decodeEvent "outcome" "leaf" (payloadOf outcomeEvSkipped) == Just outcomeEvSkipped)

decision1 :: ReplanDecision
decision1 = ReplanDecision { amendedInstruction = "try differently", abandonSubtree = False, rationale = "child failed", amendedSubtree = Nothing }

replanEv :: JournalEvent
replanEv = ReplanEvent (JournalKey "dev-tree/child") decision1

caseReplanPayloadIsBareDecision :: Text
caseReplanPayloadIsBareDecision = verify "replan-payload-is-bare-decision" (payloadOf replanEv == toJSON decision1)

caseReplanRoundTrips :: Text
caseReplanRoundTrips = verify "replan-round-trips" (decodeEvent "replan" "dev-tree/child" (payloadOf replanEv) == Just replanEv)

note1 :: RebaseNote
note1 = RebaseNote { rebaseBranch = "dev-tree/child", rebaseOnto = "abc123", rebaseTier = RebaseClean }

rebaseEv :: JournalEvent
rebaseEv = RebaseEvent (JournalKey "dev-tree/child") note1

caseRebasePayloadIsBareNote :: Text
caseRebasePayloadIsBareNote = verify "rebase-payload-is-bare-note" (payloadOf rebaseEv == toJSON note1)

caseRebaseRoundTrips :: Text
caseRebaseRoundTrips = verify "rebase-round-trips" (decodeEvent "rebase" "dev-tree/child" (payloadOf rebaseEv) == Just rebaseEv)

escalationEv :: JournalEvent
escalationEv = EscalationEvent (JournalKey "dev-tree/child") "child" "unresolved conflict"

expectedEscalationPayload :: Value
expectedEscalationPayload = object ["node" .= ("child" :: Text), "detail" .= ("unresolved conflict" :: Text)]

caseEscalationPayload :: Text
caseEscalationPayload = verify "escalation-payload-matches-legacy-wire" (payloadOf escalationEv == expectedEscalationPayload)

caseEscalationRoundTrips :: Text
caseEscalationRoundTrips = verify "escalation-round-trips" (decodeEvent "escalation" "dev-tree/child" (payloadOf escalationEv) == Just escalationEv)

caseEscalationDefaultsMissingNode :: Text
caseEscalationDefaultsMissingNode =
  let p = object ["detail" .= ("unresolved" :: Text)]
      decoded = decodeEvent "escalation" "dev-tree/fallback-key" p
  in verify "escalation-defaults-node-to-key-when-absent" (decoded == Just (EscalationEvent (JournalKey "dev-tree/fallback-key") "dev-tree/fallback-key" "unresolved"))

caseUnknownKindIsNothing :: Text
caseUnknownKindIsNothing = verify "unknown-kind-decodes-to-nothing" (decodeEvent "sprocket" "k" (object []) == Nothing)

__journalRoundTripReport :: Text
__journalRoundTripReport = T.intercalate "\n"
  [ caseSplitPayloadFirstAppend, caseSplitRoundTripFirstAppend
  , caseSplitPayloadSecondAppend, caseSplitRoundTripSecondAppend
  , caseMicroSplitPayload, caseMicroSplitRoundTrips
  , caseMicroCompletePayload, caseMicroCompleteRoundTrips
  , caseOutcomeDonePayloadIsBareReceipt, caseOutcomeDoneRoundTrips
  , caseOutcomeFailedWithReceiptPayload, caseOutcomeFailedWithReceiptRoundTrips
  , caseOutcomeFailedNoReceiptPayload, caseOutcomeFailedNoReceiptRoundTrips
  , caseChildrenPendingRoundTrips
  , caseOutcomeSkippedPayload, caseOutcomeSkippedRoundTrips
  , caseReplanPayloadIsBareDecision, caseReplanRoundTrips
  , caseRebasePayloadIsBareNote, caseRebaseRoundTrips
  , caseEscalationPayload, caseEscalationRoundTrips
  , caseEscalationDefaultsMissingNode, caseUnknownKindIsNothing
  ]
