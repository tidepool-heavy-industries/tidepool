{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Route named focused checks without making a model poll their command jobs.
module Exomonad.Contrib.CheckResults
  ( module Exomonad.Contrib.Check.Cargo
  , NoticePolicy (..), CheckSetupIssue (..), CheckVerdict (..)
  , CheckExecution (..), SourceAssurance (..), CheckOutcome (..), CheckEntry (..)
  , CheckState (..), CheckNotice (..), CheckActor (checkSnapshot)
  , watchChecks, watchChecksWithRefusals, watchChecksInto, readChecks, finishChecks, checkVerdict
  , checkExecution, checkSourceAssurance, checkEvidenceComplete, checkLine, checksSummary
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
import Exomonad.Contrib.Check.Cargo

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
  , checkForwardAdmission :: Maybe (Either Text ())
  }

instance Show CheckState where
  show state = "CheckState " ++ show
    [(checkName entry, fmap (checkVerdict entry) (checkOutcome entry))
      | entry <- checkEntries state]
    ++ " notices=" ++ show (length (checkNotices state))
    ++ " forwarding=" ++ show (checkForwardAdmission state)

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
watchChecks owner policy = watchChecksWithRefusals owner policy []

-- | Include refused starts in the aggregate notice without pretending they
-- have jobs to observe. The caller retains these typed refusals separately.
watchChecksWithRefusals
  :: Member Actor effects
  => AgentRef -> NoticePolicy -> [(Text, FocusedSetupIssue)]
  -> [(Text, FocusedRun)]
  -> Eff effects (Either CheckSetupIssue (ActorHandle CheckActor))
watchChecksWithRefusals owner policy refused =
  watchChecksWith (NotifyChecks owner policy refused) refused

-- A continuation receives the complete typed state once. It owns presentation;
-- the check actor does not also wake a model with the same completion.
watchChecksInto
  :: Member Actor effects
  => R.Send CheckState -> [(Text, FocusedRun)]
  -> Eff effects (Either CheckSetupIssue (ActorHandle CheckActor))
watchChecksInto destination = watchChecksWith (ForwardChecks destination) []

data CheckDelivery
  = NotifyChecks AgentRef NoticePolicy [(Text, FocusedSetupIssue)]
  | ForwardChecks (R.Send CheckState)

watchChecksWith
  :: Member Actor effects
  => CheckDelivery -> [(Text, FocusedSetupIssue)] -> [(Text, FocusedRun)]
  -> Eff effects (Either CheckSetupIssue (ActorHandle CheckActor))
watchChecksWith delivery refused runs = case runs of
  [] -> pure (Left NoFocusedChecks)
  _ -> case [name | (name, _) <- named, length (filter ((== name) . fst) named) > 1] of
    duplicate : _ -> pure (Left (DuplicateCheckName duplicate))
    [] -> Right <$> R.start (checkDefinition delivery runs)
  where named = [(name, ()) | (name, _) <- refused] ++ [(name, ()) | (name, _) <- runs]

readChecks :: Member Actor effects => ActorHandle CheckActor -> Eff effects CheckState
readChecks watcher = R.call (checkSnapshot (R.client watcher)) ()

finishChecks :: Member Actor effects => ActorHandle CheckActor -> Eff effects (Actor.ActorExit CheckState)
finishChecks = R.finish

checkDefinition :: CheckDelivery -> [(Text, FocusedRun)] -> ActorSpec CheckActor CheckEffects
checkDefinition delivery runs =
  R.definition "focused-check-results" (Actor.Selected knownEffects) CheckActor
      { checkState = CheckState [CheckEntry name run Nothing | (name, run) <- runs] [] Nothing
      , checkSnapshot = \() -> R.get
      , checkCompletions = R.on (mconcat
          [fmap (\receipt -> (name, run, receipt)) (Cmd.completion job)
          | (name, run) <- runs, let job = runJob run]) completed
      }
  where
    completed completion@(name, _, _) = do
      state <- R.get
      case [checkOutcome entry | entry <- checkEntries state, checkName entry == name] of
        [Nothing] -> collect completion
        _ -> pure ()
    collect (name, run, receipt) = do
        result <- collectFocused run
        let outcome = CheckOutcome receipt result
        R.modify' (\state -> state { checkEntries =
          [if checkName entry == name then entry {checkOutcome = Just outcome} else entry
            | entry <- checkEntries state] })
        state <- R.get
        let entry = CheckEntry name run (Just outcome)
        case delivery of
          ForwardChecks destination ->
            if all (maybe False (const True) . checkOutcome) (checkEntries state)
              then do
                admission <- R.trySend destination state
                R.modify' (\current -> current { checkForwardAdmission = Just admission })
              else pure ()
          NotifyChecks owner policy refused -> do
            case policy of
              NotifyProblems | checkVerdict entry outcome /= CheckPassed -> notify owner (Just name) (checkLine entry)
              NotifyAllTerminal -> notify owner (Just name) (checkLine entry)
              _ -> pure ()
            if policy == NotifySummary && all (maybe False (const True) . checkOutcome) (checkEntries state)
              then notify owner Nothing (checksSummaryWithRefusals refused state)
              else pure ()
    notify owner name message = do
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

-- | A complete result belongs to the original job and proves a passing check
-- or a counted assertion failure. Diagnostic 'CheckFailed' alone is insufficient.
checkEvidenceComplete :: CheckEntry -> CheckOutcome -> Bool
checkEvidenceComplete entry outcome =
  matchingReceipt entry outcome && focusedEvidenceComplete (checkFocused outcome)

matchingReceipt :: CheckEntry -> CheckOutcome -> Bool
matchingReceipt entry outcome =
  let spec = runSpec (checkRun entry)
      job = runJob (checkRun entry)
      result = checkFocused outcome
  in focusedSpec result == spec
    && Cmd.completedJob (focusedCommand result) == job
    && Cmd.commandResult (focusedCommand result) == checkCompletion outcome

checkLine :: CheckEntry -> Text
checkLine entry = checkName entry <> ": " <> case checkOutcome entry of
  Nothing -> "running"
  Just outcome ->
    verdictText (checkVerdict entry outcome)
      <> "; " <> focusedResultSummary (checkRun entry) (checkFocused outcome)
      <> if matchingReceipt entry outcome then "" else "; completion receipt mismatch"

checksSummary :: CheckState -> Text
checksSummary state = "focused checks: " <> Text.intercalate "; "
  [checkLine entry | entry <- checkEntries state]

checksSummaryWithRefusals :: [(Text, FocusedSetupIssue)] -> CheckState -> Text
checksSummaryWithRefusals [] state = checksSummary state
checksSummaryWithRefusals refused state =
  "focused checks incomplete: " <> Text.pack (show (length refused))
    <> " requested check(s) did not start; "
    <> Text.intercalate "; "
      [name <> ": start refused (" <> Text.pack (show issue) <> ")"
        | (name, issue) <- refused]
    <> "; " <> checksSummary state

verdictText :: CheckVerdict -> Text
verdictText CheckPassed = "passed"
verdictText CheckFailed = "failed"
verdictText CheckUnknown = "unknown"
