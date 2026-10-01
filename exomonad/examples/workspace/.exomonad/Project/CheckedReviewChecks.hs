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
  repairQuestion <- awaitOutput owner
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); case flowRepairRequests state of { [response] -> pure (length (flowNotices state) == 1 && any (\\(request, questions) -> request == requestId response && any ((== \"repair-scope\") . questionKey) questions) (flowQuestions state)); _ -> pure False } }"
    (== "True")
  check "a pending repair question retains its owner notification receipt" (repairQuestion == "True")
  pending <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowRepairCount state == 1 && null (flowReviewerRequests state) && length (flowCheckReports state) == 1)"
  check "check repair consumes the shared budget and retains evidence" (lastOutput pending == "True")
  original <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\nlet [(checked, report)] = flowCheckReports state\nlet Just observed = planState report\nlet [entry] = checkEntries observed\nlet Just outcome = checkOutcome entry\ninspectFull (focusedSource (runSpec (checkRun entry)) == renderGitOid (candidateCommit checked) && checkEvidenceComplete entry outcome && checkVerdict entry outcome == CheckFailed)"
  check "failed check retains original job, exact source, and clean terminal receipt"
    (lastOutput original == "True")
  repaired <- checkpoint (checkActor repairing) "review-flow.txt" "pass\n" "repair recovery fixture"
  void $ turn (checkActor repairing) ("respond (Produced (Candidate " <> gitOidLiteral repaired <> " [] []))")
  reviewer <- activation
  reviewedHead <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "successful repaired checks admit exact-source review" (reviewedHead == repaired)
  check "inspection review receives the separately executed check evidence"
    ("Flow check evidence:" `Text.isInfixOf` checkContext reviewer && "recovery" `Text.isInfixOf` checkContext reviewer)
  void $ turn (checkActor reviewer) "let question = Question \"review-scope\" (DesignQuestion \"plans/component.md\" (reviewBase (reviewBasis sessionInput)) \"confirm review scope\" [] [] [])\nreportProgress (WorkProgress [] [question])"
  reviewQuestion <- awaitOutput owner
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); case flowReviewerRequests state of { [response] -> pure (length (flowNotices state) == 2 && any (\\(request, questions) -> request == requestId response && any ((== \"review-scope\") . questionKey) questions) (flowQuestions state)); _ -> pure False } }"
    (== "True")
  check "a pending reviewer question retains its owner notification receipt" (reviewQuestion == "True")
  void $ turn (checkActor reviewer) "respond (Produced (Repair (reviewInput sessionInput) [\"another repair\"]))"
  void $ awaitOutput owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowStage state)" (Text.isInfixOf "RepairBudgetSpent")
  stopped <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (case flowStage state of { ReviewStopped (RepairBudgetSpent 1 _ _) -> True; _ -> False } && length (flowCheckReports state) == 2 && length (flowCheckCleanup state) == 2)"
  check "review cannot get a second repair budget after a check repair" (lastOutput stopped == "True")
  retained <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\nlet hasQuestionHistory request = any (\\(seen, progress) -> seen == request && not (null (workQuestions progress))) (flowProgress state)\ninspectFull (length (flowReviewerReceipts state) == 1 && length (flowCandidateReceipts state) == 2 && length (flowReviewerUpdates state) == 1 && length (flowRepairUpdates state) == 1 && case (flowReviewerRequests state, flowRepairRequests state) of { ([reviewer], [repair]) -> hasQuestionHistory (requestId reviewer) && hasQuestionHistory (requestId repair); _ -> False })"
  check "settlement retains original terminal receipts, progress handles and question history" (lastOutput retained == "True")
  void $ turn owner "R.finish flow"

green :: Member RecipeCheck effects => Eff effects ()
green = greenScenario "green" "ReviewAccepted"

published :: Member RecipeCheck effects => Eff effects ()
published = greenScenario "publish" "ReviewIntegrated"

greenScenario :: Member RecipeCheck effects => Text.Text -> Text.Text -> Eff effects ()
greenScenario scenario expected = do
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
  void $ awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowStage state)"
    (Text.isInfixOf expected)
  result <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowRepairCount state == 0 && length (flowCheckReports state) == 1 && length (flowCheckCleanup state) == 1 && case flowReviewedProof state of { Just _ -> True; _ -> False })"
  check "green review retains one original check and cleans its watcher"
    (lastOutput result == "True")
  headAfter <- git owner ["rev-parse", "HEAD"]
  check "review acceptance does not publish into the owner checkout" (headAfter /= candidate)
  if scenario == "publish" then do
    integrated <- turn owner "inspectFull (case flowStage state of { ReviewIntegrated reviewed (Merge.Published head _ _) -> candidateCommit (reviewedCandidate reviewed) == head; _ -> False })"
    check "accepted exact candidate is published by the existing integration owner" (lastOutput integrated == "True")
    void $ turn owner "case integration of { Just target -> do { _ <- R.finish (Merge.mergeActor target); pure () }; Nothing -> pure () }"
    else pure ()
  void $ turn owner "R.finish flow"

zeroSelection, selectionMismatch, duplicateSelection, missingEvidence, infrastructure, sourceMismatch, escalation
  :: Member RecipeCheck effects => Eff effects ()
zeroSelection = refused "zero" "CandidateChecksUnknown"
selectionMismatch = refused "selection" "CandidateChecksUnknown"
duplicateSelection = refused "duplicates" "CandidateChecksUnknown"
missingEvidence = refused "unknown" "CandidateChecksUnknown"
infrastructure = refused "infrastructure" "CandidateChecksUnknown"
sourceMismatch = refused "mismatch" "CandidateChecksUnknown"
escalation = refused "escalate" "ReviewEscalated"

refused :: Member RecipeCheck effects => Text.Text -> Text.Text -> Eff effects ()
refused scenario expected = do
  owner <- root
  prepareFixture owner scenario
  implementer <- activation
  let mode = if scenario == "escalate" then "fail" else scenario
  candidate <- checkpoint (checkActor implementer) "review-flow.txt" (mode <> "\n") "checked refusal candidate"
  void $ turn (checkActor implementer) ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  void $ awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowStage state)"
    (Text.isInfixOf expected)
  stopped <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowRepairCount state == 0 && null (flowReviewerRequests state) && null (flowRepairRequests state) && length (flowCheckReports state) == 1 && length (flowCheckCleanup state) == 1)"
  check ("check evidence stops at " <> scenario <> " without admitting review or repair")
    (lastOutput stopped == "True")
  void $ turn owner "R.finish flow"
