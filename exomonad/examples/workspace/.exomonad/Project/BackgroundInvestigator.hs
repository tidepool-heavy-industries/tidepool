{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | One completion-driven investigation of an existing focused command.
module Project.BackgroundInvestigator
  ( Investigator (investigationSnapshot)
  , InvestigationState (..)
  , InvestigationChoice
  , InvestigationFinishIssue (..)
  , watchFailedCheck, readInvestigation, finishInvestigation
  , investigationSummary
  ) where

import Control.Monad (forM_)
import Control.Monad.Freer (Eff, Member, raise)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import Project.ParallelInvestigate
import Project.TestEvidence
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Actor, Commands, Jev, Notifications)
import Tidepool.Effects.Row (knownEffects)

data InvestigationState = InvestigationState
  { investigationRun :: FocusedRun
  , investigationResult :: Maybe FocusedResult
  , investigationAvailable :: [CommandProbe]
  , investigationReport :: Maybe FollowupReport
  , investigationNotice :: Maybe (Either NotificationError NotificationReceipt)
  , investigationFollowers :: [R.ActorHandle ProbeFollower]
  }

instance Show InvestigationState where
  show state = "InvestigationState { original = " ++ show (runJob (investigationRun state))
    ++ ", focused = " ++ show (fmap focusedPassed (investigationResult state))
    ++ ", available = " ++ show (map probeName (investigationAvailable state))
    ++ ", followup = " ++ show (fmap followupStop (investigationReport state))
    ++ ", notice = " ++ show (fmap (either (const "refused") (const "sent")) (investigationNotice state))
    ++ ", followers = " ++ show (length (investigationFollowers state)) ++ " }"

data InvestigationFinishIssue = InvestigationPending Cmd.Job deriving (Show)

data Investigator mode = Investigator
  { investigationState :: mode :- State InvestigationState
  , investigationSnapshot :: mode :- Call () (R.Reply InvestigationState)
  , investigationCompleted :: mode :- Event Cmd.CommandResult
  , investigationResume :: mode :- Call Cmd.Job NoReply
  } deriving Generic

data ProbeFollower mode = ProbeFollower
  { followerState :: mode :- State ()
  , followerCompletion :: mode :- Event Cmd.CommandResult
  } deriving Generic

type InvestigatorEffects = R.LocalEffects Investigator '[Replies, Actor, Notifications, Commands, Jev]
type FollowerEffects = R.LocalEffects ProbeFollower '[Actor]

-- | A project policy chooses one of its own read-only probes or abstains.
-- Jev can be used here; its refusal stays in the typed followup report.
type InvestigationChoice = Text -> [CommandProbe]
  -> Eff InvestigatorEffects (Either ProbeChoiceFailure (Maybe CommandProbe))

-- | Attach to one original job, including a job that already completed.
-- This actor owns the report and final notice. A follower only forwards the
-- completion of a diagnostic job discovered after actor admission.
watchFailedCheck
  :: Member Actor effects
  => AgentRef -> Text -> FocusedRun -> (FocusedResult -> [CommandProbe]) -> InvestigationChoice
  -> Eff effects (R.ActorHandle Investigator)
watchFailedCheck owner intent run selectProbesFor choose = R.start specification
  where
    original = runJob run
    specification :: ActorSpec Investigator InvestigatorEffects
    specification = R.definition "focused-background-investigator"
      (Actor.Selected knownEffects) Investigator
        { investigationState = InvestigationState run Nothing [] Nothing Nothing []
        , investigationSnapshot = \() -> R.get
        , investigationCompleted = R.on (Cmd.completion original) $ \_ -> do
            current <- R.get
            case investigationReport current of
              Just _ -> pure ()
              Nothing -> do
                focused <- collectFocused run
                let probes = selectProbesFor focused
                report <- followFailureWith
                  (\context choices -> raise (choose context choices))
                  intent original probes
                R.modify' (\state -> state
                  { investigationResult = Just focused
                  , investigationAvailable = probes
                  , investigationReport = Just report })
                next report
        , investigationResume = \job -> do
            current <- R.get
            case investigationReport current of
              Just prior | FollowupStillRunning pending <- followupStop prior
                , pending == job -> do
                report <- resumeFollowupWith
                  (\context choices -> raise (choose context choices))
                  intent original (investigationAvailable current) prior
                let settled = case followupStop report of
                      FollowupStillRunning same | same == job ->
                        report { followupStop = FollowupRecoveryMismatch }
                      _ -> report
                R.modify' (\state -> state { investigationReport = Just settled })
                next settled
              _ -> pure ()
        }
    next :: FollowupReport -> R.Handler InvestigationState InvestigatorEffects ()
    next report = case followupStop report of
      FollowupStillRunning job -> do
        self <- R.self @Investigator
        follower <- R.start (followerDefinition job (investigationResume self))
        R.modify' (\state -> state
          { investigationFollowers = investigationFollowers state ++ [follower] })
      _ -> do
        current <- R.get
        case (investigationResult current, investigationNotice current) of
          (Just focused, Nothing) -> do
            receipt <- sendMessage owner (investigationSummary (investigationRun current) focused report)
            R.modify' (\state -> state { investigationNotice = Just receipt })
          _ -> pure ()

followerDefinition :: Cmd.Job -> R.Send Cmd.Job -> ActorSpec ProbeFollower FollowerEffects
followerDefinition job destination =
  R.definition "investigation-probe-completion" (Actor.Selected knownEffects)
    ProbeFollower
      { followerState = ()
      , followerCompletion = R.on (Cmd.completion job) $ \_ -> R.send destination job
      }

readInvestigation :: Member Actor effects => R.ActorHandle Investigator -> Eff effects InvestigationState
readInvestigation actor = R.call (investigationSnapshot (R.client actor)) ()

-- | Finish only after the original and any diagnostics have settled. This
-- retires every follower before the report owner, preserving one authority
-- and making cleanup explicit.
finishInvestigation
  :: Member Actor effects
  => R.ActorHandle Investigator
  -> Eff effects (Either InvestigationFinishIssue (Actor.ActorExit InvestigationState))
finishInvestigation actor = do
  state <- readInvestigation actor
  case investigationReport state of
    Nothing -> pure (Left (InvestigationPending (runJob (investigationRun state))))
    Just report -> case followupStop report of
      FollowupStillRunning job -> pure (Left (InvestigationPending job))
      _ -> do
        forM_ (investigationFollowers state) R.finish
        Right <$> R.finish actor

investigationSummary :: FocusedRun -> FocusedResult -> FollowupReport -> Text
investigationSummary original focused report =
  "focused investigation: "
    <> (if focusedPassed focused then "check passed" else "check not accepted")
    <> "; " <> focusedResultSummary original focused
    <> "; probes " <> Text.intercalate ", " (map probeLine (diagnosticObservations report))
    <> "; stop " <> Text.pack (show (followupStop report))
  where
    probeLine (ProbeStartFailed name issue) = name <> " refused: " <> Text.pack (show issue)
    probeLine (ProbeObserved name _ status _ _) = name <> " " <> Text.pack (show status)
