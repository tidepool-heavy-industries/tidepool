{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}
module ResumeDecisionProbe where
import Tidepool.Prelude hiding (render)
import Tidepool.Effects
import Harness
import HarnessTypes (OnFailure (..), FoldReceipt (..), Failure (..), FailureKind (..), ReplanDecision (..), Outcome (..))
import Tidepool.Resume (ResumeFold (..), ResumeEntry (..), emptyResume, isResumed)
import Resume (descendantAmendPending, rescuePending, rootBranchOf)
import Fold (foldLadder)
import DevTreeJournal (JournalEvent (..), JournalKey (..), payloadOf)
import Tidepool.Aeson (Value, object, toJSON, (.=))
import qualified Data.Text as T

leafPlan :: DevPlan
leafPlan = DevPlan { nodeName = "leaf", nodeTask = "implement the leaf", nodeChecks = [], nodeBoundary = [], nodeTolerated = [], nodeOnFailure = Retry, nodeSplit = Nothing, childPlans = [] }

leafBranch :: Text
leafBranch = "dev-tree/leaf"

mkReceipt :: Text -> Text -> FoldReceipt
mkReceipt node branch = FoldReceipt { receiptNode = node, receiptBranch = branch, receiptSeedHead = "seed0000", receiptHead = "head1111", receiptHeadMoved = True, receiptChecks = [], receiptRebases = [], receiptOutside = [], receiptCycles = 1, receiptAgentRan = True, receiptReviewed = False, receiptSummary = "mock done", receiptEvidence = [] }

mkEntry :: Int -> Text -> Text -> Value -> ResumeEntry
mkEntry sq kind key payload = ResumeEntry { resumeSeq = sq, resumeKind = kind, resumeKey = key, resumePayload = payload }

mkFold :: [ResumeEntry] -> ResumeFold
mkFold es = ResumeFold { resumeRunId = "test-run", resumeEntries = es }

splitPayloadFor :: Text -> DevPlan -> Text -> Value
splitPayloadFor node p scaffoldHead = object [ "node" .= node, "scaffoldHead" .= scaffoldHead, "children" .= map nodeName (childPlans p), "plan" .= toJSON p ]

verify :: Text -> Bool -> Text
verify name ok = (if ok then "PASS: " else "FAIL: ") <> name

-- A recorded outcome for a branch: that subtree is skipped, not re-entered.
caseRecordedOutcome :: Text
caseRecordedOutcome =
  let receipt = mkReceipt "leaf" leafBranch
      fold = mkFold [mkEntry 1 "outcome" leafBranch (toJSON receipt)]
      expected = ResumeSkip (Done "leaf" [] receipt)
  in verify "recorded-outcome-is-skipped" (resumePlanFor foldLadder fold leafBranch leafPlan == expected)

-- A recorded split with no outcome REPLAYS the recorded plan rather than
-- being re-derived.
caseRecordedSplitReplays :: Text
caseRecordedSplitReplays =
  let splitPlan = leafPlan { childPlans = [leafPlan { nodeName = "child" }] }
      payload = splitPayloadFor "leaf" splitPlan "scaffold0"
      fold = mkFold [mkEntry 1 "split" leafBranch payload]
      expected = ResumeReplay SplitRecord { splitNode = "leaf", splitScaffoldHead = "scaffold0", splitPlan = splitPlan, splitChildTrees = [] }
  in verify "recorded-split-replays-not-rederived" (resumePlanFor foldLadder fold leafBranch leafPlan == expected)

-- An outcome recorded BEFORE a newer split is stale: the split is the
-- newest word and must replay rather than skip the branch.
caseStaleOutcomeNewerSplitReplays :: Text
caseStaleOutcomeNewerSplitReplays =
  let splitPlan = leafPlan { childPlans = [leafPlan { nodeName = "child" }] }
      splitPayload = splitPayloadFor "leaf" splitPlan "scaffold-stale-outcome"
      receipt = mkReceipt "leaf" leafBranch
      fold = mkFold [mkEntry 1 "outcome" leafBranch (toJSON receipt), mkEntry 3 "split" leafBranch splitPayload]
      expected = ResumeReplay SplitRecord { splitNode = "leaf", splitScaffoldHead = "scaffold-stale-outcome", splitPlan = splitPlan, splitChildTrees = [] }
  in verify "stale-outcome-newer-split-replays" (resumePlanFor foldLadder fold leafBranch leafPlan == expected)

-- A replan NEWER than the split it amends drives the re-unfold.
caseReplanNewerAmends :: Text
caseReplanNewerAmends =
  let splitPlan = leafPlan { nodeTask = "original task" }
      splitPayload = splitPayloadFor "leaf" splitPlan "scaffold1"
      decision = ReplanDecision { amendedInstruction = "do it the other way", abandonSubtree = False, rationale = "child failed", amendedSubtree = Nothing }
      fold = mkFold [mkEntry 1 "split" leafBranch splitPayload, mkEntry 2 "replan" leafBranch (toJSON decision)]
      expected = ResumeAmend decision (amendPlan decision splitPlan)
  in verify "replan-newer-than-split-amends" (resumePlanFor foldLadder fold leafBranch leafPlan == expected)

-- A replan OLDER than the split it would amend does not: the split still
-- replays under its ORIGINAL recorded plan, and the stale replan plays no
-- part in the decision at all.
caseReplanOlderIgnored :: Text
caseReplanOlderIgnored =
  let splitPlan = leafPlan { nodeTask = "still the original task" }
      splitPayload = splitPayloadFor "leaf" splitPlan "scaffold2"
      decision = ReplanDecision { amendedInstruction = "a stale amendment", abandonSubtree = False, rationale = "stale", amendedSubtree = Nothing }
      fold = mkFold [mkEntry 2 "split" leafBranch splitPayload, mkEntry 1 "replan" leafBranch (toJSON decision)]
      expected = ResumeReplay SplitRecord { splitNode = "leaf", splitScaffoldHead = "scaffold2", splitPlan = splitPlan, splitChildTrees = [] }
  in verify "replan-older-than-split-is-ignored" (resumePlanFor foldLadder fold leafBranch leafPlan == expected)

-- The empty fold. 'resumed' is 'id' when 'isResumed' is False, which is the
-- mechanism that makes 'resumeLoop emptyResume' re-enter the SAME coalgebra
-- 'loop' does rather than a second one that could drift from it; this checks
-- that gate directly, plus the per-branch decision this fold would reach if
-- ever consulted.
caseEmptyFoldIsResumedFalse :: Text
caseEmptyFoldIsResumedFalse = verify "empty-fold-isResumed-false" (isResumed emptyResume == False)

caseEmptyFoldDecidesFresh :: Text
caseEmptyFoldDecidesFresh = verify "empty-fold-resumePlanFor-is-fresh" (resumePlanFor foldLadder emptyResume leafBranch leafPlan == ResumeFresh)

-- Entries recorded under an UNRELATED branch do not leak into this branch's
-- decision: ordinary work, not accidental skipping.
caseUnrelatedBranchIsFresh :: Text
caseUnrelatedBranchIsFresh =
  let otherBranch = "dev-tree/other"
      fold = mkFold [mkEntry 1 "outcome" otherBranch (toJSON (mkReceipt "other" otherBranch))]
  in verify "unrelated-branch-entries-do-not-skip" (resumePlanFor foldLadder fold leafBranch leafPlan == ResumeFresh)

-- amendmentIsNewest, directly: the same sequence rule exposed as its
-- retained pure helper, including the outcome comparison.
caseAmendmentIsNewest :: [Text]
caseAmendmentIsNewest =
  [ verify "amendmentIsNewest-no-replan-is-false" (amendmentIsNewest Nothing (Just 5) (Just 3) == False)
  , verify "amendmentIsNewest-replan-newer-than-both" (amendmentIsNewest (Just 4) (Just 2) (Just 1) == True)
  , verify "amendmentIsNewest-replan-equal-to-split-not-newer" (amendmentIsNewest (Just 2) (Just 2) Nothing == False)
  , verify "amendmentIsNewest-replan-newer-than-split-older-than-outcome" (amendmentIsNewest (Just 3) (Just 1) (Just 5) == False)
  , verify "amendmentIsNewest-replan-with-no-prior-split-or-outcome" (amendmentIsNewest (Just 0) Nothing Nothing == True)
  ]

-- Run 24's exact journal shape (2026-08-25), WIRE-FAITHFUL: the journal's
-- outcome payloads are the PRE-ladder bare receipts (a bare receipt decodes
-- 'Done' regardless of verdict), with the failure evidence living in the
-- receipt fields: receiptHeadMoved = False for the panel leaf's NoHeadMove,
-- non-empty receiptOutside for the root's BoundaryViolated.  The first
-- version of this case wrote Failed-wire payloads the real writer never
-- produces, and passed against code the real journal no-oped: a
-- wire-unfaithful fixture is a false receipt.  The child's replan is NEWER
-- than its own outcome but OLDER than the root's outcome, the sequence that
-- must still rescue (the fold journals a child's replan BEFORE the parent's
-- outcome by construction).
run24Panel, run24Seam, run24Outline, run24Root :: DevPlan
run24Panel = leafPlan { nodeName = "panel", nodeOnFailure = Replan }
run24Seam = leafPlan { nodeName = "seam" }
run24Outline = leafPlan { nodeName = "outline" }
run24Root = leafPlan { nodeName = "root-prd", nodeOnFailure = Replan, childPlans = [run24Seam, run24Panel, run24Outline] }

run24RootBranch, run24PanelBranch :: Text
run24RootBranch = "wt/root-integration"
run24PanelBranch = "wt/panel"

run24Decision :: ReplanDecision
run24Decision = ReplanDecision { amendedInstruction = "implement; do not stop at reconnaissance", abandonSubtree = False, rationale = "procedural failure", amendedSubtree = Nothing }

run24Fold :: ResumeFold
run24Fold = mkFold
  [ mkEntry 0 "propose" "root-plan" (toJSON run24Root)
  , mkEntry 2 "split" run24RootBranch (payloadOf (SplitEvent (JournalKey run24RootBranch) run24Root "scaffold24" (Just [("seam", "wt/seam"), ("panel", run24PanelBranch), ("outline", "wt/outline")])))
  , mkEntry 3 "outcome" "wt/seam" (toJSON (mkReceipt "seam" "wt/seam"))
  , mkEntry 4 "outcome" run24PanelBranch (toJSON (mkReceipt "panel" run24PanelBranch) { receiptHeadMoved = False })
  , mkEntry 5 "outcome" "wt/outline" (toJSON (mkReceipt "outline" "wt/outline"))
  , mkEntry 7 "replan" run24PanelBranch (toJSON run24Decision)
  , mkEntry 8 "outcome" run24RootBranch (toJSON (mkReceipt "root-prd" run24RootBranch) { receiptOutside = ["tidepool-harness/tests/strayed.rs"] })
  ]

caseRun24Rescue :: [Text]
caseRun24Rescue =
  [ verify "run24-rootBranchOf-finds-the-integration-branch" (rootBranchOf run24Fold "root-prd" == Just run24RootBranch)
  , verify "run24-panel-branch-verdict-is-amend" (case resumePlanFor foldLadder run24Fold run24PanelBranch run24Panel of ResumeAmend {} -> True; _ -> False)
  , verify "run24-descendant-amend-pending" (descendantAmendPending foldLadder run24Fold run24Root)
  , verify "run24-root-verdict-reenters-not-skips" (case resumePlanFor foldLadder run24Fold run24RootBranch run24Root of ResumeReplay {} -> True; _ -> False)
  , verify "run24-rescuePending-reenters-completed-run" (rescuePending foldLadder run24Fold run24RootBranch run24Root)
  ]

__resumeDecisionReport :: Text
__resumeDecisionReport = T.intercalate "\n" ( [ caseRecordedOutcome, caseRecordedSplitReplays, caseStaleOutcomeNewerSplitReplays, caseReplanNewerAmends, caseReplanOlderIgnored, caseEmptyFoldIsResumedFalse, caseEmptyFoldDecidesFresh, caseUnrelatedBranchIsFresh ] <> caseAmendmentIsNewest <> caseRun24Rescue )
