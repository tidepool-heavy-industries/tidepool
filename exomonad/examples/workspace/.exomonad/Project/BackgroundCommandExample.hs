{-# LANGUAGE FlexibleContexts #-}

-- | Await one retained command job and project its execution evidence.
--
-- The staged notebook sequence, including its exact imports, lives in
-- @checks/background-command-example.hs@.
module Project.BackgroundCommandExample
  ( CompletionEvidence (..), CompletionProjection (..), CaptureState (..)
  , awaitCommandEvidence, completionProjection
  ) where

import Control.Monad.Freer (Eff, Member)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)

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

-- | Suspend this tool continuation on the original job, then read its
-- retained output. The command owner retains evidence independently of cells.
awaitCommandEvidence :: Member Commands effects => Cmd.Job -> Eff effects CompletionEvidence
awaitCommandEvidence job = do
  result <- Cmd.quiet (Cmd.await job)
  capture <- Cmd.readCommand job
  pure (CompletionEvidence (Cmd.commandResult result) capture)

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
