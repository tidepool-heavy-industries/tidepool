{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RoutingChecks (routing, messageDeltas, independentSources, twoLaneHandoff, notificationRetention, automaticReview, requestRecovery, candidateHistory, declaredRepair, forwardingFailure) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script)

routing :: Member RecipeCheck effects => Eff effects ()
routing = do
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
  void $ turn owner "layoutReceipt <- pollResponse (forkedResponse consumer)"
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
  pending <- turn (checkActor producer) "pollReply sessionReply"
  check "publishing progress preserves the original reply" (output pending == "ReplyOpen")
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  closed <- turn owner "(== ([WorkClosed])) . map sourceStatus . collectedWork <$> readWork forwarding"
  check "source closure leaves the actor's retained state queryable" (output closed == "True")
  void $ turn owner "finishWork forwarding"
  void restart
  independentSources
  void restart
  twoLaneHandoff

-- Real Delivery values cross a typed parent mailbox; partial source remains in
-- the local collector and both later final heads are integrated by the owner.
twoLaneHandoff :: Member RecipeCheck effects => Eff effects ()
twoLaneHandoff = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> literal baseline <> " :: Text")
  script owner "handoff-setup"
  left <- activation
  void $ turn owner "(right, rightProgress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @Delivery (coding rightLabel projectHead task))"
  right <- activation
  script owner "handoff-router"
  partial <- checkpoint (checkActor left) "left.txt" "partial\n" "left partial checkpoint"
  void $ turn (checkActor left) ("reportProgress (WorkProgress [Candidate " <> literal partial <> " [] [\"final source pending\"]] [])")
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
        ("respond (Produced (Delivered (ReviewedCandidate sessionInput (Candidate " <> literal commit
          <> " [\"read final source\"] [\"product acceptance remains\"]) [\"recipe source assertion\"] \"model-free handoff fixture\") "
          <> literal commit <> " [\"read final source\"]))")
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

automaticReview :: Member RecipeCheck effects => Eff effects ()
automaticReview = reviewCycle False False

requestRecovery :: Member RecipeCheck effects => Eff effects ()
requestRecovery = reviewCycle True False

declaredRepair :: Member RecipeCheck effects => Eff effects ()
declaredRepair = reviewCycle False True

reviewCycle :: Member RecipeCheck effects => Bool -> Bool -> Eff effects ()
reviewCycle failAfterAdmission automaticRepair = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> literal baseline <> " :: Text")
  script owner "review-flow-setup"
  reviewer <- activation
  void $ turn (checkActor reviewer) "respond (Produced (Repair (reviewInput sessionInput) []))"
  void $ turn owner "(worker, progress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) (solTaskFrom workerLabel projectHead task))"
  worker <- activation
  void $ turn owner "let onReview = keepWork :: WorkSink (Outcome ReviewDecision)\nlet onStopped = const Nothing :: Settlement (Outcome Candidate) -> Maybe Text\nowner <- actorContext\nlet repairPolicy = OwnerRepairs\nlet repairLabel = \"repair-produced-candidate\" :: RequestLabel"
  if automaticRepair then void $ turn owner "let repairPolicy = RetainedImplementer (forkedActor worker)" else pure ()
  if failAfterAdmission then do
    source <- readFile owner ".shoal/checks/review-continuation.hs"
    let withoutStart = fst (Text.breakOn "reviewBox <-" source)
        definition = "let reviewBoxDefinition" <> snd (Text.breakOn " = coordinationActor" withoutStart)
        faulty = Text.replace "let reviewBoxDefinition" "let failingDefinition"
          (Text.replace "          startReview own result" "          startReview own result\n          error \"injected after admission\"" definition)
    void $ turn owner (withoutStart <> "\n" <> faulty <> "\nreviewBox <- R.start failingDefinition")
  else script owner "review-continuation"
  candidate <- checkpoint (checkActor worker) "feature.txt" "candidate feature\n" "candidate for automatic review"
  void $ turn (checkActor worker) ("respond (Produced (Candidate " <> literal candidate <> " [\"read feature\"] [\"integration pending\"]))")
  reviewing <- activation
  check "settlement submits to the available retained reviewer without a model relay" (checkActor reviewing == checkActor reviewer)
  if failAfterAdmission then do
    void $ turn owner "reviewBox <- R.replace reviewBox reviewBoxDefinition"
    retained <- turn owner "flow <- R.call (reviewView (R.client reviewBox)) ()\ninspectFull (length (reviewCollectors flow))"
    check ("replacement receives the exact request handle queued before admission: " <> lastOutput retained) (lastOutput retained == "1")
  else pure ()
  input <- turn (checkActor reviewing) "inspectFull (reviewInput sessionInput)"
  check "the automatic request carries the exact candidate and gates" (candidate `Text.isInfixOf` output input && "integration pending" `Text.isInfixOf` output input)
  void $ git (checkActor reviewing) ["merge", "--ff-only", candidate]
  feature <- readFile (checkActor reviewing) "feature.txt"
  check "the reviewer incorporates the actual settled source" (feature == "candidate feature\n")
  (accepting, finalCandidate) <- if automaticRepair then do
    void $ turn (checkActor reviewing) "respond (Produced (Repair (reviewInput sessionInput) [\"repair the feature\"]))"
    repairing <- activation
    check "the declared repair edge reuses the settled implementer" (checkActor repairing == checkActor worker)
    repairInput <- turn (checkActor repairing) "inspectFull (repairInput sessionInput, repairFindings sessionInput)"
    check "the repair carries the exact candidate and finding" (candidate `Text.isInfixOf` output repairInput && "repair the feature" `Text.isInfixOf` output repairInput)
    repaired <- checkpoint (checkActor repairing) "feature.txt" "repaired feature\n" "repair reviewed candidate"
    void $ turn (checkActor repairing) ("respond (Produced (Candidate " <> literal repaired <> " [\"read repaired feature\"] [\"integration pending\"]))")
    repeated <- activation
    check "the repaired candidate returns to the same reviewer automatically" (checkActor repeated == checkActor reviewer)
    void $ git (checkActor repeated) ["merge", "--ff-only", repaired]
    checkedFeature <- readFile (checkActor repeated) "feature.txt"
    check "the repeated reviewer checks repaired source" (checkedFeature == "repaired feature\n")
    pure (repeated, repaired)
  else pure (reviewing, candidate)
  void $ turn (checkActor accepting) "reportProgress (WorkProgress [reviewInput sessionInput] [])"
  void $ turn (checkActor accepting) "respond (Produced (Accepted (ReviewedCandidate (reviewAssignment sessionInput) (reviewInput sessionInput) [\"read exact feature\"] \"ready for owner integration\")))"
  received <- awaitOutput owner "flow <- R.call (reviewView (R.client reviewBox)) ()\ninspectFull (reviewEvents flow)" (Text.isInfixOf "Accepted")
  let arrived = finalCandidate `Text.isInfixOf` received && "WorkChanged" `Text.isInfixOf` received && "Accepted" `Text.isInfixOf` received
  check (if arrived then "typed review progress and acceptance return through the mailbox"
         else "missing typed review evidence: " <> received) arrived
  finished <- turn owner (("let expectedReviews = " <> (if automaticRepair then "2" else "1") <> "\n") <> "finished <- R.finish reviewBox\n(== (0,expectedReviews)) (case finished of { Actor.Completed state -> (length (reviewCollectors state), length (completedReviews state)); _ -> (-1,-1) })")
  check "each admitted review completes and its collector drains" (lastOutput finished == "True")
  if automaticRepair then do
    forwarding <- turn owner "case finished of { Actor.Completed state -> mapM (R.forwardingExit . snd) (repairAttempts state); _ -> pure [] }"
    check "the result-only repair forwarder exits without a model retirement turn" ("Completed" `Text.isInfixOf` output forwarding)
    void $ git owner ["merge", "--ff-only", finalCandidate]
    integrated <- readFile owner "feature.txt"
    check "the owner integrates and checks the actual accepted repair" (integrated == "repaired feature\n")
  else pure ()
  void $ turn owner "let blockedLabel = \"blocked-implementation\"\n(worker, progress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) (solTaskFrom blockedLabel projectHead task))"
  blocked <- activation
  script owner "review-continuation"
  void $ turn (checkActor blocked) "respond (Blocked \"needs owner decision\" [\"contract conflict\"] :: Outcome Candidate)"
  stopped <- awaitOutput owner "flow <- R.call (reviewView (R.client reviewBox)) ()\ninspectFull (length (reviewCollectors flow), stoppedCandidates flow, length (stoppedNotices flow))" (Text.isInfixOf "contract conflict")
  check "a blocked candidate retains its receipt without starting review" ("(0," `Text.isInfixOf` stopped && "contract conflict" `Text.isInfixOf` stopped)
  void $ turn owner "R.finish reviewBox"
  if automaticRepair then do
    void $ turn owner "let mismatchLabel = \"mismatched-source\"\n(worker, progress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) (solTaskFrom mismatchLabel projectHead task))"
    mismatched <- activation
    script owner "review-continuation"
    actual <- checkpoint (checkActor mismatched) "different.txt" "actual submitted source\n" "source evidence differs from claim"
    void $ turn (checkActor mismatched) ("respond (Produced (Candidate " <> literal baseline <> " [] []))")
    _ <- awaitOutput owner "flow <- R.call (reviewView (R.client reviewBox)) ()\nnot (null (sourceProblems flow))" (Text.isSuffixOf "True")
    let expectedProblem = "candidate " <> baseline <> "; submitted " <> actual
    rejected <- turn owner
      ("null (reviewCollectors flow) && map snd (sourceProblems flow) == [" <> literal expectedProblem
        <> "] && (case candidateReceipts flow of { [Right receipt] -> case responseValue receipt of { Produced candidate -> candidateCommit candidate == "
        <> literal baseline <> "; _ -> False }; _ -> False })")
    check "source mismatch retains the original receipt and stops automatic review"
      (lastOutput rejected == "True")
    void $ turn owner "R.finish reviewBox"
  else pure ()

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
  void $ turn owner "collection <- followWork [(\"producer\", forkedResponse producer, updates)] keepWork"
  let candidates = "let firstCandidate = Candidate " <> literal baseline <> " [\"first check\"] []\nlet secondCandidate = firstCandidate { checkedCommands = [\"different check\"] }"
  void $ turn (checkActor producer) (candidates <> "\nreportProgress (WorkProgress [firstCandidate] [])\nreportProgress (WorkProgress [secondCandidate] [])")
  before <- turn owner "view <- readWork collection\n(== ((2,2))) (length (workEvidence (sourceProgress (head (collectedWork view)))), length (workHistory view))"
  check "same-commit changed evidence remains distinct and ordered" (lastOutput before == "True")
  void $ turn owner (candidates <> "\nR.send (incorporatedWork (R.client collection)) (\"producer\", [firstCandidate])\nview <- readWork collection\nlet briefBefore = workSnapshotSummary id view")
  frontier <- turn owner "(== ([[\"different check\"]])) (map checkedCommands (outstandingEvidence view (head (collectedWork view))))"
  check "incorporation removes only the exact handled evidence" (output frontier == "True")
  void $ turn (checkActor producer) "mapM_ reportProgress (replicate 100 (WorkProgress [firstCandidate,secondCandidate] []))"
  after <- turn owner "view <- readWork collection\n(== ((True,102,[[\"first check\"],[\"different check\"]]))) (workSnapshotSummary id view == briefBefore, length (workHistory view), map checkedCommands (workEvidence (sourceProgress (head (collectedWork view)))))"
  check "100 retained publications do not expand the normal brief or erase evidence"
    (lastOutput after == "True")
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  void $ turn owner "finishWork collection"

notificationRetention :: Member RecipeCheck effects => Eff effects ()
notificationRetention = do
  owner <- root
  script owner "progress-route-producer"
  producer <- activation
  script owner "progress-route-consumer"
  _ <- activation
  void $ turn owner "stopAgent (forkedActor consumer)"
  void $ turn owner "import qualified Tidepool.Actor as Actor\ncollection <- followWork [(\"producer\", forkedResponse producer, updates)] (notifyWork (forkedActor consumer) (workMessage id))"
  script (checkActor producer) "progress-route-questions"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  retained <- turn owner "view <- readWork collection\ninspectFull (collectedWork view, [failure | Notice _ (Left failure) <- workNotices view])"
  check "a failed real send retains the new question and typed failure" ("question-a" `Text.isInfixOf` output retained && "NotificationUnavailable" `Text.isInfixOf` output retained)
  compact <- turn (checkActor producer) "inspectFull (workMessage (id :: Text -> Text) (workChange \"producer\" (ProgressCursor 1) (WorkProgress [] [first]) (WorkProgress [] [first,second])))"
  check "question messages contain only the new question" ("question-b" `Text.isInfixOf` output compact && not ("question-a" `Text.isInfixOf` output compact))
  combined <- turn (checkActor producer) "inspectFull (withCheckpoints (workMessage (id :: Text -> Text)) (workChange \"producer\" (ProgressCursor 1) (WorkProgress [] [first]) (WorkProgress [Candidate \"partial-head\" [] [\"review pending\"]] [first,second])))"
  check "a useful checkpoint and a new question both reach the owner" ("partial-head" `Text.isInfixOf` output combined && "question-b" `Text.isInfixOf` output combined && not ("question-a" `Text.isInfixOf` output combined))
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,first])"
  once <- turn owner "inspectFull . length . workNotices <$> readWork collection"
  check "repeated questions do not retry failed notification" (output once == "1")
  void $ turn owner "collection <- R.replace collection (workDefinition [(\"producer\", forkedResponse producer, updates)] (notifyWork (forkedActor consumer) (workMessage id)))"
  preserved <- turn owner "(\\view -> (== ((1,[[\"question-a\"]]))) (length (workNotices view), map (map questionKey . workQuestions . sourceProgress) (collectedWork view))) <$> readWork collection"
  check "replacement preserves failed notification evidence without replay" (output preserved == "True")
  -- Exercise uncertainty as typed sink data. No external send is claimed here.
  script owner "uncertain-route"
  uncertain <- turn owner "view <- readWork uncertain\ninspectFull (collectedWork view, [failure | Notice _ (Left failure) <- workNotices view])"
  check "uncertain admission preserves the observed question" ("question-a" `Text.isInfixOf` output uncertain && "NotificationAdmissionUnconfirmed" `Text.isInfixOf` output uncertain)
  void $ turn owner "uncertain <- R.replace uncertain (workDefinition [(\"producer\", forkedResponse producer, updates)] keepWork)"
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
  void $ turn owner "(right, rightProgress) <- unfold (batch campaign wave) (childWithProgress @WorkProgress @Text (coding rightLabel projectHead (\"right\" :: Text)))"
  right <- activation
  script owner "attention-sources-route"
  script (checkActor left) "attention-sources-question"
  script (checkActor right) "attention-sources-question"
  void $ turn (checkActor left) "reportProgress (WorkProgress [] [first,second])"
  first <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"],WorkOpen),(\"right\",[],WorkOpen)])) [(sourceName s, map questionKey (workQuestions (sourceProgress s)), sourceStatus s) | s <- collectedWork view]) <$> readWork collection"
  check "left progresses while right is silent" (output first == "True")
  void $ turn owner "collection <- R.replace collection (workDefinition [(\"left\", forkedResponse left, leftProgress), (\"right\", forkedResponse right, rightProgress)] countChanges)"
  void $ turn (checkActor left) "reportProgress (WorkProgress [] [second,first,first])"
  void $ turn owner "readWork collection"
  count <- turn owner "Actor.call wakes (RoutingCount 0 id)"
  check "reordered duplicate facts do not invoke the sink" (output count == "1")
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [first])"
  both <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"]),(\"right\",[\"same-key\"])])) [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> readWork collection"
  check "same-key questions retain both source identities" (output both == "True")
  void $ turn (checkActor left) "respond (\"finished\" :: Text)"
  closed <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"],WorkClosed),(\"right\",[\"same-key\"],WorkOpen)])) [(sourceName s, map questionKey (workQuestions (sourceProgress s)), sourceStatus s) | s <- collectedWork view]) <$> readWork collection"
  check "closure retains unanswered questions" (output closed == "True")
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [])"
  resolved <- turn owner "(\\view -> (== ([(\"left\",[\"same-key\",\"second\"]),(\"right\",[])])) [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> readWork collection"
  check "one resolution cannot erase another source's questions" (output resolved == "True")
  void $ turn (checkActor right) "respond (\"finished\" :: Text)"
  final <- turn owner "(== ([WorkClosed,WorkClosed])) . map sourceStatus . collectedWork <$> readWork collection"
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
  void $ turn owner ("let routeCampaign = " <> literal campaign <> " :: Text")
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> literal baseline <> " :: Text")
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
  if disposition == CancelDestination then void (turn owner "cancelRequest (forkedResponse lead)") else pure ()
  if disposition == LoseProducer then void (turn (checkActor lead) "stopAgent (forkedActor candidate)")
  else void $ turn (checkActor worker) ("respond (Candidate " <> literal candidate <> " [\"read exact feature\"] [\"independent review remains\"])")
  if disposition == CancelDestination then do
    failure <- awaitOutput (checkActor lead) "pollRoute forwarding" (Text.isInfixOf "RouteFailed")
    check "cancellation remains a retained callback failure" ("CancellationRequested" `Text.isInfixOf` failure)
    obligation <- turn (checkActor lead) "pollReply destination"
    check "a cancelled destination cannot silently receive success" ("ReplyCancellationRequested" `Text.isInfixOf` output obligation)
  else if disposition == LoseProducer then do
    failure <- awaitOutput (checkActor lead) "pollRoute forwarding" (Text.isInfixOf "RouteFailed")
    check "lost execution remains explicit in the retained route" ("RouteFailed" `Text.isInfixOf` failure)
    obligation <- turn (checkActor lead) "pollReply destination"
    check "lost execution does not become a successful candidate" (output obligation == "ReplyOpen")
  else do
    result <- awaitOutput owner "answer <- pollResponse (forkedResponse lead)\ninspectFull answer" (Text.isInfixOf candidate)
    check "routing forwards exact evidence without a lead relay turn" ("independent review remains" `Text.isInfixOf` result)
