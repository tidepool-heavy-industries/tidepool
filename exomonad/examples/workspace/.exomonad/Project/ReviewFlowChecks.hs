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
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-loop-first\" :: Text"
    <> "\nlet coordinatorName = \"review-flow-coordinator-first\" :: Text"
    <> "\nlet sourcePlan = RequiresSiblingCommits [sourceHead]")
  script owner "review-flow-loop"
  void $ turn owner "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  assertCell owner "cleanup refuses while the candidate is pending"
    "case cleanup of { ReviewCleanupPending AwaitingCandidate -> True; _ -> False }"
  implementer <- activation
  first <- checkpoint (checkActor implementer) "review-flow.txt" "first candidate\n" "initial candidate"
  void $ turn (checkActor implementer)
    ("respond (Produced (Candidate " <> gitOidLiteral first <> " [] [\"owner integration\"]))")
  reviewer <- activation
  awaitCell owner "candidate settlement reached exact-source review"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewingCandidate candidate -> candidateCommit candidate == " <> gitOidLiteral first <> "; _ -> False }) }")
  headSeen <- git (checkActor reviewer) ["rev-parse", "HEAD"]
  check "first review starts fresh at the submitted candidate" (headSeen == first)
  void $ turn owner "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  assertCell owner "cleanup refuses with a live reviewer"
    "case cleanup of { ReviewCleanupPending (ReviewingCandidate _) -> True; _ -> False }"
  void $ turn (checkActor reviewer) "let cleanupInterview = \"initial reviewer found a component issue\" :: Text\ncleanupInterview"
  assertCell (checkActor reviewer) "text: first reviewer interview is available before cleanup"
    "cleanupInterview == \"initial reviewer found a component issue\""
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
  void $ turn owner "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  assertCell owner "cleanup refuses while repair is pending"
    "case cleanup of { ReviewCleanupPending (AwaitingRepair _) -> True; _ -> False }"
  revised <- checkpoint (checkActor repairWorker) "review-flow.txt" "repaired candidate\n" "repair candidate"
  void $ turn (checkActor repairWorker)
    ("respond (Produced (Candidate " <> gitOidLiteral revised <> " [] [\"owner integration\"]))")
  second <- activation
  secondHead <- git (checkActor second) ["rev-parse", "HEAD"]
  check "revised source gets a new exact-source reviewer"
    (checkActor second /= checkActor reviewer && secondHead == revised)
  void $ turn (checkActor second) "let cleanupInterview = \"reviewer checked the repaired source\" :: Text\ncleanupInterview"
  assertCell (checkActor second) "text: reviewer interview is retained before cleanup"
    "cleanupInterview == \"reviewer checked the repaired source\""
  void $ turn (checkActor second)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [\"read exact source\"] \"accepted\")))"
  awaitCell owner "one component retains exact reviewed evidence, correction, and one repair"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (flowRepairCount state == 1 && length (flowReviewerRequests state) == 3 && length (flowRepairRequests state) == 1 && case flowStage state of { ReviewAccepted reviewed -> candidateCommit (reviewedCandidate reviewed) == " <> gitOidLiteral revised <> "; _ -> False }) }")
  script owner "review-flow-cleanup"
  assertCell owner "coordinator records one typed cleanup history for each reviewer actor"
    "import Tidepool.Agent.Ref (agentIdentity)\ncase cleanup of { ReviewCleanupAttempted outcomes -> do { state <- R.call (reviewSnapshot (R.client flow)) (); let agents = map (agentIdentity . fst) outcomes; let known = map agentIdentity (flowReviewerAgents state); pure (length agents == length known && and [agent `elem` known | agent <- agents] && and [left /= right | (i,left) <- zip [0..] agents, right <- drop (i + 1) agents] && all (all (\\outcome -> case outcome of { StoppedNow -> True; StoppedRetaining _ -> True; StoppedReleasing -> True; AlreadyStopped -> True; StopUnavailable -> True; StopUnauthorized -> True; StopFailed _ -> True })) (map snd outcomes)) }; _ -> False }"
  void $ turn owner "again <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\nstate <- R.call (reviewSnapshot (R.client flow)) ()\npure ()"
  assertCell owner "repeat and snapshot retain per-agent cleanup histories"
    "case (cleanup, again, flowCleanupResult state) of { (ReviewCleanupAttempted original, ReviewCleanupAttempted repeated, Just (ReviewCleanupAttempted retained)) -> original == repeated && original == retained; _ -> False }"
  void $ turn owner "retry <- R.call (reviewCleanup (R.client flow)) ReviewCleanupRetryRefused"
  assertCell owner "explicit retry adds a stop outcome only for retryable agents"
    "import Tidepool.Agent.Ref (agentIdentity)\nlet retryable outcome = case outcome of { StoppedRetaining _ -> True; StoppedReleasing -> True; StopUnavailable -> True; StopFailed _ -> True; _ -> False }\ncase (cleanup, retry) of { (ReviewCleanupAttempted before, ReviewCleanupAttempted after) -> length before == length after && and [case lookup (agentIdentity agent) [(agentIdentity nextAgent, history) | (nextAgent, history) <- after] of { Just new -> length new == length old + (case reverse old of { latest:_ -> if retryable latest then 1 else 0; [] -> 0 }); Nothing -> False } | (agent, old) <- before]; _ -> False }"
  void $ turn owner "R.finish flow"

failurePaths :: Member RecipeCheck effects => Eff effects ()
failurePaths = do
  owner2 <- root
  base2 <- git owner2 ["rev-parse", "HEAD"]
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral base2
    <> "\nlet limit = 0 :: Int\nlet campaignName = \"review-loop-budget\" :: Text"
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
  awaitCell owner2 "zero repair budget stops without dispatching another request"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (flowRepairCount state == 0 && null (flowRepairRequests state) && case flowStage state of { ReviewStopped (RepairBudgetSpent 0 _ _) -> True; _ -> False }) }"
  void $ turn owner2 "receipt <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show receipt)"
  assertCell owner2 "stopped review retains its reviewer stop outcome"
    "case receipt of { ReviewCleanupAttempted outcomes -> length outcomes == 1 && case reverse (snd (head outcomes)) of { StoppedNow : _ -> True; StoppedRetaining _ : _ -> True; StoppedReleasing : _ -> True; AlreadyStopped : _ -> True; StopUnavailable : _ -> True; StopUnauthorized : _ -> True; StopFailed _ : _ -> True; [] -> False }; _ -> False }"
  void $ turn owner2 "R.finish flow"
  void restart
  owner3 <- root
  base3 <- git owner3 ["rev-parse", "HEAD"]
  void $ turn owner3 ("let sourceHead = " <> gitOidLiteral base3
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-loop-stale\" :: Text"
    <> "\nlet coordinatorName = \"review-flow-coordinator-stale\" :: Text"
    <> "\nlet sourcePlan = ComponentReview")
  script owner3 "review-flow-loop"
  mismatched <- activation
  actual <- checkpoint (checkActor mismatched) "review-flow.txt" "different source\n" "mismatch source"
  void $ turn (checkActor mismatched)
    ("respond (Produced (Candidate " <> gitOidLiteral base3 <> " [] []))")
  void $ turn owner3 "import qualified Tidepool.Worktree as WT"
  awaitCell owner3 "stale candidate stops before reviewer admission with exact receipt heads"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (null (flowReviewerRequests state) && case (flowStage state, flowCandidateReceipts state) of { (ReviewStopped (CandidateSourceRefused _), [Right receipt]) -> case responseValue receipt of { Produced candidate -> candidateCommit candidate == " <> gitOidLiteral base3 <> " && case responseWorktree receipt of { WorktreeObserved _ _ observation -> WT.headOid (WT.submittedHead observation) == " <> gitOidLiteral actual <> "; _ -> False }; _ -> False }; _ -> False }) }")
  void $ turn owner3 "cleanup <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show cleanup)"
  assertCell owner3 "terminal flow without a reviewer has no agent to stop"
    "case cleanup of { ReviewCleanupNoReviewer -> True; _ -> False }"
  void $ turn owner3 "R.finish flow"

sourcePreflight :: Member RecipeCheck effects => Eff effects ()
sourcePreflight = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  sibling <- checkpoint owner "sibling-source.txt" "required sibling\n" "required sibling source"
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 2 :: Int\nlet campaignName = \"review-loop-missing\" :: Text"
    <> "\nlet coordinatorName = \"review-flow-coordinator-missing\" :: Text"
    <> "\nlet sourcePlan = RequiresSiblingCommits [" <> gitOidLiteral sibling <> "]")
  script owner "review-flow-loop"
  worker <- activation
  partial <- checkpoint (checkActor worker) "review-flow.txt" "component only\n" "partial component"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral partial <> " [] []))")
  awaitCell owner "missing required sibling stops before product reviewer admission"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (null (flowReviewerRequests state) && case flowStage state of { ReviewStopped (RequiredSiblingMissing required candidate) -> required == " <> gitOidLiteral sibling <> " && candidate == " <> gitOidLiteral partial <> "; _ -> False }) }")
  void $ turn owner "R.finish flow"
  void restart
  owner2 <- root
  base2 <- git owner2 ["rev-parse", "HEAD"]
  sibling2 <- checkpoint owner2 "sibling-source.txt" "separate sibling\n" "separate sibling source"
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral base2
    <> "\nlet limit = 2 :: Int\nlet campaignName = \"review-loop-partial\" :: Text"
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
  awaitCell owner2 "component acceptance stays scoped to submitted candidate"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewAccepted reviewed -> candidateCommit (reviewedCandidate reviewed) == " <> gitOidLiteral partial2 <> "; _ -> False }) }")
  void $ turn owner2 "R.finish flow"

emptyFindings :: Member RecipeCheck effects => Eff effects ()
emptyFindings = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 2 :: Int\nlet campaignName = \"review-loop-empty\" :: Text"
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
  awaitCell owner "second empty Repair stops without implementer dispatch"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (flowRepairCount state == 0 && length (flowReviewerRequests state) == 2 && null (flowRepairRequests state) && case flowStage state of { ReviewStopped (EmptyRepairFindings candidate) -> candidateCommit candidate == " <> gitOidLiteral candidate <> "; _ -> False }) }")
  void $ turn owner "R.finish flow"

effectfulRouting :: Member RecipeCheck effects => Eff effects ()
effectfulRouting = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet campaignName = \"review-flow-effectful\" :: Text"
    <> "\nlet coordinatorName = \"review-flow-effectful-coordinator\" :: Text")
  script owner "review-flow-effectful"
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "routed candidate\n" "routed source"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  void $ turn (checkActor reviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Repair (reviewInput request) [\"repair changes the task contract\"]))"
  awaitCell owner "effectful route escalates valid exact-source Repair without dispatching repair"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case (flowStage state, flowReviewRoutes state) of { (ReviewStopped (ReviewEscalated reason), [(_, ReviewRouteResult (EscalateReview selected) DeterministicRoute)]) -> reason == \"owner must decide this scope change\" && selected == reason && flowRepairCount state == 0 && null (flowRepairRequests state); _ -> False }) }"
  void $ turn owner
    "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (length (flowNotices state))"
  assertCell owner "owner receives one escalation notice"
    "length (flowNotices state) == 1"
  void $ turn owner
    "let selected = Candidate sourceHead [] []\nresult <- semanticReviewChoice (ReviewContext task selected (Repair selected [\"scope is unclear\"]) 0 1 [])\ninspectFull (show result)"
  assertCell owner "semantic route without criteria returns typed parent escalation"
    "case result of { ReviewRouteResult (EscalateReview _) RouteCriteriaMissing -> True; _ -> False }"
  void $ turn owner
    "result <- semanticReviewChoice (ReviewContext task selected (Repair selected []) 0 1 [])\ninspectFull (show result)"
  assertCell owner "empty Repair stays on bounded same-reviewer correction path"
    "case result of { ReviewRouteResult HonorReview DeterministicRoute -> True; _ -> False }"
  void $ turn owner
    "receipt <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce\ninspectFull (show receipt)"
  assertCell owner "effectful escalation retains typed terminal stop outcomes"
    "case receipt of { ReviewCleanupAttempted outcomes -> not (null outcomes) && all (not . null . snd) outcomes; _ -> False }"
  void $ turn owner "R.finish flow"
  void restart
  owner2 <- root
  baseline2 <- git owner2 ["rev-parse", "HEAD"]
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral baseline2
    <> "\nlet limit = 1 :: Int\nlet sourcePlan = ComponentReview"
    <> "\nlet campaignName = \"review-flow-semantic\" :: Text"
    <> "\nlet coordinatorName = \"review-flow-semantic-coordinator\" :: Text")
  script owner2 "review-flow-semantic"
  worker2 <- activation
  candidate2 <- checkpoint (checkActor worker2) "review-flow.txt" "semantic consumer\n" "semantic source"
  void $ turn (checkActor worker2)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate2 <> " [] []))")
  reviewer2 <- activation
  void $ turn (checkActor reviewer2)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [] \"checked exact source\")))"
  awaitCell owner2 "semantic consumer preserves exact-source acceptance"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case (flowStage state, flowReviewRoutes state) of { (ReviewAccepted reviewed, [(_, ReviewRouteResult HonorReview DeterministicRoute)]) -> candidateCommit (reviewedCandidate reviewed) == " <> gitOidLiteral candidate2 <> "; _ -> False }) }")
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
    <> "\nlet campaignName = \"review-workflow-accepted\" :: Text"
    <> "\nlet coordinatorName = \"review-workflow-accepted-coordinator\" :: Text"
    <> "\nlet routeCriteria = [\"Escalate if acceptance or paths change.\"] :: [Text]")
  script owner "review-flow-workflow"
  void $ turn owner "inspectFull (show pendingCleanup)"
  assertCell owner "workflow refuses cleanup before candidate settles"
    "case pendingCleanup of { ReviewCleanupPending AwaitingCandidate -> True; _ -> False }"
  worker <- activation
  candidate <- checkpoint (checkActor worker) "review-flow.txt" "accepted workflow\n" "workflow candidate"
  void $ turn (checkActor worker)
    ("respond (Produced (Candidate " <> gitOidLiteral candidate <> " [] []))")
  reviewer <- activation
  void $ turn (checkActor reviewer)
    "let request = sessionInput :: ReviewRequest\nrespond (Produced (Accepted (ReviewedCandidate (reviewBasis request) (reviewInput request) [\"read exact source\"] \"accepted\")))"
  awaitCell owner "workflow reads exact accepted terminal state"
    ("do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewAccepted reviewed -> candidateCommit (reviewedCandidate reviewed) == " <> gitOidLiteral candidate <> "; _ -> False }) }")
  script owner "review-flow-workflow-interview"
  void $ turn owner "inspectFull (show interviewRequests)"
  assertCell owner "workflow retains each caller-owned interview request"
    "length interviewRequests == 1"
  script owner "review-flow-workflow-interview-result"
  void $ turn owner "inspectFull (show retainedInterviews)"
  assertCell owner "delivered interview request is not an answer"
    "case retainedInterviews of { Nothing -> True; _ -> False }"
  void $ readFile owner (checkSource "review-flow-workflow-close") >>= turn owner
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowCleanupResult state))"
  assertCell owner "workflow leaves cleanup unrequested before answer"
    "case (retainedInterviews, flowCleanupResult state) of { (Nothing, Nothing) -> True; _ -> False }"
  interview <- activation
  check "interview request returns to the retained reviewer"
    (checkActor interview == checkActor reviewer)
  void $ turn (checkActor interview)
    "respond (\"Observed exact source and typed review; the handoff waited for owner snapshot; try one terminal evidence summary.\" :: Text)"
  script owner "review-flow-workflow-interview-result"
  void $ turn owner "inspectFull (show retainedInterviews)"
  assertCell owner "text: owner retains reviewer answer receipt before cleanup"
    "case retainedInterviews of { Just [receipt] -> \"Observed exact source\" `Text.isInfixOf` responseValue receipt; _ -> False }"
  script owner "review-flow-workflow-close"
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowCleanupResult state))"
  assertCell owner "accepted workflow retains each reviewer stop outcome after interview"
    ("case (flowStage state, flowCleanupResult state) of { (ReviewAccepted reviewed, Just (ReviewCleanupAttempted outcomes)) -> candidateCommit (reviewedCandidate reviewed) == " <> gitOidLiteral candidate <> " && length outcomes == length (flowReviewerAgents state) && all (not . null . snd) outcomes; _ -> False }")
  void $ turn owner "R.finish flow"

  void restart
  owner2 <- root
  source2 <- git owner2 ["rev-parse", "HEAD"]
  void $ turn owner2 ("let sourceHead = " <> gitOidLiteral source2
    <> "\nlet campaignName = \"review-workflow-escalated\" :: Text"
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
  awaitCell owner2 "workflow escalates without promoting Repair or dispatching implementation"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (flowRepairCount state == 0 && null (flowRepairRequests state) && case flowStage state of { ReviewStopped (ReviewEscalated _) -> True; _ -> False }) }"
  script owner2 "review-flow-workflow-interview"
  void $ turn owner2 "inspectFull (show interviewRequests)"
  assertCell owner2 "escalated workflow retains original reviewer interview request"
    "length interviewRequests == 1"
  interview2 <- activation
  check "escalated reviewer remains available for precleanup interview"
    (checkActor interview2 == checkActor reviewer2)
  void $ turn (checkActor interview2)
    "respond (\"Observed a scope conflict at exact source; owner decision is required; try clearer acceptance.\" :: Text)"
  script owner2 "review-flow-workflow-interview-result"
  script owner2 "review-flow-workflow-close"
  void $ turn owner2 "state <- R.call (reviewSnapshot (R.client flow)) ()\ninspectFull (show (flowStage state, flowCleanupResult state))"
  assertCell owner2 "escalated workflow retains per-reviewer stop outcomes after interview"
    "case (flowStage state, flowCleanupResult state) of { (ReviewStopped (ReviewEscalated _), Just (ReviewCleanupAttempted outcomes)) -> length outcomes == length (flowReviewerAgents state) && all (not . null . snd) outcomes; _ -> False }"
  void $ turn owner2 "R.finish flow"

-- The initial response may settle before any review actor subscribes.
lateCandidate :: Member RecipeCheck effects => Eff effects ()
lateCandidate = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-late-source\" :: Text"
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
  awaitCell owner "review reaches accepted terminal state"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewAccepted _ -> True; _ -> False }) }"
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()"
  assertCell owner "late attachment retains original review proof and receipts"
    "length (flowCandidateReceipts state) == 1 && length (flowReviewerReceipts state) == 1 && case flowReviewedProof state of { Just _ -> True; _ -> False }"
  void $ turn owner "R.finish flow"

-- Original dynamic sources and checkpointed state survive record replacement.
replacement :: Member RecipeCheck effects => Eff effects ()
replacement = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline
    <> "\nlet limit = 1 :: Int\nlet campaignName = \"review-replacement\" :: Text"
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
  awaitCell owner "replacement retains original question source"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (any (any ((== \"after-replacement\") . questionKey) . snd) (flowQuestions state)) }"
  void $ turn (checkActor reviewer)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [] \"original request after replacement\")))"
  awaitCell owner "review reaches accepted terminal state"
    "do { state <- R.call (reviewSnapshot (R.client flow)) (); pure (case flowStage state of { ReviewAccepted _ -> True; _ -> False }) }"
  void $ turn owner "state <- R.call (reviewSnapshot (R.client flow)) ()"
  assertCell owner "replacement retains original sources without a second review"
    "map requestId (flowReviewerRequests state) == map requestId (flowReviewerRequests before) && length (flowReviewerRequests state) == 1 && length (flowCandidateReceipts state) == 1 && length (flowAttachments state) == 1 && length (flowReviewerReceipts state) == 1 && case flowReviewedProof state of { Just _ -> True; _ -> False }"
  void $ turn owner "R.finish flow"
