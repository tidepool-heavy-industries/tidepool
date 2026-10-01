{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.WorkflowReminderRoutingChecks (questionRouting, snapshotTrial) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check
import Project.Checks (script)

-- A real followWork sink keeps its ordinary notification receipts while the
-- reminder actor sees only newly opened, source-identified question deltas.
questionRouting :: Member RecipeCheck effects => Eff effects ()
questionRouting = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  script owner "progress-route-producer"
  producer <- activation
  script owner "progress-route-consumer"
  consumer <- activation
  void $ turn owner (Text.unlines
    [ "import qualified Data.Text as Text"
    , "import qualified Tidepool.Actor.Record as R"
    , "let source = " <> gitOidLiteral source
    , "Right reminders <- startRemindersWith (\\_ _ -> pure Suggest) me reviewRelayPolicy"
    , "Right routed <- followWork [(\"producer\", producer, updates)] (withQuestionReminders reminders (notifyWork (responseActor consumer) (workMessage id)))"
    ])
  void $ turn (checkActor producer) (Text.unlines
    [ "let first = Question \"relay\" (DesignQuestion \"plans/review.md\" " <> gitOidLiteral source <> " \"Manual within-contract review relay\" [\"review result for exact candidate\"] [] [\"repair\"] )"
    , "let second = Question \"ownership\" (DesignQuestion \"plans/review.md\" " <> gitOidLiteral source <> " \"Shared ownership changed\" [\"new owner question\"] [] [\"parent\"] )"
    , "reportProgress (WorkProgress [] [first])"
    ])
  first <- turn owner (Text.unlines
    [ "view <- readWork routed"
    , "memory <- R.call (reminderRead (R.client reminders)) ()"
    , "inspectFull (length (workNotices view) == 1 && length (reminderEntries memory) == 1"
    , "  && map noticeEvent (workNotices view) == [0]"
    , "  && length [() | Notice _ (Left NotificationUnavailable) <- workNotices view] == 1"
    , "  && all (\\entry -> reminderDecision entry == Suggest && (case reminderReceipt entry of { Just (Left NotificationUnavailable) -> True; _ -> False })) (reminderEntries memory)"
    , "  && all (\\entry -> \"plans/review.md#relay@\" `Text.isInfixOf` reminderFacts (reminderEpisode entry)"
    , "    && \"review result for exact candidate\" `Text.isInfixOf` reminderEvidence (reminderEpisode entry)) (reminderEntries memory))"
    ])
  check "opened question retains both typed failed sends and one reminder" ("True" `Text.isSuffixOf` output first)
  pending <- turn (checkActor producer) "import Tidepool.Agent.Reply (pollReply)\npollReply sessionReply"
  check "routing leaves the worker's original reply pending" ("ReplyOpen" `Text.isSuffixOf` output pending)

  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  duplicate <- turn owner "view <- readWork routed\nmemory <- R.call (reminderRead (R.client reminders)) ()\ninspectFull (length (workNotices view) == 1 && length (reminderEntries memory) == 1)"
  check "same progress publication produces no duplicate notice or judgment" ("True" `Text.isSuffixOf` output duplicate)

  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,second])"
  changed <- turn owner (Text.unlines
    [ "view <- readWork routed"
    , "memory <- R.call (reminderRead (R.client reminders)) ()"
    , "inspectFull (length (workNotices view) == 2 && length (reminderEntries memory) == 2"
    , "  && any (\\entry -> \"plans/review.md#ownership@\" `Text.isInfixOf` reminderFacts (reminderEpisode entry)) (reminderEntries memory))"
    ])
  check "new source-identified question receives its own episode" ("True" `Text.isSuffixOf` output changed)

  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [])"
  resolved <- turn owner "view <- readWork routed\nmemory <- R.call (reminderRead (R.client reminders)) ()\ninspectFull (length (workNotices view) == 3 && length (reminderEntries memory) == 2)"
  check "question resolution keeps ordinary sink notice without another reminder" ("True" `Text.isSuffixOf` output resolved)
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  terminal <- turn owner "view <- readWork routed\nmemory <- R.call (reminderRead (R.client reminders)) ()\ninspectFull (length (workNotices view) == 4 && length (reminderEntries memory) == 2)"
  check "terminal result keeps ordinary sink notice without a reminder" ("True" `Text.isSuffixOf` output terminal)
  void $ turn (checkActor consumer) "respond (\"done\" :: Text)"
  void $ turn owner "finishWork routed\nR.finish reminders"

-- The owner reads a retained event and submits it to a separate shadow trial.
-- Collector delivery remains independent of the trial actor.
snapshotTrial :: Member RecipeCheck effects => Eff effects ()
snapshotTrial = do
  owner <- root
  source <- git owner ["rev-parse", "HEAD"]
  script owner "progress-route-producer"
  producer <- activation
  void $ turn owner (Text.unlines
    [ "import qualified Data.Text as T"
    , "import Project.NotificationTrial"
    , "import Project.WorkflowReminders"
    , "import qualified Tidepool.Actor.Record as R"
    , "Right trial <- startReminderTrialWith (\\_ _ -> pure Suggest) me (notificationPolicy \"Integrate reviewed component slices\")"
    , "Right routed <- followWork [(\"producer\", producer, updates)] (notifyWork me (workMessage id))"
    ])
  void $ turn (checkActor producer) (Text.unlines
    [ "let question = Question \"ownership\" (DesignQuestion \"plans/component.md\" " <> gitOidLiteral source <> " \"Old status repeated, but who owns the new file?\" [\"ownership not assigned\"] [] [\"parent\"])"
    , "reportProgress (WorkProgress [Candidate " <> gitOidLiteral source <> " [\"unit-one\"] [\"browser gate remains\"]] [question])"
    ])
  observed <- turn owner (Text.unlines
    [ "view <- readWork routed"
    , "let Just episode = notificationEpisode QuestionsAndResults id view 0"
    , "judged <- R.call (reminderObserve (R.client (trialReminders trial))) episode"
    , "memory <- R.call (reminderRead (R.client (trialReminders trial))) ()"
    , "inspectFull (length (workNotices view) == 1 && length (reminderEntries memory) == 1"
    , "  && (case judged of { Right entry -> (case reminderReceipt entry of { Nothing -> True; Just _ -> False }) && notificationDecision entry == InterruptOwner; Left _ -> False })"
    , "  && all (\\entry -> \"Unresolved questions: producer: ownership\" `T.isInfixOf` reminderFacts (reminderEpisode entry) && \"browser gate remains\" `T.isInfixOf` reminderFacts (reminderEpisode entry)) (reminderEntries memory))"
    ])
  check "snapshot trial retains the original notice and judges its evidence" ("True" `Text.isSuffixOf` output observed)
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [question])"
  duplicate <- turn owner "view <- readWork routed\nmemory <- R.call (reminderRead (R.client (trialReminders trial))) ()\ninspectFull (length (workNotices view) == 1 && length (reminderEntries memory) == 1 && notificationEpisode QuestionsAndResults id view 1 == Nothing)"
  check "unchanged progress does not produce another collector notice" ("True" `Text.isSuffixOf` output duplicate)
  void $ turn owner "R.finish (trialReminders trial)"
  void $ turn (checkActor producer) (Text.unlines
    [ "let second = Question \"new-gate\" (DesignQuestion \"plans/component.md\" " <> gitOidLiteral source <> " \"New gate needs owner attention\" [\"gate unresolved\"] [] [\"parent\"])"
    , "reportProgress (WorkProgress [] [question,second])"
    ])
  afterTrial <- turn owner "view <- readWork routed\ninspectFull (length (workNotices view) == 2 && length (workHistory view) == 3)"
  check "stopped trial does not affect later ordinary notices" ("True" `Text.isSuffixOf` output afterTrial)
  void $ turn (checkActor producer) "respond (\"done\" :: Text)"
  void $ turn owner "finishWork routed"
