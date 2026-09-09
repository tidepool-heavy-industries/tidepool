{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RoutingChecks (routing, independentSources) where

import Prelude hiding (readFile, writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script)

routing :: Member RecipeCheck effects => Eff effects ()
routing = do
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
  void $ turn (checkActor producer) "reportProgress [first]"
  first <- activation
  firstValue <- turn (checkActor first) "inspectFull (map questionKey sessionInput)"
  check "the cumulative question route reaches its intended consumer" (checkActor first == checkActor consumer && output firstValue == "[\"question-a\"]")
  void $ turn (checkActor consumer) "respond (\"received\" :: Text)"
  void $ turn (checkActor producer) "reportProgress [first]"
  void $ awaitOutput owner "inspectFull . length <$> listRoutes" (== "3")
  counts <- turn owner "(\\current -> inspectFull [rosterReceivedRequests actor | actor <- snapshotActors current, (rosterActorId actor, rosterActorIncarnation actor) == agentIdentity (forkedActor consumer)]) <$> snapshot"
  check "identical attention does not create another request" (output counts == "[2]")
  void $ turn (checkActor producer) "reportProgress [first,second]"
  second <- activation
  secondValue <- turn (checkActor second) "inspectFull (map questionKey sessionInput)"
  check "the later publication retains the unanswered question" (checkActor second == checkActor consumer && output secondValue == "[\"question-a\",\"question-b\"]")
  void $ turn (checkActor consumer) "respond (\"received\" :: Text)"
  pending <- turn (checkActor producer) "pollReply sessionReply"
  check "publishing progress preserves the original reply" (output pending == "ReplyOpen")
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  completed <- awaitOutput owner "inspectFull <$> (listRoutes >>= traverse pollRoute)" (\state -> not ("RouteWaiting" `Text.isInfixOf` state || "RouteRunning" `Text.isInfixOf` state))
  check "closing progress finishes all routed subscriptions" (completed == "[RouteCompleted,RouteCompleted,RouteCompleted,RouteCompleted]")

  void restart
  independentSources

-- Drive real retained routes: one source cannot block another, erase its facts,
-- or change the identity of a same-key finding in another lane.
independentSources :: Member RecipeCheck effects => Eff effects ()
independentSources = do
  owner <- root
  script owner "attention-sources-setup"
  left <- activation
  void $ turn owner "(right, rightProgress) <- unfold (batch campaign wave) (childWithProgress @Attention @Text (coding rightLabel projectHead (\"right\" :: Text)))"
  right <- activation
  void $ turn owner "consumer <- unfold (batch campaign wave) (child (coding @Text consumerLabel projectHead ([] :: [AttentionSource])))"
  consumer <- activation
  void $ turn (checkActor consumer) "respond (\"ready\" :: Text)"
  script owner "attention-sources-route"
  script (checkActor left) "attention-sources-question"
  script (checkActor right) "attention-sources-question"
  void $ turn (checkActor left) "reportProgress [first,second]"
  first <- activation
  firstView <- turn (checkActor first) "inspectFull [(attentionSource s, map questionKey (attentionQuestions s), attentionStatus s) | s <- sessionInput]"
  check "left progress routes while right is silent" (checkActor first == checkActor consumer && "[\"same-key\",\"second\"]" `Text.isInfixOf` output firstView && "(\"right\",[],AttentionOpen)" `Text.isInfixOf` output firstView)
  void $ turn (checkActor consumer) "respond (\"received\" :: Text)"
  void $ turn (checkActor left) "reportProgress [second,first,first]"
  void $ awaitOutput owner "inspectFull . length <$> listRoutes" (== "3")
  counts <- turn owner "(\\s -> inspectFull [rosterReceivedRequests a | a <- snapshotActors s, (rosterActorId a, rosterActorIncarnation a) == agentIdentity (forkedActor consumer)]) <$> snapshot"
  check "reordered duplicate facts do not enqueue another status" (output counts == "[2]")
  void $ turn (checkActor right) "reportProgress [first]"
  both <- activation
  bothView <- turn (checkActor both) "inspectFull [(attentionSource s, map questionKey (attentionQuestions s)) | s <- sessionInput]"
  check "same-key questions retain both source identities" (output bothView == "[(\"left\",[\"same-key\",\"second\"]),(\"right\",[\"same-key\"])]")
  void $ turn (checkActor consumer) "respond (\"received\" :: Text)"
  void $ turn (checkActor left) "respond (\"finished\" :: Text)"
  closed <- activation
  closedView <- turn (checkActor closed) "inspectFull [(attentionSource s, map questionKey (attentionQuestions s), attentionStatus s) | s <- sessionInput]"
  check "closing a source retains its unanswered questions" ("(\"left\",[\"same-key\",\"second\"],AttentionClosed)" `Text.isInfixOf` output closedView)
  void $ turn (checkActor consumer) "respond (\"received\" :: Text)"
  void $ turn (checkActor right) "reportProgress []"
  resolved <- activation
  resolvedView <- turn (checkActor resolved) "inspectFull [(attentionSource s, map questionKey (attentionQuestions s)) | s <- sessionInput]"
  check "one source resolution does not erase another's retained questions" (output resolvedView == "[(\"left\",[\"same-key\",\"second\"]),(\"right\",[])]")
  void $ turn (checkActor consumer) "respond (\"received\" :: Text)"
  void $ turn (checkActor right) "respond (\"finished\" :: Text)"
  final <- activation
  finalView <- turn (checkActor final) "inspectFull (map attentionStatus sessionInput)"
  check "all sources close without a polling model" (output finalView == "[AttentionClosed,AttentionClosed]")
  void $ turn (checkActor consumer) "respond (\"received\" :: Text)"
  void $ awaitOutput owner "inspectFull <$> (listRoutes >>= traverse pollRoute)" (\value -> not ("RouteWaiting" `Text.isInfixOf` value || "RouteRunning" `Text.isInfixOf` value))

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
