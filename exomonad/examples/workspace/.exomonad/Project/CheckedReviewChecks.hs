{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.CheckedReviewChecks
  ( continuation, green, zeroSelection, missingEvidence, infrastructure, sourceMismatch, escalation ) where

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
  pending <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowRepairCount state == 1 && null (flowReviewerRequests state) && length (flowCheckReports state) == 1)"
  check "check repair consumes the shared budget and retains evidence" (lastOutput pending == "True")
  original <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\nlet [(checked, report)] = flowCheckReports state\nlet Just observed = planState report\nlet [entry] = checkEntries observed\nlet Just outcome = checkOutcome entry\ninspectFull (focusedSource (runSpec (checkRun entry)) == renderGitOid (candidateCommit checked) && runJob (checkRun entry) == Cmd.completedJob (focusedCommand (checkFocused outcome)) && checkSourceAssurance entry outcome == SourceVerified && checkVerdict entry outcome == CheckFailed && Cmd.commandCleanup (checkCompletion outcome) == Cmd.CommandClean)"
  check "failed check retains original job, exact source, and clean terminal receipt"
    (lastOutput original == "True")
  repaired <- checkpoint (checkActor repairing) "review-flow.txt" "pass\n" "repair recovery fixture"
  void $ turn (checkActor repairing) ("respond (Produced (Candidate " <> gitOidLiteral repaired <> " [] []))")
  reviewer <- activation
  reviewedHead <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "successful repaired checks admit exact-source review" (reviewedHead == repaired)
  void $ turn (checkActor reviewer) "respond (Produced (Repair (reviewInput sessionInput) [\"another repair\"]))"
  void $ awaitOutput owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowStage state)" (Text.isInfixOf "RepairBudgetSpent")
  stopped <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (case flowStage state of { ReviewStopped (RepairBudgetSpent 1 _ _) -> True; _ -> False } && length (flowCheckReports state) == 2 && length (flowCheckCleanup state) == 2)"
  check "review cannot get a second repair budget after a check repair" (lastOutput stopped == "True")
  void $ turn owner "R.finish flow"

green :: Member RecipeCheck effects => Eff effects ()
green = do
  owner <- root
  prepareFixture owner "green"
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
    (Text.isInfixOf "ReviewAccepted")
  result <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (case flowStage state of { ReviewAccepted _ -> True; _ -> False } && flowRepairCount state == 0 && length (flowCheckReports state) == 1 && length (flowCheckCleanup state) == 1)"
  check "green review retains one original check and cleans its watcher"
    (lastOutput result == "True")
  headAfter <- git owner ["rev-parse", "HEAD"]
  check "review acceptance does not publish into the owner checkout" (headAfter /= candidate)
  void $ turn owner "R.finish flow"

zeroSelection, missingEvidence, infrastructure, sourceMismatch, escalation
  :: Member RecipeCheck effects => Eff effects ()
zeroSelection = refused "zero" "CandidateChecksUnknown"
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
