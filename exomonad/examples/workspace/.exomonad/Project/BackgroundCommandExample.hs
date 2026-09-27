{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | A small completion watcher for one retained command job.
--
-- The staged notebook sequence, including its exact imports, lives in
-- @checks/background-command-example.hs@.
module Project.BackgroundCommandExample
  ( CompletionEvidence (..)
  , CompletionProjection (..)
  , CaptureState (..)
  , CommandWatcher
  , startCommandWatcher
  , readCommandEvidence
  , readCommandProjection
  , finishCommandWatcher
  ) where

import Control.Monad.Freer (Eff, Member)
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (Actor, Commands)
import Tidepool.Effects.Row (knownEffects)

data CompletionEvidence = CompletionEvidence
  { completionReceipt :: Cmd.CommandResult
  , completionCapture :: Either Cmd.OutputIssue Cmd.Capture
  } deriving (Show)

data CaptureState
  = NotCaptured Cmd.OutputIssue
  | CompleteCapture
  | PartialCapture
  | RefusedCapture
  deriving (Eq, Show)

data CompletionProjection = CompletionProjection
  { projectedJob :: Cmd.Job
  , projectedOutcome :: Cmd.CommandOutcome
  , projectedCleanup :: Cmd.CommandCleanup
  , projectedStdout :: CaptureState
  , projectedStderr :: CaptureState
  } deriving (Eq, Show)

data CommandWatchState = CommandWatchState
  { originalJob :: Cmd.Job
  , retainedEvidence :: Maybe CompletionEvidence
  } deriving (Show)

data CommandWatcher mode = CommandWatcher
  { watcherState :: mode :- State CommandWatchState
  , watcherSnapshot :: mode :- Call () (R.Reply CommandWatchState)
  , watcherCompletion :: mode :- Event Cmd.CommandResult
  } deriving Generic

type WatcherEffects = R.LocalEffects CommandWatcher '[Replies, Actor, Commands]

-- | Attach to the existing job. A late attachment receives its retained
-- completion; this function never starts or restarts a command.
startCommandWatcher
  :: Member Actor effects
  => Cmd.Job
  -> Eff effects (R.ActorHandle CommandWatcher)
startCommandWatcher job = R.start specification
  where
    specification :: ActorSpec CommandWatcher WatcherEffects
    specification = R.definition "background-command-example"
      (Actor.Selected knownEffects) CommandWatcher
        { watcherState = CommandWatchState job Nothing
        , watcherSnapshot = \() -> R.get
        , watcherCompletion = R.on (Cmd.completion job) $ \receipt -> do
            capture <- Cmd.readCommand job
            R.modify' (\state -> state
              { retainedEvidence = Just (CompletionEvidence receipt capture) })
        }

readCommandEvidence
  :: Member Actor effects
  => R.ActorHandle CommandWatcher
  -> Eff effects (Maybe CompletionEvidence)
readCommandEvidence watcher =
  fmap retainedEvidence (R.call (watcherSnapshot (R.client watcher)) ())

readCommandProjection
  :: Member Actor effects
  => R.ActorHandle CommandWatcher
  -> Eff effects (Maybe CompletionProjection)
readCommandProjection watcher = do
  state <- R.call (watcherSnapshot (R.client watcher)) ()
  pure (completionProjection (originalJob state) <$> retainedEvidence state)

finishCommandWatcher
  :: Member Actor effects
  => R.ActorHandle CommandWatcher
  -> Eff effects (Actor.ActorExit (Maybe CompletionEvidence))
finishCommandWatcher watcher = do
  exit <- R.finish watcher
  pure $ case exit of
    Actor.Completed state -> Actor.Completed (retainedEvidence state)
    Actor.Failed failure -> Actor.Failed failure
    Actor.Cancelled reason -> Actor.Cancelled reason

completionProjection :: Cmd.Job -> CompletionEvidence -> CompletionProjection
completionProjection job evidence = CompletionProjection
  { projectedJob = job
  , projectedOutcome = Cmd.commandOutcome (completionReceipt evidence)
  , projectedCleanup = Cmd.commandCleanup (completionReceipt evidence)
  , projectedStdout = captureState (fmap Cmd.capturedStdout (completionCapture evidence))
  , projectedStderr = captureState (fmap Cmd.capturedStderr (completionCapture evidence))
  }

captureState :: Either Cmd.OutputIssue Cmd.StreamCapture -> CaptureState
captureState (Left issue) = NotCaptured issue
captureState (Right Cmd.CaptureComplete {}) = CompleteCapture
captureState (Right Cmd.CapturePartial {}) = PartialCapture
captureState (Right Cmd.CaptureRefused {}) = RefusedCapture
