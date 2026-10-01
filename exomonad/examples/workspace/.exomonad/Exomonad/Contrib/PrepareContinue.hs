{-# LANGUAGE FlexibleContexts #-}

-- | Await preparation and establish caller-defined readiness in the same
-- pending tool continuation. Readiness is separate from command completion.
module Exomonad.Contrib.PrepareContinue
  ( PreparationFailure (..), awaitPrepared, verifyPrepared
  ) where

import Control.Monad.Freer (Eff, Member)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands)

data PreparationFailure issue
  = PreparationReceiptMismatch Cmd.CommandResult Cmd.RunResult
  | PreparationCommandFailed Cmd.RunResult
  | PreparationReadinessFailed issue Cmd.RunResult
  deriving (Show)

awaitPrepared
  :: Member Commands effects
  => Cmd.Job -> (Cmd.RunResult -> Eff effects (Either issue value))
  -> Eff effects (Either (PreparationFailure issue) value)
awaitPrepared job readiness = do
  result <- Cmd.quiet (Cmd.await job)
  establishReady result readiness

-- | For an existing completion-source consumer, verify that its event and
-- retained job refer to the same terminal receipt before establishing readiness.
verifyPrepared
  :: Member Commands effects
  => Cmd.Job -> Cmd.CommandResult
  -> (Cmd.RunResult -> Eff effects (Either issue value))
  -> Eff effects (Either (PreparationFailure issue) value)
verifyPrepared job receipt readiness = do
  result <- Cmd.quiet (Cmd.await job)
  if Cmd.commandResult result /= receipt
    then pure (Left (PreparationReceiptMismatch receipt result))
    else establishReady result readiness

establishReady
  :: Member Commands effects
  => Cmd.RunResult -> (Cmd.RunResult -> Eff effects (Either issue value))
  -> Eff effects (Either (PreparationFailure issue) value)
establishReady result readiness = case
  (Cmd.commandOutcome (Cmd.commandResult result), Cmd.commandCleanup (Cmd.commandResult result)) of
    (Cmd.CommandExited 0, Cmd.CommandClean) -> do
      ready <- readiness result
      pure (either (\issue -> Left (PreparationReadinessFailed issue result)) Right ready)
    _ -> pure (Left (PreparationCommandFailed result))
