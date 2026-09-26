{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.WorkflowReminderChecks (bounded, semanticCases) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Tidepool.Check

bounded :: Member RecipeCheck effects => Eff effects ()
bounded = do
  owner <- root
  void $ turn owner $ T.unlines
    [ "import Project.WorkflowReminders"
    , "import Project.WorkflowReminderExamples"
    , "import qualified Tidepool.Actor.Record as R"
    , "let policy = reviewRelayPolicy { reminderEpisodeLimit = 2 }"
    , "let episode key = ReminderEpisode key \"manual relay with exact candidate and repair owner\" \"retained review request\""
    , "Right reminders <- startRemindersWith (\\_ _ -> pure Suggest) me policy"
    ]
  first <- turn owner "first <- R.call (reminderObserve (R.client reminders)) (episode \"one\")\ninspectFull first"
  check "applicable reminder retains attempted notice" ("Suggest" `T.isInfixOf` output first && "Just" `T.isInfixOf` output first)
  duplicate <- turn owner "again <- R.call (reminderObserve (R.client reminders)) (episode \"one\")\nstate <- R.call (reminderRead (R.client reminders)) ()\ninspectFull (length (reminderEntries state))"
  check "same episode produces only one retained entry" ("1" `T.isInfixOf` output duplicate)
  void $ turn owner "R.call (reminderObserve (R.client reminders)) (episode \"two\")"
  exhausted <- turn owner "observed <- R.call (reminderObserve (R.client reminders)) (episode \"three\")\ninspectFull observed"
  check "episode budget refuses further judgments" ("ReminderBudgetSpent" `T.isInfixOf` output exhausted)
  invalid <- turn owner "observed <- R.call (reminderObserve (R.client reminders)) (ReminderEpisode \"\" \"facts\" \"ref\")\ninspectFull observed"
  check "missing episode identity refused" ("InvalidReminderEpisode" `T.isInfixOf` output invalid)
  skipped <- turn owner "Right skipped <- startRemindersWith (\\_ _ -> pure NotApplicable) me policy\nobserved <- R.call (reminderObserve (R.client skipped)) (episode \"skip\")\ninspectFull observed"
  check "inapplicable observation sends no notice" ("NotApplicable" `T.isInfixOf` output skipped && "reminderReceipt = Nothing" `T.isInfixOf` output skipped)
  unavailable <- turn owner "Right uncertain <- startRemindersWith (\\_ _ -> pure (ReminderUnresolved \"service unavailable\")) me policy\nobserved <- R.call (reminderObserve (R.client uncertain)) (episode \"unknown\")\ninspectFull observed"
  check "unavailable judgment retained without notice" ("service unavailable" `T.isInfixOf` output unavailable && "reminderReceipt = Nothing" `T.isInfixOf` output unavailable)
  void $ turn owner "R.finish reminders\nR.finish skipped\nR.finish uncertain"

-- | Optional live semantic trial, separate from deterministic mechanism checks.
semanticCases :: Member RecipeCheck effects => Eff effects ()
semanticCases = do
  owner <- root
  void $ turn owner "import Project.WorkflowReminders\nimport Project.WorkflowReminderExamples"
  applicable <- turn owner "observed <- semanticReminder reviewRelayPolicy (ReminderEpisode \"relay\" \"Owner is manually forwarding within-contract findings for exact candidate abc to named implementer alice, then requesting review again. Project.ReviewFlow is installed and validated; no review flow currently owns this cycle.\" \"review/repair conversation\")\ninspectFull observed"
  check "semantic trial recognizes instructed review relay" ("Suggest" `T.isInfixOf` output applicable)
  excluded <- turn owner "observed <- semanticReminder reviewRelayPolicy (ReminderEpisode \"design\" \"Review findings dispute a shared persistence invariant; ownership is unresolved.\" \"review findings\")\ninspectFull observed"
  check "semantic trial excludes shared design dispute" ("NotApplicable" `T.isInfixOf` output excluded)
  unknown <- turn owner "observed <- semanticReminder independentWorkPolicy (ReminderEpisode \"silence\" \"The actor has been silent for ten minutes. No ready tasks or dependencies were reported.\" \"one status observation\")\ninspectFull observed"
  check "semantic trial does not infer fork readiness from silence" (not ("Suggest" `T.isInfixOf` output unknown))
