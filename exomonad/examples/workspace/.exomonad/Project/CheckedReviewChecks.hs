{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.CheckedReviewChecks
  ( continuation, green, published, zeroSelection, selectionMismatch, duplicateSelection
  , missingEvidence, infrastructure, sourceMismatch, escalation ) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Exomonad.Workspace (workspaceRoot)
import Tidepool.Check
import Project.Checks (script)

prepareFixture :: Member RecipeCheck effects => CheckActor -> Text.Text -> Eff effects ()
prepareFixture owner scenario = do
  runner <- readFile owner (Text.pack workspaceRoot <> "/checks/checked-review-fixture.sh")
  writeFile owner "scripts/cargo-focused-test" runner
  void $ git owner ["add", "--", "scripts/cargo-focused-test"]
  void $ git owner ["update-index", "--chmod=+x", "--", "scripts/cargo-focused-test"]
  void $ git owner ["checkout-index", "-f", "--", "scripts/cargo-focused-test"]
  void $ git owner ["commit", "-m", "make fixture executable"]
  status <- git owner ["status", "--porcelain"]
  check "fixture source is clean after executable runner commit" (Text.null status)
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet scenario = " <> Text.pack (show scenario) <> " :: Text")
  script owner "checked-review-setup"

continuation :: Member RecipeCheck effects => Eff effects ()
continuation = do
  owner <- root
  prepareFixture owner "recovery"
  implementer <- activation
  first <- checkpoint (checkActor implementer) "review-flow.txt" "fail\n" "failing recovery fixture"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> gitOidLiteral first <> " [] []))")
  repairing <- activation
  check "failed counted check routes to retained implementer before review"
    (checkActor repairing == checkActor implementer)
  void $ turn (checkActor repairing) "let question = Question \"repair-scope\" (DesignQuestion \"plans/component.md\" (taskSource (repairAssignment sessionInput)) \"confirm repair scope\" [] [] [])\nreportProgress (WorkProgress [] [question])"
  awaitCell owner "pending repair question retains owner notification receipt"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowRepairRequests state of { [response] -> length (flowNotices state) == 1 && any (\\(request, questions) -> request == requestId response && any ((== \"repair-scope\") . questionKey) questions) (flowQuestions state); _ -> False }) }"
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowRepairCount state == 1 && null (flowReviewerRequests state) && length (flowCheckReports state) == 1)"
  assertCell owner "check repair consumes shared budget and retains evidence"
    "flowRepairCount state == 1 && null (flowReviewerRequests state) && length (flowCheckReports state) == 1"
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\nlet [(checked, report)] = flowCheckReports state\nlet Just observed = planState report\nlet [entry] = checkEntries observed\nlet Just outcome = checkOutcome entry\ninspectFull (focusedSource (runSpec (checkRun entry)) == renderGitOid (candidateCommit checked) && checkEvidenceComplete entry outcome && checkVerdict entry outcome == CheckFailed)"
  assertCell owner "failed check retains original job, exact source, and clean terminal receipt"
    "focusedSource (runSpec (checkRun entry)) == renderGitOid (candidateCommit checked) && checkEvidenceComplete entry outcome && checkVerdict entry outcome == CheckFailed"
  repaired <- checkpoint (checkActor repairing) "review-flow.txt" "pass\n" "repair recovery fixture"
  void $ turn (checkActor repairing) ("respond (Produced (Candidate " <> gitOidLiteral repaired <> " [] []))")
  reviewer <- activation
  reviewedHead <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "successful repaired checks admit exact-source review" (reviewedHead == repaired)
  check "inspection review receives the separately executed check evidence"
    ("Flow check evidence:" `Text.isInfixOf` checkContext reviewer && "recovery" `Text.isInfixOf` checkContext reviewer)
  void $ turn (checkActor reviewer) "let question = Question \"review-scope\" (DesignQuestion \"plans/component.md\" (reviewBase (reviewBasis sessionInput)) \"confirm review scope\" [] [] [])\nreportProgress (WorkProgress [] [question])"
  awaitCell owner "pending reviewer question retains owner notification receipt"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowReviewerRequests state of { [response] -> length (flowNotices state) == 2 && any (\\(request, questions) -> request == requestId response && any ((== \"review-scope\") . questionKey) questions) (flowQuestions state); _ -> False }) }"
  void $ turn (checkActor reviewer) "respond (Produced (Repair (reviewInput sessionInput) [\"another repair\"]))"
  awaitCell owner "shared repair budget reaches terminal refusal"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewStopped (RepairBudgetSpent 1 _ _) -> True; _ -> False }) }"
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (case flowStage state of { ReviewStopped (RepairBudgetSpent 1 _ _) -> True; _ -> False } && length (flowCheckReports state) == 2 && length (flowCheckCleanup state) == 2)"
  assertCell owner "review cannot get second repair budget after check repair"
    "case flowStage state of { ReviewStopped (RepairBudgetSpent 1 _ _) -> length (flowCheckReports state) == 2 && length (flowCheckCleanup state) == 2; _ -> False }"
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\nlet hasQuestionHistory request = any (\\(seen, progress) -> seen == request && not (null (workQuestions progress))) (flowProgress state)\ninspectFull (length (flowReviewerReceipts state) == 1 && length (flowCandidateReceipts state) == 2 && length (flowReviewerUpdates state) == 1 && length (flowRepairUpdates state) == 1 && case (flowReviewerRequests state, flowRepairRequests state) of { ([reviewer], [repair]) -> hasQuestionHistory (requestId reviewer) && hasQuestionHistory (requestId repair); _ -> False })"
  assertCell owner "settlement retains original receipts, progress handles and question history"
    "length (flowReviewerReceipts state) == 1 && length (flowCandidateReceipts state) == 2 && length (flowReviewerUpdates state) == 1 && length (flowRepairUpdates state) == 1 && case (flowReviewerRequests state, flowRepairRequests state) of { ([reviewer], [repair]) -> hasQuestionHistory (requestId reviewer) && hasQuestionHistory (requestId repair); _ -> False }"
  void $ turn owner "R.finish flow"

green :: Member RecipeCheck effects => Eff effects ()
green = greenScenario "green" False

published :: Member RecipeCheck effects => Eff effects ()
published = greenScenario "publish" True

greenScenario :: Member RecipeCheck effects => Text.Text -> Bool -> Eff effects ()
greenScenario scenario publishes = do
  owner <- root
  prepareFixture owner scenario
  implementer <- activation
  candidate <- checkpoint (checkActor implementer) "review-flow.txt" "pass\n" "passing checked candidate"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  headSeen <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "green candidate reaches an exact-source reviewer" (headSeen == candidate)
  void $ turn (checkActor reviewer)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [\"fixture passed\"] \"checked exact source\")))"
  awaitCell owner "green review reaches its requested terminal stage"
    (if publishes
      then "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewIntegrated _ (Merge.Published _ _ _) -> True; _ -> False }) }"
      else "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewAccepted _ -> True; _ -> False }) }")
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()"
  assertCell owner "green review retains original check and cleans watcher"
    "flowRepairCount state == 0 && length (flowCheckReports state) == 1 && length (flowCheckCleanup state) == 1 && case flowReviewedProof state of { Just _ -> True; _ -> False }"
  headAfter <- git owner ["rev-parse", "HEAD"]
  check "review acceptance does not publish into the owner checkout" (headAfter /= candidate)
  if publishes then do
    assertCell owner "accepted exact candidate is published by existing integration owner"
      "case flowStage state of { ReviewIntegrated reviewed (Merge.Published head _ _) -> candidateCommit (reviewedCandidate reviewed) == head; _ -> False }"
    void $ turn owner "case integration of { Just target -> do { _ <- R.finish (Merge.mergeActor target); pure () }; Nothing -> pure () }"
    else pure ()
  void $ turn owner "R.finish flow"

zeroSelection, selectionMismatch, duplicateSelection, missingEvidence, infrastructure, sourceMismatch, escalation
  :: Member RecipeCheck effects => Eff effects ()
zeroSelection = refused "zero" False
selectionMismatch = refused "selection" False
duplicateSelection = refused "duplicates" False
missingEvidence = refused "unknown" False
infrastructure = refused "infrastructure" False
sourceMismatch = refused "mismatch" False
escalation = refused "escalate" True

refused :: Member RecipeCheck effects => Text.Text -> Bool -> Eff effects ()
refused scenario escalates = do
  owner <- root
  prepareFixture owner scenario
  implementer <- activation
  let mode = if escalates then "fail" else scenario
  candidate <- checkpoint (checkActor implementer) "review-flow.txt" (mode <> "\n") "checked refusal candidate"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  awaitCell owner "refused checks reach their requested terminal stage"
    (if escalates
      then "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewStopped (ReviewEscalated _) -> True; _ -> False }) }"
      else "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewStopped (CandidateChecksUnknown _) -> True; _ -> False }) }")
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()"
  assertCell owner ("check evidence stops at " <> scenario <> " without admitting review or repair")
    "flowRepairCount state == 0 && null (flowReviewerRequests state) && null (flowRepairRequests state) && length (flowCheckReports state) == 1 && length (flowCheckCleanup state) == 1"
  void $ turn owner "R.finish flow"
