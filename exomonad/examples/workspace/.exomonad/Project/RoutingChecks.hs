{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RoutingChecks (mixedBatch, refusedBatch, observerIsolation, routing, handlerCall, messageDeltas, independentSources, twoLaneHandoff, notificationRetention, candidateHistory, reviewReadiness, reviewedCheckpoints, forwardingFailure) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script, checkSource)

routing :: Member RecipeCheck effects => Eff effects ()
routing = do
  mixedBatch
  void restart
  refusedBatch
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
  layoutResult <- turn owner "(== (Just \"ready; layout preserved\")) (case layoutReceipt of { ResponseReady result -> Just (responseValue result); _ -> Nothing })"
  check "an indented finding list reaches the actual reply" (output layoutResult == "True")
  script owner "progress-route"
  script (checkActor producer) "progress-route-questions"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  first <- turn owner "(== ([[\"question-a\"]])) . map (map questionKey . workQuestions . sourceProgress) . collectedWork <$> readWork forwarding"
  check "the persistent actor consumes the first publication" (output first == "True")
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  void $ turn owner "readWork forwarding"
  counts <- turn owner "Actor.call wakes (RoutingCount 0 id)"
  check "identical attention does not invoke the sink again" (output counts == "1")
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,second])"
  second <- turn owner "(== ([[\"question-a\",\"question-b\"]])) . map (map questionKey . workQuestions . sourceProgress) . collectedWork <$> readWork forwarding"
  check "later publications arrive without rearming" (output second == "True")
  pending <- turn (checkActor producer) "import Tidepool.Agent.Reply (pollReply)\npollReply sessionReply"
  check "publishing progress preserves the original reply" ("ReplyOpen" `Text.isSuffixOf` output pending)
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  closed <- turn owner "(== ([WorkClosed])) . map Exomonad.Contrib.Routing.sourceStatus . collectedWork <$> readWork forwarding"
  check "source closure leaves the actor's retained state queryable" (output closed == "True")
  void $ turn owner "finishWork forwarding"
  void restart
  independentSources
  void restart
  twoLaneHandoff

-- The shared collector receives projected values while the product retains
-- independently typed response/progress handles and exact original receipts.
mixedBatch :: Member RecipeCheck effects => Eff effects ()
mixedBatch = do
  owner <- root
  script owner "mixed-batch"
  textWorker <- activation
  numberWorker <- activation
  void $ turn (checkActor textWorker) "reportProgress True"
  void $ turn (checkActor numberWorker) "reportProgress (\"numbers ready\" :: Text)"
  void $ turn (checkActor textWorker) "respond (\"text ready\" :: Text)"
  void $ turn (checkActor numberWorker) "respond (42 :: Int)"
  projected <- turn owner $ Text.unlines
    [ "view <- readWork (routedCollector mixed)"
    , "ResponseReady textReceipt <- pollResponse textResponse"
    , "ResponseReady numberReceipt <- pollResponse numberResponse"
    , "let sameReceipt original projected = responseExecution original == responseExecution projected && responseWorktree original == responseWorktree projected"
    , "let textMatches = [sameReceipt textReceipt receipt && responseValue receipt == TextResult (responseValue textReceipt) | WorkFinished \"text\" (Right receipt) <- workHistory view]"
    , "let numberMatches = [sameReceipt numberReceipt receipt && responseValue receipt == NumberResult (responseValue numberReceipt) | WorkFinished \"number\" (Right receipt) <- workHistory view]"
    , "inspectFull (textMatches == [True] && numberMatches == [True] && length [() | source <- collectedWork view, Just _ <- [sourceCursor source]] == 2)"
    ]
  check "mixed batch keeps original handles and exact receipts across projections" (lastOutput projected == "True")
  void $ turn owner "finishRoutedBatch mixed"

refusedBatch :: Member RecipeCheck effects => Eff effects ()
refusedBatch = do
  owner <- root
  result <- readFile owner (checkSource "refused-batch") >>= turn owner
  check "invalid batches return typed refusals before allocating any actor" (lastOutput result == "True")

observerIsolation :: Member RecipeCheck effects => Eff effects ()
observerIsolation = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  refused <- turn owner "refusedExisting <- followWork [(\"same\", producer, updates), (\"same\", producer, updates)] keepWork\noriginalState <- pollResponse producer\ninspectFull ((case refusedExisting of { Left (DuplicateWorkName \"same\") -> True; _ -> False }) && (case originalState of { ResponsePending _ -> True; _ -> False }))"
  check "collector refusal leaves original supplied response active" (lastOutput refused == "True")
  script owner "observer-isolation"
  script (checkActor producer) "progress-route-questions"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  _ <- awaitOutput owner "Actor.pollExit (R.actorRef optional)" (Text.isInfixOf "Failed")
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,second])"
  void $ turn (checkActor producer) "respond (\"still collected\" :: Text)"
  retained <- turn owner $ Text.unlines
    [ "view <- readWork collection"
    , "let refusals = [() | ObserverAdmission _ _ (Left _) <- workObserverAdmissions view]"
    , "let admissions = [() | ObserverAdmission _ _ (Right ()) <- workObserverAdmissions view]"
    , "inspectFull (length refusals >= 3 && length admissions == 1 && not (null (workNotices view)) && (case collectedWork view of { [source] -> case sourceResult source of { Just (Right receipt) -> responseValue receipt == \"still collected\"; _ -> False }; _ -> False }))"
    ]
  check "refused and failed optional observers preserve primary terminal collection" (lastOutput retained == "True")
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
  void $ turn owner "(right, rightProgress) <- unfoldDeferred (taskGroup task) (childWithProgress @WorkProgress @Delivery (withLifetime ActorOwned $ coding projectHead (assignment rightLabel task)))"
  right <- activation
  script owner "handoff-router"
  partial <- checkpoint (checkActor left) "left.txt" "partial\n" "left partial checkpoint"
  void $ turn (checkActor left) ("reportProgress (WorkProgress [Candidate " <> gitOidLiteral partial <> " [] [\"final source pending\"]] [])")
  initial <- turn owner "view <- readWork handoff\ninspectFull (collectedWork view)"
  check "partial checkpoint is retained before final publication" (partial `Text.isInfixOf` output initial)
  before <- turn owner "length <$> R.call (handoffSnapshot (R.client parent)) ()"
  check "partial progress stays local to the subtree" (lastOutput before == "0")
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
  final <- turn owner "view <- readWork handoff\ninspectFull (collectedWork view)"
  check "both later final heads arrive without rearming" (all (`Text.isInfixOf` output final) [partial, leftFinal, rightFinal])
  parentView <- turn owner "received <- R.call (handoffSnapshot (R.client parent)) ()\ninspectFull received"
  check "parent receives typed delivery and its runtime receipt" (all (`Text.isInfixOf` output parentView) [leftFinal, rightFinal, "product acceptance remains", "executionActorId"])
  count <- turn owner "inspectFull (length received)"
  check "each final result crosses the parent boundary exactly once" (output count == "2")
  void $ git owner ["merge", "--ff-only", leftFinal]
  void $ git owner ["merge", "--no-edit", rightFinal]
  integratedLeft <- readFile owner "left.txt"
  integratedRight <- readFile owner "right.txt"
  check "coordinator integrates both exact final candidates" (integratedLeft == leftSource && integratedRight == rightSource)
  void $ git owner ["merge-base", "--is-ancestor", leftFinal, "HEAD"]
  void $ git owner ["merge-base", "--is-ancestor", rightFinal, "HEAD"]
  retired <- turn owner "retired <- finishWork handoff\ninspectFull (case retired of { Actor.Completed state -> collectedWork state; _ -> [] })"
  check "drain retains both incorporated final heads" (all (`Text.isInfixOf` output retired) [leftFinal, rightFinal])
  void $ turn owner "R.finish parent"

-- A record-actor handler that calls another record actor and waits for the
-- reply must be serviced while it waits: the integrator pattern in a project
-- gate rests on exactly this (a gate's handler calls the integrator, whose
-- one Call runs the whole merge and check).
handlerCall :: Member RecipeCheck effects => Eff effects ()
handlerCall = do
  owner <- root
  script owner "handler-call"
  replied <- awaitOutput owner "state <- R.call (callerView (R.client caller)) ()\ninspectFull (callerReplies state)" (Text.isInfixOf "6")
  check ("a handler awaiting another record actor's reply is serviced: " <> replied) ("[6]" `Text.isInfixOf` replied)
  calls <- turn owner "state <- R.call (boxView (R.client box)) ()\ninspectFull (boxCalls state)"
  check "the callee ran exactly once" (lastOutput calls == "1")
  -- Every module in `modules` is imported unqualified into cells, so an
  -- exported constructor or field must not share a name with another
  -- module's. These are the names prompts and skills tell a model to write.
  outcome <- turn owner "case (Blocked \"reason\" [] :: Outcome ReviewDecision) of { Blocked _ _ -> null (map repairFindings []); Produced (Accepted _) -> False; Produced (Repair _ _) -> False }"
  check ("a cell names reply constructors and fields unqualified: " <> output outcome) (output outcome == "True")

forwardingFailure :: Member RecipeCheck effects => Eff effects ()
forwardingFailure = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  script owner "forwarding-failure"
  identity <- turn owner "forwarding"
  check "a forwarding handle displays identity without inspecting its result cell" ("Forwarding (" `Text.isPrefixOf` output identity)
  void $ turn (checkActor producer) "respond (\"retained result\" :: Text)"
  failed <- awaitOutput owner "R.forwardingExit forwarding" (Text.isInfixOf "Failed")
  check "finite forwarding retains a failed exit instead of retargeting a stale endpoint" ("Failed" `Text.isInfixOf` failed)
  count <- turn owner "R.call (acceptedCount (R.client sink)) ()"
  check "replacement does not receive a send addressed to the old incarnation" (output count == "0")
  retained <- turn owner "R.forwardingExit forwarding"
  check "the forwarder's failed exit remains inspectable after another query" ("Failed" `Text.isInfixOf` lastOutput retained)
  void $ turn owner "R.finish sink"

messageDeltas :: Member RecipeCheck effects => Eff effects ()
messageDeltas = do
  owner <- root
  script owner "question-message-deltas"
  result <- turn owner "(== ((True,True,True,True))) questionMessageChecks"
  check "amendments upsert once; source advances, resolutions and unchanged questions stay distinct"
    (output result == "True")

candidateHistory :: Member RecipeCheck effects => Eff effects ()
candidateHistory = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner "Right collection <- followWork [(\"producer\", producer, updates)] keepWork"
  let candidates = "let firstCandidate = Candidate " <> gitOidLiteral baseline <> " [\"first check\"] []\nlet secondCandidate = firstCandidate { reportedChecks = [\"different check\"] }"
  void $ turn (checkActor producer) (candidates <> "\nreportProgress (WorkProgress [firstCandidate] [])\nreportProgress (WorkProgress [secondCandidate] [])")
  before <- turn owner "view <- readWork collection\n(== ((2,2))) (length (workEvidence (sourceProgress (head (collectedWork view)))), length (workHistory view))"
  check "same-commit changed evidence remains distinct and ordered" (lastOutput before == "True")
  void $ turn owner (candidates <> "\nR.send (acknowledgeWork (R.client collection)) (\"producer\", [firstCandidate])\nview <- readWork collection\nlet briefBefore = workSnapshotSummary id view")
  frontier <- turn owner "(== ([[\"different check\"]])) (map reportedChecks (outstandingEvidence view (head (collectedWork view))))"
  check "incorporation removes only the exact handled evidence" (output frontier == "True")
  void $ turn (checkActor producer) "mapM_ reportProgress (replicate 100 (WorkProgress [firstCandidate,secondCandidate] []))"
  after <- turn owner "view <- readWork collection\n(== ((True,102,[[\"first check\"],[\"different check\"]]))) (workSnapshotSummary id view == briefBefore, length (workHistory view), map reportedChecks (workEvidence (sourceProgress (head (collectedWork view)))))"
  check "100 retained publications do not expand the normal brief or erase evidence"
    (lastOutput after == "True")
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
  premature <- turn owner "state <- readWork readiness\n(0,Nothing) == (length (workNotices state), reviewReadyMessage (last (workHistory state)))"
  check "a reported progress candidate cannot signal review readiness" (lastOutput premature == "True")
  submitted <- checkpoint actor "review-ready.txt" "submitted source\n" "commit review source"
  void $ turn actor ("reportProgress (WorkProgress [Candidate " <> gitOidLiteral submitted
    <> " [] [\"integration remains\"]] [])")
  checkpointOnly <- turn owner "null . workNotices <$> readWork readiness"
  check "a committed progress checkpoint still has no terminal source receipt" (lastOutput checkpointOnly == "True")
  void $ turn actor ("respond (Produced (Candidate " <> gitOidLiteral submitted
    <> " [\"child-reported check\"] [\"integration remains\"]))")
  notice <- turn owner "state <- readWork readiness\nlet terminal = last (workHistory state)\ninspectFull (length (workNotices state), reviewReadyMessage terminal)"
  let messageSent = and
        [ "(1," `Text.isInfixOf` output notice
        , "Just \"" `Text.isInfixOf` output notice
        , submitted `Text.isInfixOf` output notice
        , "matching submitted HEAD" `Text.isInfixOf` output notice
        , "integration remains" `Text.isInfixOf` output notice
        , not ("child-reported check" `Text.isInfixOf` output notice)
        ]
  check (if messageSent then "matching terminal HEAD sends one exact source notice without claiming review or checks"
         else "unexpected terminal notice: " <> output notice) messageSent
  casesSource <- readFile owner (checkSource "review-readiness-cases")
  cases <- turn owner casesSource
  check "mismatched and unbound sources refuse readiness; blocked results remain quiet"
    (baseline `Text.isInfixOf` output cases
      && submitted `Text.isInfixOf` output cases
      && "no bound-source evidence" `Text.isInfixOf` output cases
      && ",Nothing)" `Text.isInfixOf` output cases)
  void $ turn owner "finishWork readiness"

reviewedCheckpoints :: Member RecipeCheck effects => Eff effects ()
reviewedCheckpoints = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> gitOidLiteral baseline)
  void $ turn owner "import qualified Tidepool.Actor.Record as R\nimport qualified Data.Text as Text"
  ordinary <- turn owner
    "let progress = WorkProgress [] []\nnull (workEvidence progress) && null (workQuestions progress) && null (workReviewed progress)"
  check "ordinary two-argument progress starts without reviewed evidence"
    (lastOutput ordinary == "True")
  script owner "reviewed-checkpoint-route"
  reviewerActor <- activation
  pending <- turn owner "admitReviewedCheckpoint reviewRequest reviewer"
  check "an unsettled review handle cannot create a reviewed checkpoint"
    ("CheckpointNotReady" `Text.isInfixOf` output pending)
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [\"source read\"] \"accepted\")))"
  mismatchedBasis <- turn owner
    "admitReviewedCheckpoint (reviewRequest { reviewBasis = ExactScope sourceHead [] \"different\" }) reviewer"
  check "a review for another basis is refused"
    ("CheckpointBasisMismatch" `Text.isInfixOf` output mismatchedBasis)
  mismatchedSource <- turn owner
    "admitReviewedCheckpoint (reviewRequest { reviewInput = Candidate (GitOid \"different\") [] [] }) reviewer"
  check "a requested candidate without the review checkout HEAD is refused"
    ("CheckpointSourceRejected" `Text.isInfixOf` output mismatchedSource)
  alteredReviewCreated <- turn owner
    "(alteredReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) (assignment [label|altered-review|] reviewRequest)\nalteredReviewRetention <- detachRequest alteredReview\ninspectFull (show alteredReviewRetention)"
  check ("alteredReview retention: " <> lastOutput alteredReviewCreated)
    ("Right ()" `Text.isInfixOf` lastOutput alteredReviewCreated)
  void activation
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) ((reviewInput sessionInput) { reportedChecks = [\"different\"] }) [] \"accepted\")))"
  mismatchedCandidate <- turn owner "admitReviewedCheckpoint reviewRequest alteredReview"
  check "a reviewer verdict for another full candidate is refused"
    ("CheckpointCandidateMismatch" `Text.isInfixOf` output mismatchedCandidate)
  blockedReviewCreated <- turn owner
    "(blockedReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) (assignment [label|blocked-review|] reviewRequest)\nblockedReviewRetention <- detachRequest blockedReview\ninspectFull (show blockedReviewRetention)"
  check ("blockedReview retention: " <> lastOutput blockedReviewCreated)
    ("Right ()" `Text.isInfixOf` lastOutput blockedReviewCreated)
  void activation
  void $ turn (checkActor reviewerActor)
    "respond (Blocked \"review blocked\" [\"missing source proof\"] :: Outcome ReviewDecision)"
  blocked <- turn owner "admitReviewedCheckpoint reviewRequest blockedReview"
  check "a blocked review cannot become a reviewed checkpoint"
    ("CheckpointBlocked" `Text.isInfixOf` output blocked)
  repairReviewCreated <- turn owner
    "(repairReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) (assignment [label|repair-review|] reviewRequest)\nrepairReviewRetention <- detachRequest repairReview\ninspectFull (show repairReviewRetention)"
  check ("repairReview retention: " <> lastOutput repairReviewCreated)
    ("Right ()" `Text.isInfixOf` lastOutput repairReviewCreated)
  void activation
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Repair (reviewInput sessionInput) [\"repair requested\"]))"
  needsRepair <- turn owner "admitReviewedCheckpoint reviewRequest repairReview"
  check "a review requesting repair cannot become a reviewed checkpoint"
    ("CheckpointNeedsRepair" `Text.isInfixOf` output needsRepair)
  dirtyReviewCreated <- turn owner
    "(dirtyReview, _) <- requestWithProgress @WorkProgress @(Outcome ReviewDecision) (responseActor reviewer) (assignment [label|dirty-review|] reviewRequest)\ndirtyReviewRetention <- detachRequest dirtyReview\ninspectFull (show dirtyReviewRetention)"
  check ("dirtyReview retention: " <> lastOutput dirtyReviewCreated)
    ("Right ()" `Text.isInfixOf` lastOutput dirtyReviewCreated)
  void activation
  writeFile (checkActor reviewerActor) "README.md" "dirty review checkout\n"
  void $ turn (checkActor reviewerActor)
    "respond (Produced (Accepted (ReviewedCandidate (reviewBasis sessionInput) (reviewInput sessionInput) [] \"accepted\")))"
  dirty <- turn owner "admitReviewedCheckpoint reviewRequest dirtyReview"
  check "a matching HEAD with uncommitted reviewer edits is refused"
    ("CheckpointSourceRejected" `Text.isInfixOf` output dirty)
  void $ git (checkActor reviewerActor) ["clean", "-f", "--", "README.md"]
  void $ turn owner
    "Right reviewed <- admitReviewedCheckpoint reviewRequest reviewer"
  updates <- turn owner
    "let original = withReviewedCheckpoint reviewed (WorkProgress [] [])\nlet withQuestions = original { workQuestions = [Question \"update\" (DesignQuestion \"plans/component.md\" sourceHead \"owner choice\" [] [] [])] }\nlet withEvidence = original { workEvidence = [checkpointCandidate reviewed] }\nworkReviewed withQuestions == [reviewed] && workReviewed withEvidence == [reviewed]"
  check "updating questions or evidence preserves the reviewed checkpoint"
    (lastOutput updates == "True")
  receipt <- turn owner
    "executionRequest (responseExecution (checkpointReceipt reviewed)) == requestId reviewer"
  check "the checkpoint retains the original review response reference"
    (output receipt == "True")
  void $ turn owner
    "(producer, updates) <- unfoldDeferred (batch campaign \"produce\") (childWithProgress @WorkProgress @Text (withLifetime ActorOwned $ coding projectHead (assignment [label|producer|] reviewed)))\nRight collection <- followWork [(\"producer\", producer, updates)] (notifyWork me (workMessage id))"
  producerActor <- activation
  void $ turn (checkActor producerActor)
    "reportProgress (WorkProgress [checkpointCandidate sessionInput] [])"
  silent <- turn owner
    "view <- readWork collection\n(length (workNotices view), length (workHistory view), length (outstandingReviewed view)) == (0,1,0)"
  check "ordinary candidate progress is retained and silent by default"
    (lastOutput silent == "True")
  policy <- turn owner "setWorkNoticePolicy collection IncludeReviewed"
  check "policy change records its own history cursor without replay"
    ("Just 1" `Text.isInfixOf` output policy)
  unchanged <- turn owner "policyEvent <$> setWorkNoticePolicy collection IncludeReviewed"
  check "setting the same policy does not append another event"
    (output unchanged == "Nothing")
  void $ turn (checkActor producerActor)
    "let newQuestion = Question \"decision\" (DesignQuestion \"plans/component.md\" (candidateCommit (checkpointCandidate sessionInput)) \"owner decision needed\" [] [] [])\nreportProgress (withReviewedCheckpoint sessionInput (WorkProgress [checkpointCandidate sessionInput] [newQuestion]))"
  reviewed <- turn owner
    "view <- readWork collection\nlet event = last (workHistory view)\nlet delta = case event of WorkChanged _ change -> change; _ -> error \"missing delta\"\n(length [() | Notice _ (Left NotificationUnavailable) <- workNotices view], length (outstandingReviewed view), length (addedReviewed delta), length (openedQuestions delta))"
  check ("validated review and simultaneous question share one retained send attempt: " <> lastOutput reviewed)
    (lastOutput reviewed == "(1,1,1,1)")
  content <- turn owner
    "view <- readWork collection\nlet notice = workNoticeMessage IncludeReviewed (workMessage id) (last (workHistory view))\nmaybe False (\\message -> \"owner decision needed\" `Text.isInfixOf` message && \"browser gate remains\" `Text.isInfixOf` message) notice"
  check "the notice retains the new question and remaining product gate"
    (lastOutput content == "True")
  void $ turn (checkActor producerActor)
    "reportProgress (withReviewedCheckpoint sessionInput (WorkProgress [checkpointCandidate sessionInput] [newQuestion]))"
  duplicate <- turn owner "length . workNotices <$> readWork collection"
  check "repeating one exact reviewed checkpoint does not wake again"
    (output duplicate == "1")
  changed <- turn owner "setWorkNoticePolicy collection AllCheckpoints"
  check "a later policy change has a distinct cursor"
    ("Just 4" `Text.isInfixOf` output changed)
  void $ turn (checkActor producerActor)
    "reportProgress (WorkProgress [(checkpointCandidate sessionInput) { reportedChecks = [\"new raw evidence\"] }] [newQuestion])"
  future <- turn owner
    "view <- readWork collection\n(length [() | Notice _ (Left NotificationUnavailable) <- workNotices view], length (workHistory view), sourceCursor (head (collectedWork view)), length (outstandingReviewed view)) == (2,6,Just (ProgressCursor 4),1)"
  check "future-only policy keeps one history and source cursor"
    (lastOutput future == "True")
  void $ turn owner
    "R.send (acknowledgeWork (R.client collection)) (\"producer\", [checkpointCandidate reviewed])"
  handled <- turn owner "length . outstandingReviewed <$> readWork collection"
  check "explicit incorporation clears the reviewed snapshot"
    (output handled == "0")
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
  retained <- turn owner "view <- readWork collection\ninspectFull (collectedWork view, [failure | Notice _ (Left failure) <- workNotices view])"
  check "a failed real send retains the new question and typed failure" ("question-a" `Text.isInfixOf` output retained && "NotificationUnavailable" `Text.isInfixOf` output retained)
  compact <- turn (checkActor producer) "inspectFull (workMessage (id :: Text -> Text) (workChange \"producer\" (ProgressCursor 1) (WorkProgress [] [first]) (WorkProgress [] [first,second])))"
  check "question messages contain only the new question" ("question-b" `Text.isInfixOf` output compact && not ("question-a" `Text.isInfixOf` output compact))
  combined <- turn (checkActor producer) "inspectFull (withCheckpoints (workMessage (id :: Text -> Text)) (workChange \"producer\" (ProgressCursor 1) (WorkProgress [] [first]) (WorkProgress [Candidate (GitOid \"partial-head\") [] [\"review pending\"]] [first,second])))"
  check "a useful checkpoint and a new question both reach the owner" ("partial-head" `Text.isInfixOf` output combined && "question-b" `Text.isInfixOf` output combined && not ("question-a" `Text.isInfixOf` output combined))
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,first])"
  once <- turn owner "inspectFull . length . workNotices <$> readWork collection"
  check "repeated questions do not retry failed notification" (output once == "1")
  void $ turn owner "Right replacementSpec <- pure (workDefinition [(\"producer\", producer, updates)] (notifyWork (responseActor consumer) (workMessage id)))\ncollection <- R.replace collection replacementSpec"
  preserved <- turn owner "(\\view -> (== ((1,[[\"question-a\"]]))) (length (workNotices view), map (map questionKey . workQuestions . sourceProgress) (collectedWork view))) <$> readWork collection"
  check "replacement preserves failed notification evidence without replay" (output preserved == "True")
  -- Exercise uncertainty as typed sink data. No external send is claimed here.
  script owner "uncertain-route"
  uncertain <- turn owner "view <- readWork uncertain\ninspectFull (collectedWork view, [failure | Notice _ (Left failure) <- workNotices view])"
  check "uncertain admission preserves the observed question" ("question-a" `Text.isInfixOf` output uncertain && "NotificationAdmissionUnconfirmed" `Text.isInfixOf` output uncertain)
  void $ turn owner "Right replacementSpec <- pure (workDefinition [(\"producer\", producer, updates)] keepWork)\nuncertain <- R.replace uncertain replacementSpec"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [])"
  resolved <- turn owner "(\\view -> (== (([[]],2))) (map (workQuestions . sourceProgress) (collectedWork view), length (workNotices view))) <$> readWork collection"
  check "resolving the final question produces its own delta" (output resolved == "True")
  uncertainOnce <- turn owner "inspectFull . length . workNotices <$> readWork uncertain"
  check "replacement never replays an uncertain send" (output uncertainOnce == "1")
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  closed <- turn owner "(\\view -> inspectFull (length (workNotices view), collectedWork view)) <$> readWork collection"
  check "closure is quiet but the final result still arrives" ("(3," `Text.isPrefixOf` output closed && "finished" `Text.isInfixOf` output closed)
  void $ turn owner "finishWork collection\nfinishWork uncertain"

-- Source publications and explicit queries use the same mailbox. A snapshot
-- call after publication is a barrier; the test never rearms a progress watch.
independentSources :: Member RecipeCheck effects => Eff effects ()
independentSources = do
  owner <- root
  script owner "attention-sources-setup"
  left <- activation
  void $ turn owner "(right, rightProgress) <- unfoldDeferred (batch campaign wave) (childWithProgress @WorkProgress @Text (withLifetime ActorOwned $ coding projectHead (assignment rightLabel (\"right\" :: Text))))"
  right <- activation
  script owner "attention-sources-route"
  script (checkActor left) "attention-sources-question"
  script (checkActor right) "attention-sources-question"
  void $ turn (checkActor left) "reportProgress (WorkProgress [] [first,second])"
  first <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"],WorkOpen),(\"right\",[],WorkOpen)])) [(sourceName s, map questionKey (workQuestions (sourceProgress s)), Exomonad.Contrib.Routing.sourceStatus s) | s <- collectedWork view]) <$> readWork collection"
  check "left progresses while right is silent" (output first == "True")
  void $ turn owner "Right replacementSpec <- pure (workDefinition [(\"left\", left, leftProgress), (\"right\", right, rightProgress)] countChanges)\ncollection <- R.replace collection replacementSpec"
  void $ turn (checkActor left) "reportProgress (WorkProgress [] [second,first,first])"
  void $ turn owner "readWork collection"
  count <- turn owner "Actor.call wakes (RoutingCount 0 id)"
  check "reordered duplicate facts do not invoke the sink" (output count == "1")
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [first])"
  both <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"]),(\"right\",[\"same-key\"])])) [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> readWork collection"
  check "same-key questions retain both source identities" (output both == "True")
  void $ turn (checkActor left) "respond (\"finished\" :: Text)"
  closed <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"],WorkClosed),(\"right\",[\"same-key\"],WorkOpen)])) [(sourceName s, map questionKey (workQuestions (sourceProgress s)), Exomonad.Contrib.Routing.sourceStatus s) | s <- collectedWork view]) <$> readWork collection"
  check "closure retains unanswered questions" (output closed == "True")
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [])"
  resolved <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"]),(\"right\",[])])) [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> readWork collection"
  check "one resolution cannot erase another source's questions" (output resolved == "True")
  void $ turn (checkActor right) "respond (\"finished\" :: Text)"
  final <- turn owner "(== ([WorkClosed,WorkClosed])) . map Exomonad.Contrib.Routing.sourceStatus . collectedWork <$> readWork collection"
  check "both sources close without rearming" (output final == "True")
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
  inheritedReasoning <- turn (checkActor worker) "inspectFull sharedReasoning"
  check "implementation inherits the parent's resident reasoning binding" ("consumer contract established" `Text.isInfixOf` output inheritedReasoning)
  inheritedHead <- git (checkActor worker) ["rev-parse", "HEAD"]
  check "implementation uses current parent source despite older task provenance" (inheritedHead == shared)
  candidate <- checkpoint (checkActor worker) "feature.txt" "routed candidate\n" "prepare candidate"
  source <- readFile (checkActor worker) "feature.txt"
  check "the routed candidate has actual source evidence" (source == "routed candidate\n")
  if disposition == CancelDestination then void (turn owner "cancelRequest lead") else pure ()
  if disposition == LoseProducer then void (turn (checkActor lead) "stopAgent (responseActor candidate)")
  else void $ turn (checkActor worker) ("respond (Candidate " <> gitOidLiteral candidate <> " [\"read exact feature\"] [\"independent review remains\"])")
  if disposition == CancelDestination then do
    failure <- awaitOutput (checkActor lead) "pollRoute forwarding" (Text.isInfixOf "RouteFailed")
    -- Route diagnostics are rendered text; the reply below owns cancellation state.
    check ("cancellation remains a retained callback failure: " <> failure) ("RouteFailed" `Text.isInfixOf` failure)
    obligation <- turn (checkActor lead) "import Tidepool.Agent.Reply (pollReply)\npollReply sessionReply"
    check "a cancelled destination cannot silently receive success" ("ReplyCancellationRequested" `Text.isInfixOf` output obligation)
  else if disposition == LoseProducer then do
    failure <- awaitOutput (checkActor lead) "pollRoute forwarding" (Text.isInfixOf "RouteFailed")
    check "lost execution remains explicit in the retained route" ("RouteFailed" `Text.isInfixOf` failure)
    obligation <- turn (checkActor lead) "import Tidepool.Agent.Reply (pollReply)\npollReply sessionReply"
    check "lost execution does not become a successful candidate" ("ReplyOpen" `Text.isSuffixOf` output obligation)
  else do
    result <- awaitOutput owner "answer <- pollResponse lead\ninspectFull answer" (Text.isInfixOf candidate)
    check "routing forwards exact evidence without a lead relay turn" ("independent review remains" `Text.isInfixOf` result)
