{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.WorkflowReminderChecks (bounded, shadow, semanticCases, checkpointCases) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Tidepool.Check

bounded :: Member RecipeCheck effects => Eff effects ()
bounded = do
  owner <- root
  void $ turn owner $ T.unlines
    [ "import qualified Data.Text as T"
    , "import Project.WorkflowReminders"
    , "import Project.WorkflowReminderExamples"
    , "import qualified Tidepool.Actor.Record as R"
    , "let policy = reviewRelayPolicy { reminderEpisodeLimit = 2 }"
    , "let episode key = ReminderEpisode key \"manual relay with exact candidate and repair owner\" \"retained review request\""
    , "Right reminders <- startRemindersWith (\\_ _ -> pure Suggest) me policy"
    ]
  void $ turn owner "first <- R.call (reminderObserve (R.client reminders)) (episode \"one\")"
  assertCell owner "applicable reminder retains attempted notice"
    "case first of { Right entry -> reminderDecision entry == Suggest && (case reminderReceipt entry of { Just _ -> True; Nothing -> False }); Left _ -> False }"
  void $ turn owner "again <- R.call (reminderObserve (R.client reminders)) (episode \"one\")\nstate <- R.call (reminderRead (R.client reminders)) ()"
  assertCell owner "same episode produces only one retained entry" "length (reminderEntries state) == 1"
  void $ turn owner "R.call (reminderObserve (R.client reminders)) (episode \"two\")"
  void $ turn owner "observed <- R.call (reminderObserve (R.client reminders)) (episode \"three\")"
  assertCell owner "episode budget refuses further judgments"
    "case observed of { Left ReminderBudgetSpent -> True; _ -> False }"
  void $ turn owner "observed <- R.call (reminderObserve (R.client reminders)) (ReminderEpisode \"\" \"facts\" \"ref\")"
  assertCell owner "missing episode identity refused"
    "case observed of { Left InvalidReminderEpisode -> True; _ -> False }"
  void $ turn owner "Right skipped <- startRemindersWith (\\_ _ -> pure NotApplicable) me policy\nobserved <- R.call (reminderObserve (R.client skipped)) (episode \"skip\")"
  assertCell owner "inapplicable observation sends no notice"
    "case observed of { Right entry -> reminderDecision entry == NotApplicable && (case reminderReceipt entry of { Nothing -> True; Just _ -> False }); Left _ -> False }"
  void $ turn owner "Right uncertain <- startRemindersWith (\\_ _ -> pure (ReminderUnresolved \"service unavailable\")) me policy\nobserved <- R.call (reminderObserve (R.client uncertain)) (episode \"unknown\")"
  assertCell owner "unavailable judgment retained without notice"
    "case observed of { Right entry -> (case reminderDecision entry of { ReminderUnresolved _ -> True; _ -> False }) && (case reminderReceipt entry of { Nothing -> True; Just _ -> False }); Left _ -> False }"
  assertCell owner "text: unresolved reminder retains the supplied reason"
    "case observed of { Right entry -> case reminderDecision entry of { ReminderUnresolved reason -> reason == \"service unavailable\"; _ -> False }; Left _ -> False }"
  void $ turn owner "R.finish reminders\nR.finish skipped\nR.finish uncertain"

-- | Optional live semantic trial, separate from deterministic mechanism checks.
semanticCases :: Member RecipeCheck effects => Eff effects ()
semanticCases = do
  owner <- root
  void $ turn owner "import Project.WorkflowReminders\nimport Project.WorkflowReminderExamples"
  void $ turn owner "observed <- semanticReminder reviewRelayPolicy (ReminderEpisode \"relay\" \"Owner is manually forwarding within-contract findings for exact candidate abc to named implementer alice, then requesting review again. Exomonad.Contrib.ReviewFlow is installed and validated; no review flow currently owns this cycle.\" \"review/repair conversation\")"
  assertCell owner "semantic trial recognizes instructed review relay" "observed == Suggest"
  void $ turn owner "observed <- semanticReminder reviewRelayPolicy (ReminderEpisode \"design\" \"Review findings dispute a shared persistence invariant; ownership is unresolved.\" \"review findings\")"
  assertCell owner "semantic trial excludes shared design dispute" "observed == NotApplicable"
  void $ turn owner "observed <- semanticReminder independentWorkPolicy (ReminderEpisode \"silence\" \"The actor has been silent for ten minutes. No ready tasks or dependencies were reported.\" \"one status observation\")"
  assertCell owner "semantic trial does not infer fork readiness from silence" "observed /= Suggest"

-- Shadow execution proves the same decision path without an outgoing notice.
shadow :: Member RecipeCheck effects => Eff effects ()
shadow = do
  owner <- root
  void $ turn owner $ T.unlines
    [ "import qualified Data.Text as T"
    , "import Project.WorkflowReminders"
    , "import Project.WorkflowReminderExamples"
    , "import qualified Tidepool.Actor.Record as R"
    , "let policy = reviewRelayPolicy { reminderEpisodeLimit = 2 }"
    , "let episode = ReminderEpisode \"review-1\" \"exact review remains pending\" \"request 63\""
    , "Right shadow <- startReminderTrialWith (\\_ _ -> pure Suggest) me policy"
    , "let trial = trialReminders shadow"
    ]
  void $ turn owner "entry <- R.call (reminderObserve (R.client trial)) episode"
  assertCell owner "shadow suggestion retains judgment without notification receipt"
    "case entry of { Right retained -> reminderDecision retained == Suggest && (case reminderReceipt retained of { Nothing -> True; Just _ -> False }); Left _ -> False }"
  void $ turn owner "changed <- R.call (reminderObserve (R.client trial)) (episode { reminderFacts = \"review has now settled\" })"
  assertCell owner "episode identity cannot silently reuse a judgment for changed evidence"
    "case changed of { Left ReminderEpisodeChanged -> True; _ -> False }"
  void $ turn owner "oversized <- R.call (reminderObserve (R.client trial)) (ReminderEpisode \"large\" (T.replicate 7000 \"x\") \"original artifact\")"
  assertCell owner "total packet budget refuses oversized evidence before judgment"
    "case oversized of { Left ReminderPacketTooLarge -> True; _ -> False }"
  void $ turn owner "state <- R.call (reminderRead (R.client trial)) ()"
  assertCell owner "refused episodes do not become judged entries" "length (reminderEntries state) == 1"
  void $ turn owner "R.finish trial"

-- Bounded live replay of the observed module-only checkpoint and counterexamples.
checkpointCases :: Member RecipeCheck effects => Eff effects ()
checkpointCases = do
  owner <- root
  void $ turn owner "import Project.WorkflowReminders\nimport Project.WorkflowReminderExamples\nimport Project.NotificationTrial"
  void $ turn owner "missing <- semanticReminder consumerCheckpointPolicy (ReminderEpisode \"consumer-missing\" \"Agreed checkpoint: first compiling main.rs production invocation. Owner reports that checkpoint is due now. Candidate supplies a library module only; main.rs is explicitly not wired.\" \"Retained owner checkpoint and candidate diff\")"
  assertCell owner "declared checkpoint asks for missing production consumer" "missing == Suggest"
  void $ turn owner "blocked <- semanticReminder consumerCheckpointPolicy (ReminderEpisode \"expected-red\" \"Consumer tests intentionally remain red pending the producer owned by another actor. Parent acknowledged this dependency and accepted the waiting checkpoint.\" \"Expected-red report and parent acknowledgment\")"
  assertCell owner "acknowledged external producer is not a missed checkpoint" "blocked == NotApplicable"
  void $ turn owner "silence <- semanticReminder consumerCheckpointPolicy (ReminderEpisode \"quiet\" \"Actor has been quiet for ten minutes. No agreed checkpoint or current candidate is known.\" \"Elapsed-time observation only\")"
  assertCell owner "silence cannot establish a missed consumer checkpoint" "silence /= Suggest"
  void $ turn owner "mixed <- semanticReminder (notificationPolicy \"Integrate reviewed slices\") (ReminderEpisode \"mixed\" \"Already handled: candidate abc. Incoming event repeats candidate abc but also asks who owns the new browser file; that question is unresolved.\" \"Collector retained source-identified question\")"
  assertCell owner "shadow routing preserves novel question mixed with stale status" "mixed == Suggest"
  void $ turn owner "stale <- semanticReminder (notificationPolicy \"Integrate reviewed slices\") (ReminderEpisode \"stale\" \"Already handled and incorporated: candidate abc. Incoming event only repeats candidate abc, with no other claim, question, or change.\" \"Collector handled candidate and full incoming event\")"
  assertCell owner "shadow routing records entirely handled repetition" "stale == NotApplicable"
