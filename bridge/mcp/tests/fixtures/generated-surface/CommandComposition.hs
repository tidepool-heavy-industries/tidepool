{-# LANGUAGE FlexibleContexts #-}
module CommandComposition where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Tidepool.Command as Cmd
import Tidepool.Command (Commands)

argv :: [Text] -> Cmd.Command
argv = Cmd.argv
bashCommand :: Text -> Cmd.Command
bashCommand = Cmd.bashCommand
withMemory :: Cmd.Memory -> Cmd.Command -> Cmd.Command
withMemory = Cmd.withMemory
inDirectory :: Text -> Cmd.Command -> Cmd.Command
inDirectory = Cmd.inDirectory
withEnvironment :: [(Text, Text)] -> Cmd.Command -> Cmd.Command
withEnvironment = Cmd.withEnvironment
run :: forall effs. Member Commands effs => Cmd.Command -> Eff effs Cmd.RunResult
run = Cmd.run
start :: forall effs. Member Commands effs => Cmd.Command -> Eff effs Cmd.Job
start = Cmd.start
await :: forall effs. Member Commands effs => Cmd.Job -> Eff effs Cmd.RunResult
await = Cmd.await
job :: Cmd.RunResult -> Cmd.Job
job = Cmd.job
stdout :: Cmd.RunResult -> Either Cmd.OutputIssue Text
stdout = Cmd.stdout
failure :: Cmd.RunResult -> Maybe Text
failure = Cmd.failure
readStdout :: forall effs. Member Commands effs => Cmd.Job -> Eff effs (Either Cmd.OutputIssue Text)
readStdout = Cmd.readStdout
observe :: forall effs. Member Commands effs => Cmd.Observation -> Cmd.Job -> Eff effs Cmd.CommandStatus
observe = Cmd.observe
output :: forall effs. Member Commands effs => Cmd.Job -> Eff effs Cmd.OutputPage
output = Cmd.output
pageText :: Cmd.OutputPage -> Text
pageText = Cmd.pageText
next :: forall effs. Member Commands effs => Cmd.OutputPage -> Eff effs Cmd.OutputPage
next = Cmd.next
cancel :: forall effs. Member Commands effs => Cmd.Job -> Eff effs ()
cancel = Cmd.cancel

example :: forall effs. Member Commands effs => Eff effs (Maybe Text, Either Cmd.OutputIssue Text)
example = do
  result <- Cmd.run (Cmd.withMemory (Cmd.MiB 256) (Cmd.argv ["printf", "hello\n"]))
  pure (Cmd.failure result, Cmd.stdout result)
