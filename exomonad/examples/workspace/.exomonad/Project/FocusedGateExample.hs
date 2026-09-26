{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | A small consumer of the focused-check workflow. Call 'startGate' in the
-- actor whose checkout contains the source. The notice carries the compact
-- terminal facts; call 'readChecks' and 'foldGate' only when a decision needs
-- the retained typed result.
module Project.FocusedGateExample
  ( GateStart (..), startGate, startPreparedGate, reopenGate, readGate, foldGate
  , CheckPreparation (..), PlanCheck (..), PlanStart (..), PlanReport (..)
  , startCheckPlan, readCheckPlan, planPassed, planSummary
  ) where

import Control.Monad (forM, forM_)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Project.CheckResults
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad (AgentRef)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Actor, Commands)
import Tidepool.Worktree (GitOid)

data GateStart
  = GateSetupRefused FocusedSetupIssue
  | GateWatchRefused FocusedRun CheckSetupIssue
  | GateWatching FocusedRun (R.ActorHandle CheckActor)
  deriving (Show)

-- The command embeds its own artifact before exit, so a root actor and a
-- child bound to a managed checkout use the same evidence path: the original
-- job's terminal output. The watcher never opens a second actor's files.
startGate
  :: (Member Actor effects, Member Commands effects)
  => AgentRef -> Text -> Cmd.Memory -> FocusedSpec -> Eff effects GateStart
startGate owner name memory spec = do
  startFocused memory spec >>= attachGate owner name

-- One original job retains prerequisite and test evidence in the owner checkout.
startPreparedGate
  :: (Member Actor effects, Member Commands effects)
  => AgentRef -> Text -> Cmd.Memory -> FocusedSpec -> [Text] -> Eff effects GateStart
startPreparedGate owner name memory spec preparation =
  startFocusedAfter memory spec preparation >>= attachGate owner name

-- | Reattach to the original job after a watcher binding was lost. A retained
-- 'FocusedRun' (or its spec, job and constructor) is required. An old watcher
-- may still be live and send a second notice; use 'collectFocused' instead if
-- only the original terminal result is needed. No command is submitted here.
reopenGate :: Member Actor effects => AgentRef -> Text -> FocusedRun -> Eff effects GateStart
reopenGate owner name run = attachGate owner name (Right run)

attachGate :: Member Actor effects => AgentRef -> Text -> Either FocusedSetupIssue FocusedRun -> Eff effects GateStart
attachGate owner name started =
  case started of
    Left issue -> pure (GateSetupRefused issue)
    Right run -> do
      watched <- watchChecks owner NotifyAllTerminal [(name, run)]
      pure $ case watched of
        Left issue -> GateWatchRefused run issue
        Right watcher -> GateWatching run watcher

readGate
  :: Member Actor effects
  => R.ActorHandle CheckActor
  -> (CheckEntry -> CheckOutcome -> Eff effects ())
  -> (CheckEntry -> CheckOutcome -> Eff effects ())
  -> Eff effects (Maybe Text)
readGate watcher onFailed onUnknown =
  readChecks watcher >>= foldGate onFailed onUnknown

-- Use after a completion notice. The callbacks can inspect the original
-- receipt, source assurance and evidence; neither changes the pass rule.
-- Pending state remains explicit if a notice has not settled the watcher yet.
foldGate
  :: Monad m
  => (CheckEntry -> CheckOutcome -> m ())
  -> (CheckEntry -> CheckOutcome -> m ())
  -> CheckState -> m (Maybe Text)
foldGate onFailed onUnknown state
  | any (maybe True (const False) . checkOutcome) entries = pure Nothing
  | otherwise = do
      forM_ entries $ \entry -> case checkOutcome entry of
        Nothing -> pure ()
        Just outcome -> case checkVerdict entry outcome of
          CheckPassed -> pure ()
          CheckFailed -> onFailed entry outcome
          CheckUnknown -> onUnknown entry outcome
      pure (Just (checksSummary state))
  where entries = checkEntries state

-- | Each check builds its spec and optional preparation from the invocation's
-- candidate. The job and resource reservation remain owned by Commands.
data CheckPreparation = WithoutPreparation | PrepareWith (GitOid -> [Text])

data PlanCheck = PlanCheck
  { planName :: Text
  , planSpec :: GitOid -> FocusedSpec
  , planMemory :: Cmd.Memory
  , planPreparation :: CheckPreparation
  }

data PlanStart = PlanStart
  { planStarts :: [(Text, Either FocusedSetupIssue FocusedRun)]
  , planWatcher :: Maybe (Either CheckSetupIssue (R.ActorHandle CheckActor))
  } deriving (Show)

data PlanReport = PlanReport
  { planOriginal :: PlanStart
  , planState :: Maybe CheckState
  } deriving (Show)

-- | Validate names before submission, retain every start result, and attach
-- one aggregate watcher to admitted jobs. A refused start is never retried.
startCheckPlan
  :: (Member Actor effects, Member Commands effects)
  => AgentRef -> GitOid -> [PlanCheck]
  -> Eff effects (Either CheckSetupIssue PlanStart)
startCheckPlan owner candidate checks = case checks of
  [] -> pure (Left NoFocusedChecks)
  _ | Just name <- duplicateName names -> pure (Left (DuplicateCheckName name))
  _ -> do
    launched <- forM checks $ \item -> do
      let spec = planSpec item candidate
      result <- case planPreparation item of
        WithoutPreparation -> startFocused (planMemory item) spec
        PrepareWith commandFor -> startFocusedAfter (planMemory item) spec (commandFor candidate)
      pure (planName item, result)
    let admitted = [(name, run) | (name, Right run) <- launched]
        refused = [(name, issue) | (name, Left issue) <- launched]
    watcher <- case admitted of
      [] -> pure Nothing
      _ -> Just <$> watchChecksWithRefusals owner NotifySummary refused admitted
    pure (Right (PlanStart launched watcher))
  where names = map planName checks

duplicateName :: [Text] -> Maybe Text
duplicateName [] = Nothing
duplicateName (name : rest)
  | name `elem` rest = Just name
  | otherwise = duplicateName rest

readCheckPlan :: Member Actor effects => PlanStart -> Eff effects PlanReport
readCheckPlan started = do
  state <- case planWatcher started of
    Just (Right watcher) -> Just <$> readChecks watcher
    _ -> pure Nothing
  pure (PlanReport started state)

-- | Every requested check must have started, completed and passed.
planPassed :: PlanReport -> Bool
planPassed report = case planWatcher (planOriginal report) of
  Just (Right _) -> not (null statuses) && all (== Just CheckPassed) statuses
  _ -> False
  where statuses = map (snd . planStatus report) (planStarts (planOriginal report))

planSummary :: PlanReport -> Text
planSummary report =
  "check plan " <> (if planPassed report then "passed" else "not passed")
    <> (if any (either (const True) (const False) . snd)
          (planStarts (planOriginal report))
        then " (not all requested checks ran)" else "")
    <> ": " <> Text.intercalate "; "
      [case result of
        Left issue -> name <> ": start refused (" <> Text.pack (show issue) <> ")"
        Right _ -> case planState report of
          Nothing -> name <> ": running or unavailable"
          Just state -> case [entry | entry <- checkEntries state, checkName entry == name] of
            entry : _ -> checkLine entry
            [] -> name <> ": running or unavailable"
      | (name, result) <- planStarts (planOriginal report)
      ]
    <> case planWatcher (planOriginal report) of
      Just (Left issue) -> "; watcher refused (" <> Text.pack (show issue) <> ")"
      _ -> ""

planStatus :: PlanReport -> (Text, Either FocusedSetupIssue FocusedRun) -> (Text, Maybe CheckVerdict)
planStatus report (name, result) = (name, case (result, planState report) of
  (Right _, Just state) -> case [entry | entry <- checkEntries state, checkName entry == name] of
    entry : _ -> checkVerdict entry <$> checkOutcome entry
    [] -> Nothing
  _ -> Nothing)
