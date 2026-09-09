{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RoutingChecks (routing, messageDeltas, independentSources, twoLaneHandoff, notificationRetention, automaticReview) where

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
  notificationRetention
  void restart
  automaticReview
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
  layoutResult <- turn owner "inspectFull (case layoutReceipt of { ResponseReady result -> Just (responseValue result); _ -> Nothing })"
  check "an indented finding list reaches the actual reply" (output layoutResult == "Just \"ready; layout preserved\"")
  script owner "progress-route"
  script (checkActor producer) "progress-route-questions"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  first <- turn owner "inspectFull . map (map questionKey . workQuestions . sourceProgress) . collectedWork <$> Actor.call forwarding WorkSnapshot"
  check "the persistent actor consumes the first publication" (output first == "[[\"question-a\"]]")
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  void $ turn owner "Actor.call forwarding WorkSnapshot"
  counts <- turn owner "Actor.call wakes (RoutingCount 0 id)"
  check "identical attention does not invoke the sink again" (output counts == "1")
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,second])"
  second <- turn owner "inspectFull . map (map questionKey . workQuestions . sourceProgress) . collectedWork <$> Actor.call forwarding WorkSnapshot"
  check "later publications arrive without rearming" (output second == "[[\"question-a\",\"question-b\"]]")
  pending <- turn (checkActor producer) "pollReply sessionReply"
  check "publishing progress preserves the original reply" (output pending == "ReplyOpen")
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  closed <- turn owner "inspectFull . map sourceStatus . collectedWork <$> Actor.call forwarding WorkSnapshot"
  check "source closure leaves the actor's retained state queryable" (output closed == "[WorkClosed]")
  void $ turn owner "Actor.drainActor forwarding\nActor.awaitExit forwarding"
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
  initial <- turn owner "view <- Actor.call handoff WorkSnapshot\ninspectFull (collectedWork view)"
  check "partial checkpoint is retained before final publication" (partial `Text.isInfixOf` output initial)
  before <- turn owner "Actor.call parent (HandoffSnapshot length)"
  check "partial progress stays local to the subtree" (output before == "0")
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
  final <- turn owner "view <- Actor.call handoff WorkSnapshot\ninspectFull (collectedWork view)"
  check "both later final heads arrive without rearming" (all (`Text.isInfixOf` output final) [partial, leftFinal, rightFinal])
  parentView <- turn owner "received <- Actor.call parent (HandoffSnapshot id)\ninspectFull received"
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
  retired <- turn owner "Actor.drainActor handoff\nretired <- Actor.awaitExit handoff\ninspectFull (case retired of { Actor.Completed state -> collectedWork state; _ -> [] })"
  check "drain retains both incorporated final heads" (all (`Text.isInfixOf` output retired) [leftFinal, rightFinal])
  void $ turn owner "Actor.drainActor parent\nActor.awaitExit parent"

automaticReview :: Member RecipeCheck effects => Eff effects ()
automaticReview = do
  owner <- root
  baseline <- git owner ["rev-parse", "HEAD"]
  void $ turn owner ("let sourceHead = " <> literal baseline <> " :: Text")
  script owner "review-flow-setup"
  reviewer <- activation
  void $ turn (checkActor reviewer) "respond (Produced (Repair (reviewInput sessionInput) []))"
  void $ turn owner "(worker, progress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) (solTaskFrom workerLabel projectHead task))"
  worker <- activation
  void $ turn owner "let onReview = keepWork :: WorkSink (Outcome ReviewDecision)\nlet onStopped = const Nothing :: Settlement (Outcome Candidate) -> Maybe Text\nowner <- actorContext"
  script owner "review-continuation"
  candidate <- checkpoint (checkActor worker) "feature.txt" "candidate feature\n" "candidate for automatic review"
  void $ turn (checkActor worker) ("respond (Produced (Candidate " <> literal candidate <> " [\"read feature\"] [\"integration pending\"]))")
  reviewing <- activation
  check "settlement submits to the available retained reviewer without a model relay" (checkActor reviewing == checkActor reviewer)
  input <- turn (checkActor reviewing) "inspectFull (reviewInput sessionInput)"
  check "the automatic request carries the exact candidate and gates" (candidate `Text.isInfixOf` output input && "integration pending" `Text.isInfixOf` output input)
  void $ git (checkActor reviewing) ["merge", "--ff-only", candidate]
  feature <- readFile (checkActor reviewing) "feature.txt"
  check "the reviewer incorporates the actual settled source" (feature == "candidate feature\n")
  void $ turn (checkActor reviewing) "reportProgress (WorkProgress [reviewInput sessionInput] [])"
  void $ turn (checkActor reviewing) "respond (Produced (Accepted (ReviewedCandidate (reviewAssignment sessionInput) (reviewInput sessionInput) [\"read exact feature\"] \"ready for owner integration\")))"
  received <- awaitOutput owner "flow <- Actor.call reviewBox (ReviewFlowSnapshot id)\ninspectFull (reviewEvents flow)" (Text.isInfixOf "WorkFinished")
  let arrived = candidate `Text.isInfixOf` received && "WorkChanged" `Text.isInfixOf` received && "Accepted" `Text.isInfixOf` received
  check (if arrived then "typed review progress and acceptance return through the mailbox"
         else "missing typed review evidence: " <> received) arrived
  void $ turn owner "import Data.Foldable (traverse_)\ntraverse_ Actor.drainActor (reviewCollectors flow)\nretired <- traverse Actor.awaitExit (reviewCollectors flow)\nActor.drainActor reviewBox\nActor.awaitExit reviewBox"
  void $ turn owner "let Right blockedLabel = branchLabel \"blocked-implementation\"\n(worker, progress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @(Outcome Candidate) (solTaskFrom blockedLabel projectHead task))"
  blocked <- activation
  script owner "review-continuation"
  void $ turn (checkActor blocked) "respond (Blocked \"needs owner decision\" [\"contract conflict\"] :: Outcome Candidate)"
  stopped <- awaitOutput owner "Actor.call reviewBox (ReviewFlowSnapshot (\\flow -> inspectFull (length (reviewCollectors flow), stoppedCandidates flow, stoppedNotices flow)))" (Text.isInfixOf "contract conflict")
  check "a blocked candidate retains its receipt without starting review" ("(0," `Text.isPrefixOf` stopped && "contract conflict" `Text.isInfixOf` stopped)
  void $ turn owner "Actor.drainActor reviewBox\nActor.awaitExit reviewBox"

messageDeltas :: Member RecipeCheck effects => Eff effects ()
messageDeltas = do
  owner <- root
  script owner "question-message-deltas"
  result <- turn owner "inspectFull questionMessageChecks"
  check "amendments upsert once; source advances, resolutions and unchanged questions stay distinct"
    (output result == "(True,True,True,True)")

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
  retained <- turn owner "view <- Actor.call collection WorkSnapshot\ninspectFull (collectedWork view, [failure | Notice _ (Left failure) <- workNotices view])"
  check "a failed real send retains the new question and typed failure" ("question-a" `Text.isInfixOf` output retained && "NotificationUnavailable" `Text.isInfixOf` output retained)
  compact <- turn (checkActor producer) "inspectFull (workMessage (id :: Text -> Text) (WorkChanged \"producer\" (WorkProgress [] [first]) (WorkProgress [] [first,second])))"
  check "question messages contain only the new question" ("question-b" `Text.isInfixOf` output compact && not ("question-a" `Text.isInfixOf` output compact))
  combined <- turn (checkActor producer) "inspectFull (withCheckpoints (workMessage (id :: Text -> Text)) (WorkChanged \"producer\" (WorkProgress [] [first]) (WorkProgress [Candidate \"partial-head\" [] [\"review pending\"]] [first,second])))"
  check "a useful checkpoint and a new question both reach the owner" ("partial-head" `Text.isInfixOf` output combined && "question-b" `Text.isInfixOf` output combined && not ("question-a" `Text.isInfixOf` output combined))
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,first])"
  once <- turn owner "inspectFull . length . workNotices <$> Actor.call collection WorkSnapshot"
  check "repeated questions do not retry failed notification" (output once == "1")
  void $ turn owner "collection <- Actor.replaceActor collection (workDefinition [(\"producer\", forkedResponse producer, updates)] (notifyWork (forkedActor consumer) (workMessage id)))"
  preserved <- turn owner "(\\view -> inspectFull (length (workNotices view), map (map questionKey . workQuestions . sourceProgress) (collectedWork view))) <$> Actor.call collection WorkSnapshot"
  check "replacement preserves failed notification evidence without replay" (output preserved == "(1,[[\"question-a\"]])")
  -- Exercise uncertainty as typed sink data. No external send is claimed here.
  script owner "uncertain-route"
  uncertain <- turn owner "view <- Actor.call uncertain WorkSnapshot\ninspectFull (collectedWork view, [failure | Notice _ (Left failure) <- workNotices view])"
  check "uncertain admission preserves the observed question" ("question-a" `Text.isInfixOf` output uncertain && "NotificationAdmissionUnconfirmed" `Text.isInfixOf` output uncertain)
  void $ turn owner "uncertain <- Actor.replaceActor uncertain (workDefinition [(\"producer\", forkedResponse producer, updates)] keepWork)"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [])"
  resolved <- turn owner "(\\view -> inspectFull (map (workQuestions . sourceProgress) (collectedWork view), length (workNotices view))) <$> Actor.call collection WorkSnapshot"
  check "resolving the final question produces its own delta" (output resolved == "([[]],2)")
  uncertainOnce <- turn owner "inspectFull . length . workNotices <$> Actor.call uncertain WorkSnapshot"
  check "replacement never replays an uncertain send" (output uncertainOnce == "1")
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  closed <- turn owner "(\\view -> inspectFull (length (workNotices view), collectedWork view)) <$> Actor.call collection WorkSnapshot"
  check "closure is quiet but the final result still arrives" ("(3," `Text.isPrefixOf` output closed && "finished" `Text.isInfixOf` output closed)
  void $ turn owner "Actor.drainActor collection\nActor.awaitExit collection\nActor.drainActor uncertain\nActor.awaitExit uncertain"

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
  first <- turn owner "(\\view -> inspectFull [(sourceName s, map questionKey (workQuestions (sourceProgress s)), sourceStatus s) | s <- collectedWork view]) <$> Actor.call collection WorkSnapshot"
  check "left progresses while right is silent" ("[\"same-key\",\"second\"]" `Text.isInfixOf` output first && "(\"right\",[],WorkOpen)" `Text.isInfixOf` output first)
  void $ turn owner "collection <- Actor.replaceActor collection (workDefinition [(\"left\", forkedResponse left, leftProgress), (\"right\", forkedResponse right, rightProgress)] countChanges)"
  void $ turn (checkActor left) "reportProgress (WorkProgress [] [second,first,first])"
  void $ turn owner "Actor.call collection WorkSnapshot"
  count <- turn owner "Actor.call wakes (RoutingCount 0 id)"
  check "reordered duplicate facts do not invoke the sink" (output count == "1")
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [first])"
  both <- turn owner "(\\view -> inspectFull [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> Actor.call collection WorkSnapshot"
  check "same-key questions retain both source identities" (output both == "[(\"left\",[\"same-key\",\"second\"]),(\"right\",[\"same-key\"])]")
  void $ turn (checkActor left) "respond (\"finished\" :: Text)"
  closed <- turn owner "(\\view -> inspectFull [(sourceName s, map questionKey (workQuestions (sourceProgress s)), sourceStatus s) | s <- collectedWork view]) <$> Actor.call collection WorkSnapshot"
  check "closure retains unanswered questions" ("(\"left\",[\"same-key\",\"second\"],WorkClosed)" `Text.isInfixOf` output closed)
  void $ turn (checkActor right) "reportProgress (WorkProgress [] [])"
  resolved <- turn owner "(\\view -> inspectFull [(sourceName s, map questionKey (workQuestions (sourceProgress s))) | s <- collectedWork view]) <$> Actor.call collection WorkSnapshot"
  check "one resolution cannot erase another source's questions" (output resolved == "[(\"left\",[\"same-key\",\"second\"]),(\"right\",[])]")
  void $ turn (checkActor right) "respond (\"finished\" :: Text)"
  final <- turn owner "inspectFull . map sourceStatus . collectedWork <$> Actor.call collection WorkSnapshot"
  check "both sources close without rearming" (output final == "[WorkClosed,WorkClosed]")
  void $ turn owner "Actor.drainActor collection\nActor.awaitExit collection"

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
