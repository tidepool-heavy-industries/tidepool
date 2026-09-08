{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.RoutingChecks (routing) where

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
  void $ turn (checkActor consumer) "respond (\"ready\" :: Text)"
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
