{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
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
  awaitCell owner "opened question retains both typed failed sends and one reminder" (Text.unlines
    [ "do"
    , "  view <- readWork routed"
    , "  memory <- R.call (reminderRead (R.client reminders)) ()"
    , "  pure (length (workNotices view) == 1 && length (reminderEntries memory) == 1"
    , "    && map noticeEvent (workNotices view) == [0]"
    , "    && length [() | Notice _ (Left NotificationUnavailable) <- workNotices view] == 1"
    , "    && all (\\entry -> reminderDecision entry == Suggest && (case reminderReceipt entry of { Just (Left NotificationUnavailable) -> True; _ -> False })) (reminderEntries memory))"
    ])
  void $ turn owner "memory <- R.call (reminderRead (R.client reminders)) ()"
  assertCell owner "text: reminder retains source-identified question and review evidence"
    "length (reminderEntries memory) == 1 && all (\\entry -> \"plans/review.md#relay@\" `Text.isInfixOf` reminderFacts (reminderEpisode entry) && \"review result for exact candidate\" `Text.isInfixOf` reminderEvidence (reminderEpisode entry)) (reminderEntries memory)"
  void $ turn (checkActor producer) "import Tidepool.Agent.Reply (pollReply, ReplyState (..))\npending <- pollReply sessionReply"
  assertCell (checkActor producer) "routing leaves the worker's original reply pending" "pending == ReplyOpen"

  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first])"
  awaitCell owner "same progress publication produces no duplicate notice or judgment"
    "do { view <- readWork routed; memory <- R.call (reminderRead (R.client reminders)) (); pure (length (workNotices view) == 1 && length (reminderEntries memory) == 1) }"

  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [first,second])"
  awaitCell owner "new source-identified question receives its own episode"
    "do { view <- readWork routed; memory <- R.call (reminderRead (R.client reminders)) (); pure (length (workNotices view) == 2 && length (reminderEntries memory) == 2) }"
  void $ turn owner "memory <- R.call (reminderRead (R.client reminders)) ()"
  assertCell owner "text: new question retains its own source identity"
    "any (\\entry -> \"plans/review.md#ownership@\" `Text.isInfixOf` reminderFacts (reminderEpisode entry)) (reminderEntries memory)"

  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [])"
  awaitCell owner "question resolution keeps ordinary sink notice without another reminder"
    "do { view <- readWork routed; memory <- R.call (reminderRead (R.client reminders)) (); pure (length (workNotices view) == 3 && length (reminderEntries memory) == 2) }"
  void $ turn (checkActor producer) "respond (\"finished\" :: Text)"
  awaitCell owner "terminal result keeps ordinary sink notice without a reminder"
    "do { view <- readWork routed; memory <- R.call (reminderRead (R.client reminders)) (); pure (length (workNotices view) == 4 && length (reminderEntries memory) == 2) }"
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
  awaitCell owner "snapshot trial receives the original notice"
    "do { view <- readWork routed; pure (length (workNotices view) == 1) }"
  void $ turn owner (Text.unlines
    [ "view <- readWork routed"
    , "let Just episode = notificationEpisode QuestionsAndResults id view 0"
    , "judged <- R.call (reminderObserve (R.client (trialReminders trial))) episode"
    , "memory <- R.call (reminderRead (R.client (trialReminders trial))) ()"
    ])
  assertCell owner "snapshot trial retains the original notice and judges its evidence"
    "length (workNotices view) == 1 && length (reminderEntries memory) == 1 && (case judged of { Right entry -> (case reminderReceipt entry of { Nothing -> True; Just _ -> False }) && notificationDecision entry == InterruptOwner; Left _ -> False })"
  assertCell owner "text: snapshot trial retains unresolved question and pending gate"
    "length (reminderEntries memory) == 1 && all (\\entry -> \"Unresolved questions: producer: ownership\" `T.isInfixOf` reminderFacts (reminderEpisode entry) && \"browser gate remains\" `T.isInfixOf` reminderFacts (reminderEpisode entry)) (reminderEntries memory)"
  void $ turn (checkActor producer) "reportProgress (WorkProgress [] [question])"
  awaitCell owner "unchanged progress does not produce another collector notice"
    "do { view <- readWork routed; memory <- R.call (reminderRead (R.client (trialReminders trial))) (); pure (length (workNotices view) == 1 && length (reminderEntries memory) == 1 && notificationEpisode QuestionsAndResults id view 1 == Nothing) }"
  void $ turn owner "R.finish (trialReminders trial)"
  void $ turn (checkActor producer) (Text.unlines
    [ "let second = Question \"new-gate\" (DesignQuestion \"plans/component.md\" " <> gitOidLiteral source <> " \"New gate needs owner attention\" [\"gate unresolved\"] [] [\"parent\"])"
    , "reportProgress (WorkProgress [] [question,second])"
    ])
  awaitCell owner "stopped trial does not affect later ordinary notices"
    "do { view <- readWork routed; pure (length (workNotices view) == 2 && length (workHistory view) == 3) }"
  void $ turn (checkActor producer) "respond (\"done\" :: Text)"
  void $ turn owner "finishWork routed"
