{-# LANGUAGE OverloadedStrings #-}

-- | Context-specific policies and one concrete progress-routing consumer.
module Project.WorkflowReminderExamples
  ( reviewRelayPolicy, independentWorkPolicy
  , questionEpisode, withQuestionReminders
  ) where

import qualified Data.Text as Text
import Tidepool.Actors.Exomonad (ActorHandle)
import Tidepool.Worktree (renderGitOid)
import Project.Routing (WorkDelta (..), WorkEvent (..), WorkSink)
import Project.Types (Question (..), DesignQuestion (..))
import Project.WorkflowReminders

reviewRelayPolicy :: ReminderPolicy
reviewRelayPolicy = ReminderPolicy
  { reminderContext = "A feature owner has an exact reviewed candidate, a named repair owner, and the installed Project.ReviewFlow composition."
  , reminderTrigger = "The evidence describes the owner manually forwarding ordinary within-contract review findings to that repair owner and collecting a revised candidate for another review."
  , reminderExclusions = "Do not suggest when the findings dispute a shared invariant, change ownership, lack an exact candidate, or when ReviewFlow is unavailable or already owns this cycle. A single necessary design decision is not routine relay."
  , reminderSuggestion = "For the next within-contract repair cycle, use the installed Project.ReviewFlow policy with the exact candidate, named repair owner and bounded repair limit. Let it route repair/review; keep shared-design questions with the parent. Read its published example and retain the flow handle. Do not start a second review of this candidate while one is pending."
  , reminderEpisodeLimit = 8
  }

independentWorkPolicy :: ReminderPolicy
independentWorkPolicy = ReminderPolicy
  { reminderContext = "A feature owner can admit bounded Luna children with the published Project.Work and Project.Routing interfaces."
  , reminderTrigger = "The supplied evidence explicitly names multiple ready obligations with independent inputs and disjoint owned paths, yet describes doing them sequentially."
  , reminderExclusions = "Abstain if dependencies, shared-file ownership, available resources or required source are unclear. Do not infer readiness from elapsed time, actor silence or task counts. Do not suggest more forks when those obligations already have executing owners."
  , reminderSuggestion = "Consider admitting the named independent obligations together with the installed unfold/childWithProgress composition and collecting their results through Project.Routing.followWork. Pass exact source, owned paths, acceptance and published helper names. Retain local integration ownership; keep dependent work behind its actual prerequisite."
  , reminderEpisodeLimit = 8
  }

-- | One newly opened progress delta is one immutable reminder episode. The
-- source name and cursor identify the publication within one routing actor;
-- every question keeps its exact source and key in the evidence. A later
-- changed delta gets a new key.
questionEpisode :: WorkEvent value -> Maybe ReminderEpisode
questionEpisode (WorkChanged name delta)
  | null (openedQuestions delta) = Nothing
  | otherwise = Just ReminderEpisode
      { reminderKey = name <> ":" <> Text.pack (show (deltaCursor delta))
      , reminderFacts = Text.unlines
          [ ref question <> " finding: " <> questionFinding (questionDetails question)
          | question <- openedQuestions delta
          ]
      , reminderEvidence = Text.unlines
          [ ref question <> " reported evidence: " <> evidence (questionDetails question)
          | question <- openedQuestions delta
          ]
      }
  where
    ref question = questionPlan details <> "#" <> questionKey question
      <> "@" <> renderGitOid (questionSource details)
      where details = questionDetails question
    evidence details = case questionEvidence details of
      [] -> "none supplied"
      reports -> Text.intercalate "; " reports
questionEpisode _ = Nothing

-- | Attach suggestions to an existing live WorkSink. The routing sink still
-- owns its ordinary notifications and receipt evidence; the reminder actor
-- receives only newly opened, source-identified question deltas.
withQuestionReminders :: ActorHandle Reminders -> WorkSink value -> WorkSink value
withQuestionReminders reminders = withReminders reminders questionEpisode
