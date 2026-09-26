{-# LANGUAGE FlexibleContexts #-}

-- | A small consumer of the focused-check workflow. Call 'startGate' in the
-- actor whose checkout contains the source. The notice carries the compact
-- terminal facts; call 'readChecks' and 'foldGate' only when a decision needs
-- the retained typed result.
module Project.FocusedGateExample
  ( GateStart (..), startGate, readGate, foldGate
  ) where

import Control.Monad (forM_)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Project.CheckResults
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad (AgentRef)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Actor, Commands)

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
  started <- startFocused memory spec
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
