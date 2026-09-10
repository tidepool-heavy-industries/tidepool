{-# LANGUAGE FlexibleContexts #-}
module Tidepool.Command
  ( Commands, Command, Job, Memory (..), RunResult (..)
  , bash, argv, describe, withMemory, inDirectory, withEnvironment, withArguments, withStdin, withTerminal
  , start, run, await, status, output, sendInput, closeInput, resize, cancel, completion
  , CommandStatus (..), CommandResult (..), CommandOutcome (..), CommandCleanup (..), CommandOutput (..), CommandError (..)
  , CommandSpec (..), CommandInput (..)
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import qualified Tidepool.Actor.Record as R
import Tidepool.Command.Types
import Tidepool.QQ.Bash (bash)
import Tidepool.Effects.Core
  ( Commands (..), CommandError (..), CommandStatus (..), CommandResult (..)
  , CommandOutcome (..), CommandCleanup (..), CommandOutput (..), CommandSpec (..), CommandInput (..)
  )

data RunResult
  = Finished
      { completedJob :: Job
      , commandResult :: CommandResult
      , capturedOutput :: CommandOutput
      }
  | Pending Job
  | Unavailable Job CommandError
  deriving (Eq, Show)

checked :: Either CommandError a -> a
checked = either (error . show) id

start :: Member Commands effects => Command -> Eff effects Job
start (Command spec) = Job . checked <$> send (CommandStartWith spec)

-- | Wait up to one second for completion; retain the job if observation fails.
run :: Member Commands effects => Command -> Eff effects RunResult
run command = do
  job@(Job key) <- start command
  observed <- send (CommandAwaitWith key 1000)
  case observed of
    Left failure -> pure (Unavailable job failure)
    Right (CommandFinished result) -> do
      captured <- send (CommandOutputWith key 8192)
      pure $ either (Unavailable job) (Finished job result) captured
    Right _ -> pure (Pending job)

await :: Member Commands effects => Job -> Eff effects CommandResult
await (Job key) = do
  observed <- checked <$> send (CommandAwaitWith key (-1))
  case observed of
    CommandFinished result -> pure result
    _ -> error "command owner returned a nonterminal result to await"

status :: Member Commands effects => Job -> Eff effects CommandStatus
status (Job key) = checked <$> send (CommandStatusWith key)

-- | Read a bounded tail; truncation is explicit in the result.
output :: Member Commands effects => Job -> Int -> Eff effects CommandOutput
output (Job key) bytes = checked <$> send (CommandOutputWith key bytes)

sendInput :: Member Commands effects => Job -> Text -> Eff effects ()
sendInput (Job key) text = checked <$> send (CommandInputWith key text)

closeInput :: Member Commands effects => Job -> Eff effects ()
closeInput (Job key) = checked <$> send (CommandCloseInputWith key)

resize :: Member Commands effects => Job -> Int -> Int -> Eff effects ()
resize (Job key) rows columns = checked <$> send (CommandResizeWith key rows columns)

cancel :: Member Commands effects => Job -> Eff effects ()
cancel (Job key) = checked <$> send (CommandCancelWith key)

completion :: Job -> R.EventSource CommandResult
completion = R.command
