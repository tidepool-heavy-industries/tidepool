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
  first <- turn owner "inspectFull . map (map questionKey . attentionQuestions) <$> Actor.call forwarding AttentionSnapshot"
  check "the persistent actor consumes the first publication" (output first == "[[\"question-a\"]]")
  void $ turn (checkActor producer) "reportProgress [first]"
  void $ turn owner "Actor.call forwarding AttentionSnapshot"
  counts <- turn owner "Actor.call wakes (RoutingCount 0 id)"
  check "identical attention does not invoke the sink again" (output counts == "1")
  void $ turn (checkActor producer) "reportProgress [first,second]"
  second <- turn owner "inspectFull . map (map questionKey . attentionQuestions) <$> Actor.call forwarding AttentionSnapshot"
  check "later publications arrive without rearming" (output second == "[[\"question-a\",\"question-b\"]]")
  pending <- turn (checkActor producer) "pollReply sessionReply"
  check "publishing progress preserves the original reply" (output pending == "ReplyOpen")
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  closed <- turn owner "inspectFull . map attentionStatus <$> Actor.call forwarding AttentionSnapshot"
  check "source closure leaves the actor's retained state queryable" (output closed == "[AttentionClosed]")
  void $ turn owner "Actor.drainActor forwarding\nActor.awaitExit forwarding"
  void restart
  independentSources

-- Source publications and explicit queries use the same mailbox. A snapshot
-- call after publication is a barrier; the test never rearms a progress watch.
independentSources :: Member RecipeCheck effects => Eff effects ()
independentSources = do
  owner <- root
  script owner "attention-sources-setup"
  left <- activation
  void $ turn owner "(right, rightProgress) <- unfold (batch campaign wave) (childWithProgress @Attention @Text (coding rightLabel projectHead (\"right\" :: Text)))"
  right <- activation
  script owner "attention-sources-route"
  script (checkActor left) "attention-sources-question"
  script (checkActor right) "attention-sources-question"
  void $ turn (checkActor left) "reportProgress [first,second]"
  first <- turn owner "(\\view -> inspectFull [(attentionSource s, map questionKey (attentionQuestions s), attentionStatus s) | s <- view]) <$> Actor.call collection AttentionSnapshot"
  check "left progresses while right is silent" ("[\"same-key\",\"second\"]" `Text.isInfixOf` output first && "(\"right\",[],AttentionOpen)" `Text.isInfixOf` output first)
  void $ turn (checkActor left) "reportProgress [second,first,first]"
  void $ turn owner "Actor.call collection AttentionSnapshot"
  count <- turn owner "Actor.call wakes (RoutingCount 0 id)"
  check "reordered duplicate facts do not invoke the sink" (output count == "1")
  void $ turn (checkActor right) "reportProgress [first]"
  both <- turn owner "(\\view -> inspectFull [(attentionSource s, map questionKey (attentionQuestions s)) | s <- view]) <$> Actor.call collection AttentionSnapshot"
  check "same-key questions retain both source identities" (output both == "[(\"left\",[\"same-key\",\"second\"]),(\"right\",[\"same-key\"])]")
  void $ turn (checkActor left) "respond (\"finished\" :: Text)"
  closed <- turn owner "(\\view -> inspectFull [(attentionSource s, map questionKey (attentionQuestions s), attentionStatus s) | s <- view]) <$> Actor.call collection AttentionSnapshot"
  check "closure retains unanswered questions" ("(\"left\",[\"same-key\",\"second\"],AttentionClosed)" `Text.isInfixOf` output closed)
  void $ turn (checkActor right) "reportProgress []"
  resolved <- turn owner "(\\view -> inspectFull [(attentionSource s, map questionKey (attentionQuestions s)) | s <- view]) <$> Actor.call collection AttentionSnapshot"
  check "one resolution cannot erase another source's questions" (output resolved == "[(\"left\",[\"same-key\",\"second\"]),(\"right\",[])]")
  void $ turn (checkActor right) "respond (\"finished\" :: Text)"
  final <- turn owner "inspectFull . map attentionStatus <$> Actor.call collection AttentionSnapshot"
  check "both sources close without rearming" (output final == "[AttentionClosed,AttentionClosed]")
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
