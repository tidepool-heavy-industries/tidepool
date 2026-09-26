{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.ReviewFlowChecks (oneComponent, failurePaths, sourcePreflight, emptyFindings) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script)

oneComponent :: Member RecipeCheck effects => Eff effects ()
oneComponent = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-loop-first\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-coordinator-first\" :: Text"
    <> "\nlet sourcePlan = RequiresSiblingCommits [sourceHead]")
  script owner "review-flow-loop"
  implementer <- activation
  first <- checkpoint (checkActor implementer) "review-flow.txt" "first candidate\n" "initial candidate"
  void $ turn (checkActor implementer)
    ("respond (Produced (Candidate " <> gitOidLiteral first <> " [] [\"owner integration\"]))")
  reviewer <- activation
  firstState <- awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show state)"
    ("ReviewingCandidate" `Text.isInfixOf`)
  check "candidate settlement reached review flow"
    ("ReviewingCandidate" `Text.isInfixOf` firstState)
  headSeen <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "first review starts fresh at the submitted candidate" (headSeen == first)
  void $ turn (checkActor reviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) []))"
  correction <- activation
  check "empty findings return to the same reviewer"
    (checkActor correction == checkActor reviewer)
  void $ turn (checkActor correction)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) [\"fix the component\"]))"
  repairWorker <- activation
  check "repair returns directly to the retained implementer"
    (checkActor repairWorker == checkActor implementer)
  revised <- checkpoint (checkActor repairWorker) "review-flow.txt" "repaired candidate\n" "repair candidate"
  void $ turn (checkActor repairWorker)
    ("respond (Produced (Candidate " <> gitOidLiteral revised <> " [] [\"owner integration\"]))")
  second <- activation
  secondHead <- git (checkActor second) ["rev-parse", "HEAD"]
  check "revised source gets a new exact-source reviewer"
    (checkActor second /= checkActor reviewer && secondHead == revised)
  void $ turn (checkActor second)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [\"read exact source\"] \"accepted\")))"
  accepted <- awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowRepairCount state, length (flowReviewerRequests state), length (flowRepairRequests state)))"
    ("ReviewAccepted" `Text.isInfixOf`)
  check "one component finishes with exact reviewed evidence, correction, and one repair"
    ("ReviewAccepted" `Text.isInfixOf` accepted
      && revised `Text.isInfixOf` accepted
      && "1,3,1" `Text.isInfixOf` accepted)
  void $ turn owner "R.finish flow"

failurePaths :: Member RecipeCheck effects => Eff effects ()
failurePaths = do
  owner2 <- root
  base2 <- git owner2 ["rev-parse", "HEAD"]
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral base2
    <> "\nlet limit = 0 :: Int\nlet campaignName = \"review-loop-budget\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-coordinator-budget\" :: Text"
    <> "\nlet sourcePlan = ComponentReview")
  script owner2 "review-flow-loop"
  limited <- activation
  candidate <- checkpoint (checkActor limited) "review-flow.txt" "budget candidate\n" "budget source"
  void $ turn (checkActor limited)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  budgetReviewer <- activation
  void $ turn (checkActor budgetReviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) [\"one finding\"]))"
  budget <- awaitOutput owner2
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowRepairCount state, length (flowRepairRequests state)))"
    ("RepairBudgetSpent" `Text.isInfixOf`)
  check "zero repair budget stops without dispatching another request"
    ("RepairBudgetSpent" `Text.isInfixOf` budget
      && "0,0)" `Text.isInfixOf` budget)
  void $ turn owner2 "R.finish flow"
  void restart
  owner3 <- root
  base3 <- git owner3 ["rev-parse", "HEAD"]
  void $ turn owner3 ("let sourceHead = " <> gitOidLiteral base3
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-loop-stale\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-coordinator-stale\" :: Text"
    <> "\nlet sourcePlan = ComponentReview")
  script owner3 "review-flow-loop"
  mismatched <- activation
  actual <- checkpoint (checkActor mismatched) "review-flow.txt" "different source\n" "mismatch source"
  void $ turn (checkActor mismatched)
    ("respond (Produced (Candidate " <> gitOidLiteral base3 <> " [] []))")
  stopped <- awaitOutput owner3
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, length (flowReviewerRequests state)))"
    ("CandidateSourceRefused" `Text.isInfixOf`)
  check "a stale candidate claim stops before reviewer admission"
    ("CandidateSourceRefused" `Text.isInfixOf` stopped
      && base3 `Text.isInfixOf` stopped
      && actual `Text.isInfixOf` stopped
      && ",0)" `Text.isInfixOf` stopped)
  void $ turn owner3 "R.finish flow"

sourcePreflight :: Member RecipeCheck effects => Eff effects ()
sourcePreflight = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  sibling <- checkpoint owner "sibling-source.txt" "required sibling\n" "required sibling source"
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 2 :: Int\nlet campaignName = \"review-loop-missing\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-coordinator-missing\" :: Text"
    <> "\nlet sourcePlan = RequiresSiblingCommits [" <> gitOidLiteral sibling <> "]")
  script owner "review-flow-loop"
  worker <- activation
  partial <- checkpoint (checkActor worker) "review-flow.txt" "component only\n" "partial component"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral partial <> " [] []))")
  missing <- awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, length (flowReviewerRequests state)))"
    ("RequiredSiblingMissing" `Text.isInfixOf`)
  check "missing required sibling stops before product reviewer admission"
    ("RequiredSiblingMissing" `Text.isInfixOf` missing
      && sibling `Text.isInfixOf` missing
      && partial `Text.isInfixOf` missing
      && ",0)" `Text.isInfixOf` missing)
  void $ turn owner "R.finish flow"
  void restart
  owner2 <- root
  base2 <- git owner2 ["rev-parse", "HEAD"]
  sibling2 <- checkpoint owner2 "sibling-source.txt" "separate sibling\n" "separate sibling source"
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral base2
    <> "\nlet limit = 2 :: Int\nlet campaignName = \"review-loop-partial\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-coordinator-partial\" :: Text"
    <> "\nlet sourcePlan = ComponentReview")
  script owner2 "review-flow-loop"
  worker2 <- activation
  partial2 <- checkpoint (checkActor worker2) "review-flow.txt" "component only\n" "partial component"
  void $ turn (checkActor worker2)
    ("respond (Produced (Candidate " <> gitOidLiteral partial2 <> " [] []))")
  reviewer <- activation
  reviewHead <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "declared component review admits exact partial source"
    (reviewHead == partial2 && sibling2 /= partial2)
  void $ turn (checkActor reviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [] \"component accepted\")))"
  accepted <- awaitOutput owner2
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state))"
    ("ReviewAccepted" `Text.isInfixOf`)
  check "component acceptance remains scoped to its submitted candidate"
    ("ReviewAccepted" `Text.isInfixOf` accepted && partial2 `Text.isInfixOf` accepted)
  void $ turn owner2 "R.finish flow"

emptyFindings :: Member RecipeCheck effects => Eff effects ()
emptyFindings = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 2 :: Int\nlet campaignName = \"review-loop-empty\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-coordinator-empty\" :: Text"
    <> "\nlet sourcePlan = ComponentReview")
  script owner "review-flow-loop"
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "empty findings\n" "empty findings source"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  void $ turn (checkActor reviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) []))"
  correction <- activation
  check "first empty Repair queues one continuation to the same reviewer"
    (checkActor correction == checkActor reviewer)
  void $ turn (checkActor correction)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) []))"
  stopped <- awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowRepairCount state, length (flowReviewerRequests state), length (flowRepairRequests state)))"
    ("EmptyRepairFindings" `Text.isInfixOf`)
  check "second empty Repair stops without implementer dispatch"
    ("EmptyRepairFindings" `Text.isInfixOf` stopped
      && candidate `Text.isInfixOf` stopped
      && "0,2,0)" `Text.isInfixOf` stopped)
  void $ turn owner "R.finish flow"
