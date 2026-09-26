{-# LANGUAGE FlexibleContexts #-}

-- | Verify one retained preparation job when its completion event arrives,
-- then return a caller-defined readiness value for ordinary Haskell composition.
module Project.PrepareContinue
  ( PreparationFailure (..), verifyPrepared
  ) where

import Control.Monad.Freer (Eff, Member)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)

data PreparationFailure issue
  = PreparationNotTerminal Cmd.Job Cmd.CommandStatus
  | PreparationReceiptMismatch Cmd.CommandResult Cmd.CommandResult Cmd.RunResult
  | PreparationCommandFailed Cmd.RunResult
  | PreparationReadinessFailed issue Cmd.RunResult
  deriving (Show)

-- Use this in @R.on (Cmd.completion job)@. The readiness function must
-- establish the caller's actual prerequisite (source, artifact, service, or
-- other proof). It receives the exact run result and returns a typed value
-- for the next step. The status check proves terminal before the short
-- foreground read, so a long job never enters the 30-second handoff path.
verifyPrepared
  :: Member Commands effects
  => Cmd.Job
  -> Cmd.CommandResult
  -> (Cmd.RunResult -> Eff effects (Either issue a))
  -> Eff effects (Either (PreparationFailure issue) a)
verifyPrepared job receipt readiness = do
  status <- Cmd.status job
  case status of
    Cmd.CommandFinished observed -> do
      result <- Cmd.quiet (Cmd.await job)
      if observed /= receipt || Cmd.commandResult result /= receipt
        then pure (Left (PreparationReceiptMismatch receipt observed result))
        else case (Cmd.commandOutcome receipt, Cmd.commandCleanup receipt) of
          (Cmd.CommandExited 0, Cmd.CommandClean) -> do
            ready <- readiness result
            pure (either
              (\issue -> Left (PreparationReadinessFailed issue result))
              Right
              ready)
          _ -> pure (Left (PreparationCommandFailed result))
    other -> pure (Left (PreparationNotTerminal job other))
