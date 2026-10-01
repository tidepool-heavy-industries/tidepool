{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.ReviewFlowChecks (oneComponent, failurePaths, sourcePreflight, emptyFindings, effectfulRouting, workflowExample, lateCandidate, replacement) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script, checkSource)

oneComponent :: Member RecipeCheck effects => Eff effects ()
oneComponent = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-loop-first\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-coordinator-first\" :: Text"
    <> "\nlet sourcePlan = RequiresSiblingCommits [sourceHead]")
  script owner "review-flow-loop"
  pending <- turn owner "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  check "cleanup refuses while the candidate is pending"
    ("ReviewCleanupPending AwaitingCandidate" `Text.isInfixOf` output pending)
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
  pendingReview <- turn owner "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  check "cleanup refuses with a live reviewer"
    ("ReviewCleanupPending (ReviewingCandidate" `Text.isInfixOf` output pendingReview)
  firstInterview <- turn (checkActor reviewer) "let cleanupInterview = \"initial reviewer found a component issue\" :: Text\ncleanupInterview"
  check "first reviewer interview is available before cleanup"
    ("initial reviewer found a component issue" `Text.isInfixOf` output firstInterview)
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
  pendingRepair <- turn owner "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  check "cleanup refuses while repair is pending"
    ("ReviewCleanupPending (AwaitingRepair" `Text.isInfixOf` output pendingRepair)
  revised <- checkpoint (checkActor repairWorker) "review-flow.txt" "repaired candidate\n" "repair candidate"
  void $ turn (checkActor repairWorker)
    ("respond (Produced (Candidate " <> gitOidLiteral revised <> " [] [\"owner integration\"]))")
  second <- activation
  secondHead <- git (checkActor second) ["rev-parse", "HEAD"]
  check "revised source gets a new exact-source reviewer"
    (checkActor second /= checkActor reviewer && secondHead == revised)
  interview <- turn (checkActor second) "let cleanupInterview = \"reviewer checked the repaired source\" :: Text\ncleanupInterview"
  check "reviewer interview is retained before cleanup"
    ("reviewer checked the repaired source" `Text.isInfixOf` output interview)
  void $ turn (checkActor second)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [\"read exact source\"] \"accepted\")))"
  accepted <- awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowRepairCount state, length (flowReviewerRequests state), length (flowRepairRequests state)))"
    ("ReviewAccepted" `Text.isInfixOf`)
  check "one component finishes with exact reviewed evidence, correction, and one repair"
    ("ReviewAccepted" `Text.isInfixOf` accepted
      && revised `Text.isInfixOf` accepted
      && "1,3,1" `Text.isInfixOf` accepted)
  script owner "review-flow-cleanup"
  cleanup <- turn owner "case cleanup of { ReviewCleanupAttempted groups -> inspectFull (length groups == 2 && map (length . snd) groups == [1,1]); _ -> inspectFull False }"
  check "coordinator records cleanup of both distinct reviewer groups"
    (lastOutput cleanup == "True")
  repeated <- turn owner "again <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\nstate <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show again == show cleanup && show (flowCleanupResult state) == show (Just cleanup))"
  check "repeat and snapshot retain original cleanup outcomes"
    (lastOutput repeated == "True")
  retry <- turn owner "retry <- R.call (reviewCleanup (R.client flow)) ReviewCleanupRetryRefused\nlet refused receipts = case reverse receipts of { latest:_ -> case cleanupReceiptSteps latest of { [CleanupBlocked _] -> True; [CleanupStalePlan] -> True; _ -> False }; [] -> True }\ncase (cleanup, retry) of { (ReviewCleanupAttempted before, ReviewCleanupAttempted after) -> inspectFull (length before == length after && and [length new == length old + (if refused old then 1 else 0) | ((_,old),(_,new)) <- zip before after]); _ -> inspectFull False }"
  check "explicit retry only reruns previously refused groups"
    (lastOutput retry == "True")
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
  budgetCleanup <- turn owner2 "receipt <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (case receipt of { ReviewCleanupAttempted groups -> (length groups, map (map cleanupReceiptComplete . snd) groups); _ -> (0 :: Int, []) })"
  check "stopped review still retains its reviewer cleanup receipt"
    ("(1," `Text.isInfixOf` output budgetCleanup)
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
  none <- turn owner3 "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  check "terminal flow without a reviewer has no reviewer group to clean"
    ("ReviewCleanupNoReviewer" `Text.isInfixOf` output none)
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

effectfulRouting :: Member RecipeCheck effects => Eff effects ()
effectfulRouting = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet campaignName = \"review-flow-effectful\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-effectful-coordinator\" :: Text")
  script owner "review-flow-effectful"
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "routed candidate\n" "routed source"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  void $ turn (checkActor reviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) [\"repair changes the task contract\"]))"
  routed <- awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (case (flowStage state, flowReviewRoutes state) of { (ReviewStopped (ReviewEscalated reason), [(_, ReviewRouteResult (EscalateReview selected) DeterministicRoute)]) -> reason == \"owner must decide this scope change\" && selected == reason && flowRepairCount state == 0; _ -> False })"
    ("True" `Text.isInfixOf`)
  check "effectful route escalates a valid exact-source Repair without dispatching repair"
    ("True" `Text.isInfixOf` routed)
  notice <- turn owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (length (flowNotices state))"
  check "owner receives one escalation notice"
    (lastOutput notice == "1")
  missing <- turn owner
    "let selected = Candidate sourceHead [] []\nresult <- semanticReviewChoice (ReviewContext task selected (Repair selected [\"scope is unclear\"]) 0 1 [])\ninspectFull (show result)"
  check "semantic route without escalation criteria returns typed parent escalation"
    ("EscalateReview" `Text.isInfixOf` output missing
      && "RouteCriteriaMissing" `Text.isInfixOf` output missing)
  empty <- turn owner
    "result <- semanticReviewChoice (ReviewContext task selected (Repair selected []) 0 1 [])\ninspectFull (show result)"
  check "empty Repair stays on the bounded same-reviewer correction path"
    ("HonorReview" `Text.isInfixOf` output empty
      && "DeterministicRoute" `Text.isInfixOf` output empty)
  cleanup <- turn owner
    "receipt <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show receipt)"
  check "effectful escalation retains cleanup receipt after terminal review"
    ("ReviewCleanupAttempted" `Text.isInfixOf` output cleanup)
  void $ turn owner "R.finish flow"
  void restart
  owner2 <- root
  baseline2 <- git owner2 ["rev-parse", "HEAD"]
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral baseline2
    <> "\nlet limit = 1 :: Int\nlet sourcePlan = ComponentReview"
    <> "\nlet campaignName = \"review-flow-semantic\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-flow-semantic-coordinator\" :: Text")
  script owner2 "review-flow-semantic"
  worker2 <- activation
  candidate2 <- checkpoint (checkActor worker2) "review-flow.txt" "semantic consumer\n" "semantic source"
  void $ turn (checkActor worker2)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate2 <> " [] []))")
  reviewer2 <- activation
  void $ turn (checkActor reviewer2)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [] \"checked exact source\")))"
  accepted <- awaitOutput owner2
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, map (routeEvidence . snd) (flowReviewRoutes state)))"
    ("ReviewAccepted" `Text.isInfixOf`)
  check "compiled semantic consumer preserves a reviewer's exact-source acceptance"
    ("ReviewAccepted" `Text.isInfixOf` accepted
      && "DeterministicRoute" `Text.isInfixOf` accepted)
  void $ turn owner2 "R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce"
  void $ turn owner2 "R.finish flow"

-- A fixture reviewer supplies the interview answer. This checks that the
-- owner retains an actual response before cleanup; it does not assess an
-- interview's quality or infer host release from the cleanup receipt.
workflowExample :: Member RecipeCheck effects => Eff effects ()
workflowExample = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral source
    <> "\nlet campaignName = \"review-workflow-accepted\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-workflow-accepted-coordinator\" :: Text"
    <> "\nlet routeCriteria = [\"Escalate if acceptance or paths change.\"] :: [Text]")
  script owner "review-flow-workflow"
  pending <- turn owner "inspectFull (show pendingCleanup)"
  check "workflow refuses owner cleanup before the candidate settles"
    ("ReviewCleanupPending AwaitingCandidate" `Text.isInfixOf` output pending)
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "accepted workflow\n" "workflow candidate"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  void $ turn (checkActor reviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [\"read exact source\"] \"accepted\")))"
  accepted <- awaitOutput owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state))"
    ("ReviewAccepted" `Text.isInfixOf`)
  check "workflow reads exact accepted terminal state"
    ("ReviewAccepted" `Text.isInfixOf` accepted && candidate `Text.isInfixOf` accepted)
  script owner "review-flow-workflow-interview"
  script owner "review-flow-workflow-interview-result"
  pendingInterview <- turn owner "inspectFull (show retainedInterviews)"
  check "delivered interview request is not treated as an answer"
    ("Nothing" `Text.isInfixOf` output pendingInterview)
  pendingClose <- readFile owner (checkSource "review-flow-workflow-close") >>= turn owner
  beforeAnswer <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowCleanupResult state))"
  check "workflow leaves cleanup unrequested before the answer"
    ("interview pending; cleanup not requested" `Text.isInfixOf` output pendingClose
      && "Nothing" `Text.isInfixOf` output beforeAnswer)
  interview <- activation
  check "interview request returns to the retained reviewer"
    (checkActor interview == checkActor reviewer)
  void $ turn (checkActor interview)
    "respond (\"Observed exact source and typed review; the handoff waited for owner snapshot; try one terminal evidence summary.\" :: Text)"
  script owner "review-flow-workflow-interview-result"
  answer <- turn owner "inspectFull (show retainedInterviews)"
  check "owner retains the reviewer answer receipt before cleanup"
    ("Observed exact source" `Text.isInfixOf` output answer)
  script owner "review-flow-workflow-close"
  closed <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowCleanupResult state))"
  check "accepted workflow retains typed owner cleanup after interview"
    ("ReviewAccepted" `Text.isInfixOf` output closed
      && "ReviewCleanupAttempted" `Text.isInfixOf` output closed)
  void $ turn owner "R.finish flow"

  void restart
  owner2 <- root
  source2 <- git owner2 ["rev-parse", "HEAD"]
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral source2
    <> "\nlet campaignName = \"review-workflow-escalated\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-workflow-escalated-coordinator\" :: Text"
    <> "\nlet routeCriteria = [] :: [Text]")
  script owner2 "review-flow-workflow"
  worker2 <- activation
  candidate2 <- checkpoint (checkActor worker2) "review-flow.txt" "escalated workflow\n" "workflow candidate"
  void $ turn (checkActor worker2)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate2 <> " [] []))")
  reviewer2 <- activation
  void $ turn (checkActor reviewer2)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) [\"change assigned acceptance\"]))"
  escalated <- awaitOutput owner2
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowRepairCount state, length (flowRepairRequests state)))"
    ("ReviewEscalated" `Text.isInfixOf`)
  check "workflow escalates without promoting Repair or requesting implementation"
    ("ReviewEscalated" `Text.isInfixOf` escalated && "0,0)" `Text.isInfixOf` escalated)
  script owner2 "review-flow-workflow-interview"
  interview2 <- activation
  check "escalated reviewer remains available for precleanup interview"
    (checkActor interview2 == checkActor reviewer2)
  void $ turn (checkActor interview2)
    "respond (\"Observed a scope conflict at exact source; owner decision is required; try clearer acceptance.\" :: Text)"
  script owner2 "review-flow-workflow-interview-result"
  script owner2 "review-flow-workflow-close"
  closed2 <- turn owner2 "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowCleanupResult state))"
  check "escalated workflow retains typed cleanup after interview"
    ("ReviewEscalated" `Text.isInfixOf` output closed2
      && "ReviewCleanupAttempted" `Text.isInfixOf` output closed2)
  void $ turn owner2 "R.finish flow"

-- The initial response may settle before any review actor subscribes.
lateCandidate :: Member RecipeCheck effects => Eff effects ()
lateCandidate = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-late-source\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-late-coordinator\" :: Text"
    <> "\nlet sourcePlan = ComponentReview")
  script owner "review-flow-late-setup"
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "late candidate\n" "settled before review subscription"
  void $ turn (checkActor worker) ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  script owner "review-flow-late-start"
  reviewer <- activation
  headSeen <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "late subscription preserves the original exact candidate" (headSeen == candidate)
  void $ turn (checkActor reviewer)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [] \"late exact source\")))"
  void $ awaitOutput owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowStage state)" (Text.isInfixOf "ReviewAccepted")
  retained <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (length (flowCandidateReceipts state) == 1 && length (flowReviewerReceipts state) == 1 && case flowReviewedProof state of { Just _ -> True; _ -> False })"
  check "late attachment retains original review proof and receipts" (lastOutput retained == "True")
  void $ turn owner "R.finish flow"

-- Original dynamic sources and checkpointed state survive record replacement.
replacement :: Member RecipeCheck effects => Eff effects ()
replacement = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-replacement\" :: CampaignLabel"
    <> "\nlet coordinatorName = \"review-replacement-coordinator\" :: Text"
    <> "\nlet sourcePlan = ComponentReview")
  script owner "review-flow-loop"
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "replacement candidate\n" "source before flow replacement"
  void $ turn (checkActor worker) ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  void $ turn owner "before <- R.call (reviewSnapshot (R.client flow)) ()\nflow <- R.replace flow flowDefinition"
  void $ turn (checkActor reviewer)
    "let question = Question \"after-replacement\" (DesignQuestion \"plans/component.md\" (reviewBase (reviewBasis sessionInput)) \"owner decision\" [] [] [])\nreportProgress (WorkProgress [] [question])"
  void $ awaitOutput owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (any (any ((== \"after-replacement\") . questionKey) . snd) (flowQuestions state))" (== "True")
  void $ turn (checkActor reviewer)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [] \"original request after replacement\")))"
  void $ awaitOutput owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (flowStage state)" (Text.isInfixOf "ReviewAccepted")
  retained <- turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (map requestId (flowReviewerRequests state) == map requestId (flowReviewerRequests before) && length (flowReviewerRequests state) == 1 && length (flowCandidateReceipts state) == 1 && length (flowAttachments state) == 1 && length (flowReviewerReceipts state) == 1 && case flowReviewedProof state of { Just _ -> True; _ -> False })"
  check "replacement retains original sources without starting a second review" (lastOutput retained == "True")
  void $ turn owner "R.finish flow"
