{-# LANGUAGE OverloadedStrings #-}

-- | Pure projection for owner-driven comparison of retained work notices.
-- The owner reads the collector, then submits an episode to a separate
-- ReminderTrial. The collector never waits on or sends to that trial.
module Project.NotificationTrial
  ( NotificationDecision (..), notificationDecision, notificationPolicy
  , notificationEpisode
  ) where

import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Worktree (renderGitOid)
import Project.Routing
import Project.Types
import Project.WorkflowReminders

data NotificationDecision = RecordOnly | InterruptOwner | RoutingUncertain Text
  deriving (Show, Eq)

notificationDecision :: ReminderEntry -> NotificationDecision
notificationDecision entry = case reminderDecision entry of
  Suggest -> InterruptOwner
  NotApplicable -> RecordOnly
  ReminderUnresolved reason -> RoutingUncertain reason

notificationPolicy :: Text -> ReminderPolicy
notificationPolicy taskIntent = ReminderPolicy
  { reminderContext = taskIntent
  , reminderTrigger = "The incoming event contains a new decision, unresolved question, conflicting evidence, failure, or useful unincorporated result needing the owner's attention. Any such content makes a mixed stale/new message actionable."
  , reminderExclusions = "Record only if ALL meaningful content repeats facts already explicitly handled in the supplied collector state. An acknowledgment is not evidence of incorporation. Missing context, missing source, or uncertain novelty must remain unresolved. Do not infer completion from silence or prose claiming acceptance."
  , reminderSuggestion = "Interrupt the owner with this event and its retained evidence. This is a shadow routing proposal; it changes no delivery or authority."
  , reminderEpisodeLimit = 16
  }

-- | Project one retained event using the notice policy active when it was
-- published. Pass that policy explicitly: a later policy change does not
-- rewrite the event's history. An event without a notice has no trial episode.
notificationEpisode
  :: WorkNoticePolicy -> (value -> Text) -> WorkState value -> Int
  -> Maybe ReminderEpisode
notificationEpisode policy render state index
  | index < 0 = Nothing
  | otherwise = case drop index (workHistory state) of
      [] -> Nothing
      event : _ -> fmap (episode event) (workNoticeMessage policy (workMessage render) event)
  where
    key = "work-event:" <> Text.pack (show index)
    episode event message = ReminderEpisode key
      (Text.unlines
        [ "Incoming event: " <> message
        , "Incoming candidate facts: " <> case event of
            WorkChanged _ delta -> Text.intercalate "; "
              (map candidateFacts (addedEvidence delta ++ map checkpointCandidate (addedReviewed delta)))
            _ -> "none"
        , "Explicitly incorporated candidates: " <> Text.intercalate "; "
            [name <> "@" <> candidateFacts candidate
            | (name, candidate) <- handledWork state]
        , "Unresolved questions: " <> Text.intercalate "; "
            [sourceName source <> ": " <> questionKey question <> ": "
              <> questionFinding (questionDetails question)
            | source <- collectedWork state
            , question <- workQuestions (sourceProgress source)]
        ])
      ("Original event retained in workHistory at " <> key
        <> "; exact responses remain in collectedWork. No omitted excerpt establishes completion.")

-- A repeated OID can carry changed check claims or remaining gates.
candidateFacts :: Candidate -> Text
candidateFacts candidate = renderGitOid (candidateCommit candidate)
  <> " reported checks: " <> Text.pack (show (checkedCommands candidate))
  <> " remaining gates: " <> Text.pack (show (remainingGates candidate))
