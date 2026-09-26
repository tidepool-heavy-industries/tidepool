{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Route named focused checks without making a model poll their command jobs.
module Project.CheckResults
  ( module Project.TestEvidence
  , NoticePolicy (..), CheckSetupIssue (..), CheckVerdict (..)
  , CheckExecution (..), SourceAssurance (..), CheckOutcome (..), CheckEntry (..)
  , CheckState (..), CheckNotice (..), CheckActor (checkSnapshot)
  , watchChecks, readChecks, finishChecks, checkVerdict
  , checkExecution, checkSourceAssurance, checksSummary
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Actor, Commands)
import Tidepool.Effects.Row (knownEffects)
import Project.TestEvidence

data NoticePolicy = NotifyProblems | NotifyAllTerminal | NotifySummary deriving (Eq, Show)

data CheckSetupIssue = NoFocusedChecks | DuplicateCheckName Text deriving (Eq, Show)

data CheckVerdict = CheckPassed | CheckFailed | CheckUnknown deriving (Eq, Show)

data CheckOutcome = CheckOutcome
  { checkCompletion :: Cmd.CommandResult
  , checkFocused :: FocusedResult
  } deriving (Show)

data CheckEntry = CheckEntry
  { checkName :: Text
  , checkRun :: FocusedRun
  , checkOutcome :: Maybe CheckOutcome
  } deriving (Show)

data CheckNotice = CheckNotice
  { checkNoticeName :: Maybe Text
  , checkNoticeReceipt :: Either NotificationError NotificationReceipt
  }

instance Show CheckNotice where
  show notice = "CheckNotice " ++ show (checkNoticeName notice) ++ " "
    ++ either show (const "NotificationReceipt") (checkNoticeReceipt notice)

data CheckState = CheckState
  { checkEntries :: [CheckEntry]
  , checkNotices :: [CheckNotice]
  }

instance Show CheckState where
  show state = "CheckState " ++ show
    [(checkName entry, fmap (checkVerdict entry) (checkOutcome entry))
      | entry <- checkEntries state]
    ++ " notices=" ++ show (length (checkNotices state))

data CheckActor mode = CheckActor
  { checkState :: mode :- State CheckState
  , checkSnapshot :: mode :- Call () (R.Reply CheckState)
  , checkCompletions :: mode :- Event (Text, FocusedRun, Cmd.CommandResult)
  } deriving Generic

type CheckEffects = LocalEffects CheckActor '[Replies, Actor, Notifications, Commands]

-- | Attach even after a job has completed. The caller retains each original
-- FocusedRun; this actor only observes completion and reads its evidence.
watchChecks
  :: Member Actor effects
  => AgentRef -> NoticePolicy -> [(Text, FocusedRun)]
  -> Eff effects (Either CheckSetupIssue (ActorHandle CheckActor))
watchChecks owner policy runs = case runs of
  [] -> pure (Left NoFocusedChecks)
  _ -> case [name | (name, _) <- runs, length (filter ((== name) . fst) runs) > 1] of
    duplicate : _ -> pure (Left (DuplicateCheckName duplicate))
    [] -> Right <$> R.start (checkDefinition owner policy runs)

readChecks :: Member Actor effects => ActorHandle CheckActor -> Eff effects CheckState
readChecks watcher = R.call (checkSnapshot (R.client watcher)) ()

finishChecks :: Member Actor effects => ActorHandle CheckActor -> Eff effects (Actor.ActorExit CheckState)
finishChecks = R.finish

checkDefinition :: AgentRef -> NoticePolicy -> [(Text, FocusedRun)] -> ActorSpec CheckActor CheckEffects
checkDefinition owner policy runs =
  R.definition "focused-check-results" (Actor.Selected knownEffects) CheckActor
      { checkState = CheckState [CheckEntry name run Nothing | (name, run) <- runs] []
      , checkSnapshot = \() -> R.get
      , checkCompletions = R.on (mconcat
          [fmap (\receipt -> (name, run, receipt)) (Cmd.completion job)
          | (name, run@(FocusedRun _ job)) <- runs]) completed
      }
  where
    completed (name, run, receipt) = do
        result <- collectFocused run
        let outcome = CheckOutcome receipt result
        R.modify' (\state -> state { checkEntries =
          [if checkName entry == name then entry {checkOutcome = Just outcome} else entry
            | entry <- checkEntries state] })
        state <- R.get
        let entry = CheckEntry name run (Just outcome)
        case policy of
          NotifyProblems | checkVerdict entry outcome /= CheckPassed -> notify (Just name) (checkLine entry)
          NotifyAllTerminal -> notify (Just name) (checkLine entry)
          _ -> pure ()
        if policy == NotifySummary && all (maybe False (const True) . checkOutcome) (checkEntries state)
          then notify Nothing (checksSummary state)
          else pure ()
    notify name message = do
      sent <- sendMessage owner message
      R.modify' (\state -> state {checkNotices = checkNotices state ++ [CheckNotice name sent]})

checkVerdict :: CheckEntry -> CheckOutcome -> CheckVerdict
checkVerdict entry outcome
  | not (matchingReceipt entry outcome) = CheckUnknown
  | focusedPassed result = CheckPassed
  | ExecutionFailed _ _ <- checkExecution entry outcome = CheckFailed
  | Cmd.failure (focusedCommand result) /= Nothing = CheckFailed
  | otherwise = CheckUnknown
  where
    result = checkFocused outcome

checkExecution :: CheckEntry -> CheckOutcome -> CheckExecution
checkExecution entry outcome
  | not (matchingReceipt entry outcome) = ExecutionUnknown
  | otherwise = focusedExecution (checkFocused outcome)

checkSourceAssurance :: CheckEntry -> CheckOutcome -> SourceAssurance
checkSourceAssurance entry outcome | not (matchingReceipt entry outcome) = SourceUnrecorded
checkSourceAssurance _ outcome = focusedSourceAssurance (checkFocused outcome)

matchingReceipt :: CheckEntry -> CheckOutcome -> Bool
matchingReceipt entry outcome =
  let FocusedRun spec job = checkRun entry
      result = checkFocused outcome
  in focusedSpec result == spec
    && Cmd.completedJob (focusedCommand result) == job
    && Cmd.commandResult (focusedCommand result) == checkCompletion outcome

checkLine :: CheckEntry -> Text
checkLine entry = checkName entry <> ": " <> case checkOutcome entry of
  Nothing -> "running"
  Just outcome ->
    verdictText (checkVerdict entry outcome)
      <> "; executed " <> executionText (checkExecution entry outcome)
      <> "; source " <> sourceText (checkSourceAssurance entry outcome)
      <> "; " <> Text.pack (show (Cmd.commandOutcome (checkCompletion outcome)))
      <> "; cleanup " <> Text.pack (show (Cmd.commandCleanup (checkCompletion outcome)))
      <> maybe "" ("; evidence " <>) (focusedEvidencePath (checkFocused outcome))
      <> either ("; " <>) (\record -> "; log " <> recordOutput record)
           (focusedEvidence (checkFocused outcome))

checksSummary :: CheckState -> Text
checksSummary state = "focused checks: " <> Text.intercalate "; "
  [checkName entry <> " " <> maybe "running" (\outcome ->
      verdictText (checkVerdict entry outcome)
        <> " (executed " <> executionText (checkExecution entry outcome)
        <> ", source " <> sourceText (checkSourceAssurance entry outcome) <> ")")
      (checkOutcome entry)
    | entry <- checkEntries state]

executionText :: CheckExecution -> Text
executionText (ExecutionPassed count) = Text.pack (show count) <> " passed"
executionText (ExecutionFailed passed failed) =
  Text.pack (show passed) <> " passed, " <> Text.pack (show failed) <> " failed"
executionText ExecutionUnknown = "unknown"

sourceText :: SourceAssurance -> Text
sourceText SourceVerified = "verified"
sourceText (SourceModified _) = "dirty"
sourceText (SourceDifferent _) = "different"
sourceText SourceUnrecorded = "unrecorded"

verdictText :: CheckVerdict -> Text
verdictText CheckPassed = "passed"
verdictText CheckFailed = "failed"
verdictText CheckUnknown = "unknown"
