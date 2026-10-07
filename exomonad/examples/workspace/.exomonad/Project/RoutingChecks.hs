{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RoutingChecks (mixedRequests, refusedRoutes, observerIsolation, routing, handlerCall, messageDeltas, independentSources, twoLaneHandoff, notificationRetention, candidateHistory, reviewReadiness, reviewedCheckpoints, forwardingFailure) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script)

routing :: Member RecipeCheck effects => Eff effects ()
routing = do
  mixedRequests
  void restart
  refusedRoutes
  void restart
  observerIsolation
  void restart
  messageDeltas
  void restart
  forwardCandidate Forward
  void restart
  forwardCandidate CancelDestination
  void restart
  forwardCandidate LoseProducer
  void restart
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  script owner "progress-route-consumer"
  consumer <- activation
  script (checkActor consumer) "layout-reply"
  void $ turn owner "layoutReceipt <- pollResponse consumer"
  assertCell owner "an indented finding list reaches the actual reply" "(== (Just \"ready; layout preserved\")) (case layoutReceipt of { ResponseReady result -> Just (responseValue result); _ -> Nothing })"
  script owner "progress-route"
  script (checkActor producer) "progress-route-questions"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  awaitCell owner "the persistent actor consumes the first publication" "(== ([[\"question-a\"]])) . map (map questionKey . workQuestions . sourceProgress) . collectedWork <$> readWork forwarding"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  void $ turn owner "readWork forwarding"
  void $ turn owner "checkObserved <- (Actor.call wakes (RoutingCount 0 id))"
  assertCell owner "identical attention does not invoke the sink again" "checkObserved == 1"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,second])"
  awaitCell owner "later publications arrive without rearming" "(== ([[\"question-a\",\"question-b\"]])) . map (map questionKey . workQuestions . sourceProgress) . collectedWork <$> readWork forwarding"
  void $ turn (checkActor producer) "import Tidepool.Agent.Reply (pollReply, ReplyState (..))\noriginalReply <- pollReply sessionReply"
  assertCell (checkActor producer) "publishing progress preserves the original reply" "originalReply == ReplyOpen"
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  awaitCell owner "source closure leaves the actor's retained state queryable" "(== ([WorkClosed])) . map Exomonad.Contrib.Routing.sourceStatus . collectedWork <$> readWork forwarding"
  void $ turn owner "finishWork forwarding"
  void restart
  independentSources
  void restart
  twoLaneHandoff

-- The collector observes already launched requests and retains their exact
-- terminal receipts alongside independent progress sources.
mixedRequests :: Member RecipeCheck effects => Eff effects ()
mixedRequests = do
  owner <- root
  script owner "mixed-requests"
  textWorker <- activation
  numberWorker <- activation
  void $ turn (checkActor textWorker) "reportProgress True"
  void $ turn (checkActor numberWorker) "reportProgress (\"numbers ready\" :: Text)"
  void $ turn (checkActor textWorker) "respond (\"text ready\" :: Text)"
  void $ turn (checkActor numberWorker) "respond (42 :: Int)"
  void $ turn owner $ Text.unlines
    [ "view <- readWork collection"
    , "ResponseReady textReceipt <- pollResponse textRequest"
    , "ResponseReady numberReceipt <- pollResponse numberRequest"
    , "let textMatches = [responseExecution receipt == responseExecution textReceipt && responseWorktree receipt == responseWorktree textReceipt && responseValue receipt == TextResult (responseValue textReceipt) | WorkFinished \"text\" (Right receipt) <- workHistory view]"
    , "let numberMatches = [responseExecution receipt == responseExecution numberReceipt && responseWorktree receipt == responseWorktree numberReceipt && responseValue receipt == NumberResult (responseValue numberReceipt) | WorkFinished \"number\" (Right receipt) <- workHistory view]"
    , "let checkObserved = (textMatches == [True] && numberMatches == [True] && length [() | source <- collectedWork view, Just _ <- [sourceCursor source]] == 2)"
    ]
  assertCell owner "collector retains exact receipts and progress from both supplied requests" "checkObserved"
  void $ turn owner "finishWork collection"

refusedRoutes :: Member RecipeCheck effects => Eff effects ()
refusedRoutes = do
  owner <- root
  script owner "refused-routes"
  assertCell owner "invalid collector configuration returns typed domain errors without consuming requests" "(case emptyRefusal of { Left NoSources -> True; _ -> False }) && (case blankRefusal of { Left BlankSourceName -> True; _ -> False }) && (case duplicateRefusal of { Left (DuplicateSourceName \"same\") -> True; _ -> False }) && beforeRefusal == afterRefusal"

observerIsolation :: Member RecipeCheck effects => Eff effects ()
observerIsolation = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  void $ turn owner "refusedExisting <- followWork [(\"same\", producer, updates), (\"same\", producer, updates)] keepWork\noriginalState <- pollResponse producer"
  assertCell owner "collector refusal leaves original supplied request active" "((case refusedExisting of { Left (DuplicateSourceName \"same\") -> True; _ -> False }) && (case originalState of { ResponsePending _ -> True; _ -> False }))"
  script owner "observer-isolation"
  script (checkActor producer) "progress-route-questions"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  void $ turn owner "optionalExit <- Actor.awaitExit (R.actorRef optional)"
  assertCell owner "optional observer fails explicitly" "case optionalExit of { Actor.Failed _ -> True; _ -> False }"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,second])"
  void $ turn (checkActor producer) "respond (\"still collected\" :: Text)"
  void $ turn owner $ Text.unlines
    [ "view <- readWork collection"
    , "let refusals = [() | ObserverAdmission _ _ (Left _) <- workObserverAdmissions view]"
    , "let admissions = [() | ObserverAdmission _ _ (Right ()) <- workObserverAdmissions view]"
    , "let checkObserved = (length refusals >= 3 && length admissions == 1 && not (null (workNotices view)) && (case collectedWork view of { [source] -> case sourceResult source of { Just (Right receipt) -> responseValue receipt == \"still collected\"; _ -> False }; _ -> False }))"
    ]
  assertCell owner "refused and failed optional observers preserve primary terminal collection" "checkObserved"
  void $ turn owner "finishWork collection"

-- Real Delivery values cross a typed parent mailbox; partial source remains in
-- the local collector and both later final heads are integrated by the owner.
twoLaneHandoff :: Member RecipeCheck effects => Eff effects ()
twoLaneHandoff = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline)
  script owner "handoff-setup"
  left <- activation
  right <- activation
  script owner "handoff-router"
  partial <- checkpoint (checkActor left) "left.txt" "partial\n" "left partial checkpoint"
  void $ turn (checkActor left) ("reportProgress (WorkProgress [Candidate " <> gitOidLiteral partial <> " [] [\"final source pending\"]] [])")
  awaitCell owner "partial checkpoint is retained before final publication" ("do\n  view <- readWork handoff\n  pure (any (elem (Candidate " <> gitOidLiteral partial <> " [] [\"final source pending\"]) . workEvidence . sourceProgress) (collectedWork view))")
  awaitCell owner "partial progress stays local to the subtree" "do\n  received <- R.call (handoffSnapshot (R.client parent)) ()\n  pure (null received)\n"
  leftFinal <- checkpoint (checkActor left) "left.txt" "left final\n" "left final checkpoint"
  rightFinal <- checkpoint (checkActor right) "right.txt" "right final\n" "right final checkpoint"
  leftSource <- readFile (checkActor left) "left.txt"
  rightSource <- readFile (checkActor right) "right.txt"
  check "both final candidates have source evidence" (leftSource == "left final\n" && rightSource == "right final\n")
  let deliver actor commit = void $ turn actor
        ("respond (Produced (Delivered (ReviewedCandidate (AssignedTask sessionInput) (Candidate " <> gitOidLiteral commit
          <> " [\"read final source\"] [\"product acceptance remains\"]) [\"recipe source assertion\"] \"model-free handoff fixture\") "
          <> gitOidLiteral commit <> " [\"read final source\"]))")
  deliver (checkActor left) leftFinal
  deliver (checkActor right) rightFinal
  void $ turn owner "view <- readWork handoff\nreceived <- R.call (handoffSnapshot (R.client parent)) ()"
  assertCell owner "both later final heads arrive without rearming" ("all (\\oid -> any ((== oid) . candidateCommit) (concatMap (workEvidence . sourceProgress) (collectedWork view) ++ [reviewedCandidate reviewed | source <- collectedWork view, Just (Right receipt) <- [sourceResult source], Produced (Delivered reviewed _ _) <- [responseValue receipt]])) " <> "[" <> gitOidLiteral partial <> ", " <> gitOidLiteral leftFinal <> ", " <> gitOidLiteral rightFinal <> "]")
  assertCell owner "parent receives typed delivery and its runtime receipt" "let deliveries = [(reviewedCandidate reviewed, receipt) | WorkFinished _ (Right receipt) <- received, Produced (Delivered reviewed _ _) <- [responseValue receipt]] in length deliveries == 2 && all (\\(candidate,receipt) -> remainingGates candidate == [\"product acceptance remains\"] && responseWorktree receipt /= NoBoundWorktree) deliveries"
  assertCell owner "each final result crosses the parent boundary exactly once" "length received == 2"
  void $ git owner ["merge", "--ff-only", leftFinal]
  void $ git owner ["merge", "--no-edit", rightFinal]
  integratedLeft <- readFile owner "left.txt"
  integratedRight <- readFile owner "right.txt"
  check "coordinator integrates both exact final candidates" (integratedLeft == leftSource && integratedRight == rightSource)
  void $ git owner ["merge-base", "--is-ancestor", leftFinal, "HEAD"]
  void $ git owner ["merge-base", "--is-ancestor", rightFinal, "HEAD"]
  void $ turn owner "retired <- finishWork handoff"
  assertCell owner "drain retains both incorporated final heads" ("case retired of { Actor.Completed state -> length [() | source <- collectedWork state, Just (Right receipt) <- [sourceResult source], Produced (Delivered _ commit _) <- [responseValue receipt], commit `elem` " <> "[" <> gitOidLiteral leftFinal <> ", " <> gitOidLiteral rightFinal <> "]" <> "] == 2; _ -> False }")
  void $ turn owner "R.finish parent"

-- A record-actor handler that calls another record actor and waits for the
-- reply must be serviced while it waits: the integrator pattern in a project
-- gate rests on exactly this (a gate's handler calls the integrator, whose
-- one Call runs the whole merge and check).
handlerCall :: Member RecipeCheck effects => Eff effects ()
handlerCall = do
  owner <- root
  script owner "handler-call"
  awaitCell owner "a handler awaiting another record actor's reply is serviced" "do { state <- R.call (callerView (R.client caller)) (); pure (callerReplies state == [6]) }"
  awaitCell owner "the callee ran exactly once" "do { state <- R.call (boxView (R.client box)) (); pure (boxCalls state == 1) }"
  -- Every module in `modules` is imported unqualified into cells, so an
  -- exported constructor or field must not share a name with another
  -- module's. These are the names prompts and skills tell a model to write.
  assertCell owner "a cell names reply constructors and fields unqualified" "case (Blocked \"reason\" [] :: Outcome ReviewDecision) of { Blocked _ _ -> null (map repairFindings []); Produced (Accepted _) -> False; Produced (Repair _ _) -> False }"

forwardingFailure :: Member RecipeCheck effects => Eff effects ()
forwardingFailure = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  script owner "forwarding-failure"
  identity <- turn owner "forwarding"
  check "display UX: a forwarding handle displays identity without inspecting its result cell" ("Forwarding (" `Text.isPrefixOf` output identity)
  void $ turn (checkActor producer) "respond (\"retained result\" :: Text)"
  awaitCell owner "finite forwarding retains a failed exit instead of retargeting a stale endpoint" "do { exit <- R.forwardingExit forwarding; pure (case exit of { Just (Actor.Failed _) -> True; _ -> False }) }"
  awaitCell owner "replacement does not receive a send addressed to the old incarnation" "do { count <- R.call (acceptedCount (R.client sink)) (); pure (count == 0) }"
  awaitCell owner "the forwarder's failed exit remains inspectable after another query" "do { exit <- R.forwardingExit forwarding; pure (case exit of { Just (Actor.Failed _) -> True; _ -> False }) }"
  void $ turn owner "R.finish sink"

messageDeltas :: Member RecipeCheck effects => Eff effects ()
messageDeltas = do
  owner <- root
  script owner "question-message-deltas"
  assertCell owner "amendments upsert once; source advances, resolutions and unchanged questions stay distinct" "(== ((True,True,True,True))) questionMessageChecks"

candidateHistory :: Member RecipeCheck effects => Eff effects ()
candidateHistory = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner "Right collection <- followWork [(\"producer\", producer, updates)] keepWork"
  let candidates = "let firstCandidate = Candidate " <> gitOidLiteral baseline <> " [\"first check\"] []\nlet secondCandidate = firstCandidate { reportedChecks = [\"different check\"] }"
  void $ turn (checkActor producer) (candidates <> "\nreportProgress (WorkProgress [firstCandidate] [])\nreportProgress (WorkProgress [secondCandidate] [])")
  void $ turn owner "view <- readWork collection"
  assertCell owner "same-commit changed evidence remains distinct and ordered" "(== ((2,2))) (length (workEvidence (sourceProgress (head (collectedWork view)))), length (workHistory view))"
  void $ turn owner (candidates <> "\nR.send (acknowledgeWork (R.client collection)) (\"producer\", [firstCandidate])\nview <- readWork collection\nlet briefBefore = workSnapshotSummary id view")
  assertCell owner "incorporation removes only the exact handled evidence" "(== ([[\"different check\"]])) (map reportedChecks (outstandingEvidence view (head (collectedWork view))))"
  void $ turn (checkActor producer) "mapM_ reportProgress (replicate 100 (WorkProgress [firstCandidate,secondCandidate] []))"
  void $ turn owner "view <- readWork collection"
  assertCell owner "100 retained publications do not expand the normal brief or erase evidence" "(== ((True,102,[[\"first check\"],[\"different check\"]]))) (workSnapshotSummary id view == briefBefore, length (workHistory view), map reportedChecks (workEvidence (sourceProgress (head (collectedWork view)))))"
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  void $ turn owner "finishWork collection"

reviewReadiness :: Member RecipeCheck effects => Eff effects ()
reviewReadiness = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline)
  script owner "review-readiness"
  worker <- activation
  let actor = checkActor worker
  void $ turn actor ("reportProgress (WorkProgress [Candidate " <> gitOidLiteral baseline
    <> " [] [\"review pending\"]] [])")
  void $ turn owner "state <- readWork readiness"
  assertCell owner "a reported progress candidate cannot signal review readiness" "(0,Nothing) == (length (workNotices state), reviewReadyMessage (last (workHistory state)))"
  submitted <- checkpoint actor "review-ready.txt" "submitted source\n" "commit review source"
  void $ turn actor ("reportProgress (WorkProgress [Candidate " <> gitOidLiteral submitted
    <> " [] [\"integration remains\"]] [])")
  awaitCell owner "a committed progress checkpoint still has no terminal source receipt" "null . workNotices <$> readWork readiness"
  void $ turn actor ("respond (Produced (Candidate " <> gitOidLiteral submitted
    <> " [\"child-reported check\"] [\"integration remains\"]))")
  void $ turn owner "state <- readWork readiness\nlet terminal = last (workHistory state)"
  assertCell owner "matching terminal HEAD sends one exact source notice" ("length (workNotices state) == 1 && case Exomonad.Contrib.Routing.reviewReadiness terminal of { Just (ReviewReady _ candidate) -> candidateCommit candidate == " <> gitOidLiteral submitted <> " && remainingGates candidate == [\"integration remains\"]; _ -> False }")
  assertCell owner "text: source notice reports admission and gates without claiming child checks" "maybe False (\\message -> \"matching submitted HEAD\" `T.isInfixOf` message && \"integration remains\" `T.isInfixOf` message && not (\"child-reported check\" `T.isInfixOf` message)) (reviewReadyMessage terminal)"
  script owner "review-readiness-cases"
  assertCell owner "mismatched and unbound sources refuse readiness; blocked results remain quiet" "case cases of { (Just (ReviewSourceRejected _ _), Just (ReviewSourceRejected _ _), Nothing) -> True; _ -> False }"
  void $ turn owner "finishWork readiness"

reviewedCheckpoints :: Member RecipeCheck effects => Eff effects ()
reviewedCheckpoints = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline)
  void $ turn owner "import qualified Tidepool.Actor.Record as R\nimport qualified Data.Text as Text"
  void $ turn owner "let progress = WorkProgress [] []"
  assertCell owner "ordinary two-argument progress starts without reviewed evidence" "null (workEvidence progress) && null (workQuestions progress) && null (workReviewed progress)"
  script owner "reviewed-checkpoint-route"
  reviewerActor <- activation
  void $ turn owner "checkObserved <- (admitReviewedCheckpoint reviewRequest reviewer)"
  assertCell owner "an unsettled review handle cannot create a reviewed checkpoint" "case checkObserved of { Left (CheckpointNotReady) -> True; _ -> False }"
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [\"source read\"] \"accepted\")))"
  void $ turn owner "checkObserved <- (admitReviewedCheckpoint (reviewRequest { reviewBasis = ExactScope sourceHead [] \"different\" }) reviewer)"
  assertCell owner "a review for another basis is refused" "case checkObserved of { Left (CheckpointBasisMismatch _ _) -> True; _ -> False }"
  void $ turn owner "checkObserved <- (admitReviewedCheckpoint (reviewRequest { reviewInput = Candidate (GitOid \"different\") [] [] }) reviewer)"
  assertCell owner "a requested candidate without the review checkout HEAD is refused" "case checkObserved of { Left (CheckpointSourceRejected _) -> True; _ -> False }"
  void $ turn owner "Right (alteredReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) reviewRequest defaultRequestOptions"
  void activation
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) ((reviewInput sessionInput) { reportedChecks = [\"different\"] }) [] \"accepted\")))"
  void $ turn owner "checkObserved <- (admitReviewedCheckpoint reviewRequest alteredReview)"
  assertCell owner "a reviewer verdict for another full candidate is refused" "case checkObserved of { Left (CheckpointCandidateMismatch _ _) -> True; _ -> False }"
  void $ turn owner "Right (blockedReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) reviewRequest defaultRequestOptions"
  void activation
  void $ turn (checkActor reviewerActor)
    "respond (Blocked \"review blocked\" [\"missing source proof\"] :: Outcome ReviewDecision)"
  void $ turn owner "checkObserved <- (admitReviewedCheckpoint reviewRequest blockedReview)"
  assertCell owner "a blocked review cannot become a reviewed checkpoint" "case checkObserved of { Left (CheckpointBlocked _ _) -> True; _ -> False }"
  void $ turn owner "Right (repairReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) reviewRequest defaultRequestOptions"
  void activation
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Repair (reviewInput sessionInput) [\"repair requested\"]))"
  void $ turn owner "checkObserved <- (admitReviewedCheckpoint reviewRequest repairReview)"
  assertCell owner "a review requesting repair cannot become a reviewed checkpoint" "case checkObserved of { Left (CheckpointNeedsRepair _ _) -> True; _ -> False }"
  void $ turn owner "Right (dirtyReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) reviewRequest defaultRequestOptions"
  void activation
  writeFile (checkActor reviewerActor) "README.md" "dirty review checkout\n"
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [] \"accepted\")))"
  void $ turn owner "checkObserved <- (admitReviewedCheckpoint reviewRequest dirtyReview)"
  assertCell owner "a matching HEAD with uncommitted reviewer edits is refused" "case checkObserved of { Left (CheckpointSourceRejected _) -> True; _ -> False }"
  void $ git (checkActor reviewerActor) ["clean", "-f", "--", "README.md"]
  void $ turn owner
    "Right reviewed <- admitReviewedCheckpoint reviewRequest reviewer"
  void $ turn owner "let original = withReviewedCheckpoint reviewed (WorkProgress [] [])\nlet withQuestions = original { workQuestions = [Question \"update\" (DesignQuestion \"plans/component.md\" sourceHead \"owner choice\" [] [] [])] }\nlet withEvidence = original { workEvidence = [checkpointCandidate reviewed] }"
  assertCell owner "updating questions or evidence preserves the reviewed checkpoint" "workReviewed withQuestions == [reviewed] && workReviewed withEvidence == [reviewed]"
  assertCell owner "the checkpoint retains the original review response reference" "executionRequest (responseExecution (checkpointReceipt reviewed)) == requestId reviewer"
  void $ turn owner
    "Right producerAgent <- spawnSubagent (FreshCtx \"Publish a reviewed candidate and its questions.\") (ForkWorktree projectHead) ((defaultSpawnOptions workspaceAgentSpec) { spawnInstructions = Just (projectPrompt \"task\"), spawnLabel = Just \"producer\" })\nRight (producer, updates) <- requestWithProgress @WorkProgress @Text producerAgent reviewed (defaultRequestOptions { requestReporting = Silent })\nRight collection <- followWork [(\"producer\", producer, updates)] (notifyWork me (workMessage id))"
  producerActor <- activation
  void $ turn (checkActor producerActor)
    "reportProgress (WorkProgress [checkpointCandidate sessionInput] [])"
  void $ turn owner "view <- readWork collection"
  assertCell owner "ordinary candidate progress is retained and silent by default" "(length (workNotices view), length (workHistory view), length (outstandingReviewed view)) == (0,1,0)"
  void $ turn owner "policy <- setWorkNoticePolicy collection IncludeReviewed"
  assertCell owner "policy change records its own history cursor without replay" "policyEvent policy == Just 1"
  void $ turn owner "checkObserved <- (policyEvent <$> setWorkNoticePolicy collection IncludeReviewed)"
  assertCell owner "setting the same policy does not append another event" "checkObserved == Nothing"
  void $ turn (checkActor producerActor)
    "let newQuestion = Question \"decision\" (DesignQuestion \"plans/component.md\" (candidateCommit (checkpointCandidate sessionInput)) \"owner decision needed\" [] [] [])\nreportProgress (withReviewedCheckpoint sessionInput (WorkProgress [checkpointCandidate sessionInput] [newQuestion]))"
  void $ turn owner "view <- readWork collection"
  assertCell owner "validated review and simultaneous question share one retained send attempt" "case last (workHistory view) of { WorkChanged _ delta -> (length [() | Notice _ (Left NotificationUnavailable) <- workNotices view], length (outstandingReviewed view), length (addedReviewed delta), length (openedQuestions delta)) == (1,1,1,1); _ -> False }"
  void $ turn owner "view <- readWork collection\nlet notice = workNoticeMessage IncludeReviewed (workMessage id) (last (workHistory view))"
  assertCell owner "text: the notice retains the new question and remaining product gate" "maybe False (\\message -> \"owner decision needed\" `T.isInfixOf` message && \"browser gate remains\" `T.isInfixOf` message) notice"
  void $ turn (checkActor producerActor)
    "reportProgress (withReviewedCheckpoint sessionInput (WorkProgress [checkpointCandidate sessionInput] [newQuestion]))"
  void $ turn owner "checkObserved <- (length . workNotices <$> readWork collection)"
  assertCell owner "repeating one exact reviewed checkpoint does not wake again" "checkObserved == 1"
  void $ turn owner "changed <- setWorkNoticePolicy collection AllCheckpoints"
  assertCell owner "a later policy change has a distinct cursor" "policyEvent changed == Just 4"
  void $ turn (checkActor producerActor)
    "reportProgress (WorkProgress [(checkpointCandidate sessionInput) { reportedChecks = [\"new raw evidence\"] }] [newQuestion])"
  void $ turn owner "view <- readWork collection"
  assertCell owner "future-only policy keeps one history and source cursor" "(length [() | Notice _ (Left NotificationUnavailable) <- workNotices view], length (workHistory view), sourceCursor (head (collectedWork view)), length (outstandingReviewed view)) == (2,6,Just (ProgressCursor 4),1)"
  void $ turn owner
    "R.send (acknowledgeWork (R.client collection)) (\"producer\", [checkpointCandidate reviewed])"
  void $ turn owner "checkObserved <- (length . outstandingReviewed <$> readWork collection)"
  assertCell owner "explicit incorporation clears the reviewed snapshot" "checkObserved == 0"
  void $ turn (checkActor producerActor) "respond (\"done\" :: Text)"
  void $ turn owner "finishWork collection"

notificationRetention :: Member RecipeCheck effects => Eff effects ()
notificationRetention = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  script owner "progress-route-consumer"
  _ <- activation
  void $ turn owner "stopAgent (responseActor consumer)"
  void $ turn owner "import qualified Tidepool.Actor as Actor\nRight collection <- followWork [(\"producer\", producer, updates)] (notifyWork (responseActor consumer) (workMessage id))"
  script (checkActor producer) "progress-route-questions"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  awaitCell owner "a failed real send retains the new question and typed failure" "do\n  view <- readWork collection\n  pure (map (map questionKey . workQuestions . sourceProgress) (collectedWork view) == [[\"question-a\"]] && length [() | Notice _ (Left NotificationUnavailable) <- workNotices view] == 1)\n"
  assertCell (checkActor producer) "text: question messages contain only the new question" "maybe False (\\message -> \"question-b\" `T.isInfixOf` message && not (\"question-a\" `T.isInfixOf` message)) (workMessage (id :: Text -> Text) (workChange \"producer\" (ProgressCursor 1) (WorkProgress [] [first]) (WorkProgress [] [first,second])))"
  assertCell (checkActor producer) "text: a useful checkpoint and a new question both reach the owner" "maybe False (\\message -> \"partial-head\" `T.isInfixOf` message && \"question-b\" `T.isInfixOf` message && not (\"question-a\" `T.isInfixOf` message)) (withCheckpoints (workMessage (id :: Text -> Text)) (workChange \"producer\" (ProgressCursor 1) (WorkProgress [] [first]) (WorkProgress [Candidate (GitOid \"partial-head\") [] [\"review pending\"]] [first,second])))"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,first])"
  void $ turn owner "checkObserved <- (length . workNotices <$> readWork collection)"
  assertCell owner "repeated questions do not retry failed notification" "checkObserved == 1"
  void $ turn owner "Right replacementSpec <- pure (workDefinition [(\"producer\", producer, updates)] (notifyWork (responseActor consumer) (workMessage id)))\ncollection <- R.replace collection replacementSpec"
  awaitCell owner "replacement preserves failed notification evidence without replay" "(\\view -> (== ((1,[[\"question-a\"]]))) (length (workNotices view), map (map questionKey . workQuestions . sourceProgress) (collectedWork view))) <$> readWork collection"
  -- Exercise uncertainty as typed sink data. No external send is claimed here.
  script owner "uncertain-route"
  awaitCell owner "uncertain admission preserves the observed question" "do\n  view <- readWork uncertain\n  pure (map (map questionKey . workQuestions . sourceProgress) (collectedWork view) == [[\"question-a\"]] && length [() | Notice _ (Left (NotificationAdmissionUnconfirmed _)) <- workNotices view] == 1)\n"
  void $ turn owner "Right replacementSpec <- pure (workDefinition [(\"producer\", producer, updates)] keepWork)\nuncertain <- R.replace uncertain replacementSpec"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [])"
  awaitCell owner "resolving the final question produces its own delta" "(\\view -> (== (([[]],2))) (map (workQuestions . sourceProgress) (collectedWork view), length (workNotices view))) <$> readWork collection"
  void $ turn owner "checkObserved <- (length . workNotices <$> readWork uncertain)"
  assertCell owner "replacement never replays an uncertain send" "checkObserved == 1"
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  awaitCell owner "closure is quiet but the final result still arrives" "do\n  view <- readWork collection\n  pure (length (workNotices view) == 3 && case collectedWork view of { [source] -> case sourceResult source of { Just (Right receipt) -> responseValue receipt == \"finished\"; _ -> False }; _ -> False })\n"
  void $ turn owner "finishWork collection\nfinishWork uncertain"

-- Source publications and explicit queries use the same mailbox. A snapshot
-- call after publication is a barrier; the test never rearms a progress watch.
independentSources :: Member RecipeCheck effects => Eff effects ()
independentSources = do
  owner <- root
  script owner "attention-sources-setup"
  left <- activation
  right <- activation
  script owner "attention-sources-route"
  script (checkActor left) "attention-sources-question"
  script (checkActor right) "attention-sources-question"
  void $ turn (checkActor left) "reportProgress (WorkProgress [] [first,second])"
  awaitCell owner "left progresses while right is silent" "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"],WorkOpen),(\"right\",[],WorkOpen)])) [(sourceName s, map questionKey (workQuestions (sourceProgress s)), Exomonad.Contrib.Routing.sourceStatus s) | s <- collectedWork view]) <$> readWork collection"
  void $ turn owner "Right replacementSpec <- pure (workDefinition [(\"left\", left, leftProgress), (\"right\", right, rightProgress)] countChanges)\ncollection <- R.replace collection replacementSpec"
  void $ turn (checkActor left) "reportProgress (WorkProgress [] [second,first,first])"
  void $ turn owner "readWork collection"
  void $ turn owner "checkObserved <- (Actor.call wakes (RoutingCount 0 id))"
  assertCell owner "reordered duplicate facts do not invoke the sink" "checkObserved == 1"
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [first])"
  awaitCell owner "same-key questions retain both source identities" "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"]),(\"right\",[\"same-key\"])])) [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> readWork collection"
  void $ turn (checkActor left) "respond (\"finished\" :: Text)"
  awaitCell owner "closure retains unanswered questions" "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"],WorkClosed),(\"right\",[\"same-key\"],WorkOpen)])) [(sourceName s, map questionKey (workQuestions (sourceProgress s)), Exomonad.Contrib.Routing.sourceStatus s) | s <- collectedWork view]) <$> readWork collection"
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [])"
  awaitCell owner "one resolution cannot erase another source's questions" "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"]),(\"right\",[])])) [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> readWork collection"
  void $ turn (checkActor right) "respond (\"finished\" :: Text)"
  awaitCell owner "both sources close without rearming" "(== ([WorkClosed,WorkClosed])) . map Exomonad.Contrib.Routing.sourceStatus . collectedWork <$> readWork collection"
  void $ turn owner "finishWork collection"

data RouteCase = Forward | CancelDestination | LoseProducer deriving (Eq)

forwardCandidate :: Member RecipeCheck effects => RouteCase -> Eff effects ()
forwardCandidate disposition = do
  owner <- root
  let campaign = case disposition of
        Forward -> "route-forward"
        CancelDestination -> "route-cancel"
        LoseProducer -> "route-unavailable"
  void $ turn owner ("let routeCampaign = " <> literal campaign <> " :: CampaignLabel")
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline)
  script owner "route-reply-setup"
  lead <- activation
  shared <- checkpoint (checkActor lead) "contract.txt" "shared contract\n" "establish shared contract"
  void $ turn (checkActor lead) "let sharedReasoning = (\"consumer contract established\" :: Text)"
  script (checkActor lead) "route-reply-worker"
  worker <- activation
  assertCell (checkActor worker) "implementation inherits the parent's resident reasoning binding" "sharedReasoning == \"consumer contract established\""
  inheritedHead <- git (checkActor worker) ["rev-parse", "HEAD"]
  check "implementation uses current parent source despite older task provenance" (inheritedHead == shared)
  candidate <- checkpoint (checkActor worker) "feature.txt" "routed candidate\n" "prepare candidate"
  source <- readFile (checkActor worker) "feature.txt"
  check "the routed candidate has actual source evidence" (source == "routed candidate\n")
  if disposition == CancelDestination then void (turn owner "cancelRequest lead") else pure ()
  if disposition == LoseProducer then void (turn (checkActor lead) "stopAgent (responseActor candidate)")
  else void $ turn (checkActor worker) ("respond (Candidate " <> gitOidLiteral candidate <> " [\"read exact feature\"] [\"independent review remains\"])")
  if disposition == CancelDestination then do
    awaitCell (checkActor lead) "cancellation remains a retained callback failure" "do\n  state <- pollRoute forwarding\n  pure (case state of { RouteFailed _ -> True; _ -> False })\n"
    void $ turn (checkActor lead) "import Tidepool.Agent.Reply (pollReply, ReplyState (..))\nobligation <- pollReply sessionReply"
    assertCell (checkActor lead) "a cancelled destination cannot silently receive success" "case obligation of { ReplyCancellationRequested _ -> True; _ -> False }"
  else if disposition == LoseProducer then do
    awaitCell (checkActor lead) "lost execution remains explicit in the retained route" "do\n  state <- pollRoute forwarding\n  pure (case state of { RouteFailed _ -> True; _ -> False })\n"
    void $ turn (checkActor lead) "import Tidepool.Agent.Reply (pollReply, ReplyState (..))\nobligation <- pollReply sessionReply"
    assertCell (checkActor lead) "lost execution does not become a successful candidate" "obligation == ReplyOpen"
  else do
    void $ turn owner "answer <- await (settlement lead)"
    assertCell owner "routing forwards exact evidence without a lead relay turn"
      ("case answer of { Right (Right value) -> candidateCommit value == " <> gitOidLiteral candidate <> " && remainingGates value == [\"independent review remains\"]; _ -> False }")
