{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

module Tidepool.Command
  ( Commands,
    Command,
    Job,
    Memory (..),
    RunResult (..),
    OutputIssue (..),
    DecodeIssue (..),
    bash,
    bashCommand,
    argv,
    describe,
    withMemory,
    inDirectory,
    withEnvironment,
    withArguments,
    withStdin,
    withTerminal,
    start,
    run,
    await,
    Observation (..),
    observe,
    quiet,
    job,
    status,
    stdout,
    readStdout,
    decodeWith,
    asJSON,
    OutputPage,
    CommandStream (..),
    output,
    next,
    readOutput,
    readPage,
    CommandPosition (..),
    tailOutput,
    nextPage,
    pageText,
    pageDetails,
    sendInput,
    closeInput,
    resize,
    cancel,
    completion,
    CommandStatus (..),
    CommandResult (..),
    CommandOutcome (..),
    CommandCleanup (..),
    CommandOutput (..),
    CommandError (..),
    CommandSpec (..),
    CommandInput (..),
    CommandPage (..),
  )
where

import Control.Monad.Freer (Eff, Member, interpose, send)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Tidepool.Actor.Record as R
import Tidepool.Aeson.FromJSON (FromJSON, eitherDecode)
import Tidepool.Command.Types
import Tidepool.Effects.Core
  ( CommandCleanup (..),
    CommandError (..),
    CommandInput (..),
    CommandObservation (..),
    CommandOutcome (..),
    CommandOutput (..),
    CommandPage (..),
    CommandPosition (..),
    CommandPresentation (..),
    CommandResult (..),
    CommandSpec (..),
    CommandStatus (..),
    CommandStream (..),
    Commands (..),
  )
import Tidepool.Inspection (Display (..), WorkbenchDisplay (..), renderText)
import Tidepool.QQ.Bash (bash)

data RunResult
  = Finished {completedJob :: Job, commandResult :: CommandResult, capturedOutput :: CommandOutput}
  deriving (Eq, Show)

-- | Bounded observation, independent of the lifetime of the process.
data Observation = Observation {waitMilliseconds :: Int, outputBytes :: Int}
  deriving (Eq, Show)

data OutputIssue
  = StillRunning Job
  | Unsuccessful CommandOutcome
  | IncompleteStdout Job
  | InvalidOutputEncoding Job
  | OutputUnavailable Job CommandError
  deriving (Eq, Show)

data DecodeIssue e = OutputProblem OutputIssue | DecodeProblem e
  deriving (Eq, Show)

data OutputPage = OutputPage {pageJob :: Job, pageStream :: CommandStream, pageDetails :: CommandPage}
  deriving (Eq, Show)

checked :: Either CommandError a -> a
checked = either (error . show) id

start :: (Member Commands effects) => Command -> Eff effects Job
start (Command spec) = Job . checked <$> send (CommandStartWith spec)

-- | Run and observe for up to 30 seconds, returning a completed result.
-- If still running, the interactive workbench stops the enclosing computation
-- and installs a retained Job binding. A Haskell handler instead fails through
-- its normal supervision boundary. Neither observation deadline cancels the job.
run :: (Member Commands effects) => Command -> Eff effects RunResult
run command = start command >>= await

-- | Observe the same job for up to 30 seconds. A foreground handoff never resumes
-- an earlier block; subsequent calls observe only the retained command.
await :: (Member Commands effects) => Job -> Eff effects RunResult
await retained@(Job key) = do
  observation <- checked <$> send (CommandForegroundWith key)
  let result = Finished retained (observedCommandResult observation) (observedCommandOutput observation)
  send (CommandPresentWith key (CommandVisible (resultHeading (commandResult result) <> " · session_id: " <> key) 65536))
  pure result

-- | Wait briefly and display newly available output, retaining the same job.
-- Unlike foreground 'await', an observation deadline returns the live status
-- normally, so authored handlers can continue composing effects.
observe :: (Member Commands effects) => Observation -> Job -> Eff effects CommandStatus
observe Observation {waitMilliseconds = milliseconds, outputBytes = bytes} (Job key) = do
  current <- checked <$> send (CommandAwaitWith key milliseconds)
  let heading = case current of
        CommandFinished result -> resultHeading result
        _ -> T.pack (show current)
  send (CommandPresentWith key (CommandVisible (heading <> " · session_id: " <> key) bytes))
  pure current

-- | Suppress routine command output within this computation, without changing
-- command execution, retained results, or necessary background-handoff receipts.
quiet :: (Member Commands effects) => Eff effects a -> Eff effects a
quiet = interpose $ \case
  CommandPresentWith key _ -> send (CommandPresentWith key CommandQuiet)
  request -> send request

job :: RunResult -> Job
job Finished {completedJob = retained} = retained

-- | Pure successful, complete stdout extraction. Cleanup is a separate obligation.
stdout :: RunResult -> Either OutputIssue Text
stdout result@Finished {commandResult = outcome, capturedOutput = captured} =
  case commandOutcome outcome of
    CommandExited 0 ->
      let text = commandStdout captured
       in if outputLossy text
            then Left (InvalidOutputEncoding (job result))
            else
              if outputStart text /= 0 || outputEnd text /= outputAvailableEnd text || outputLostBytes text /= 0 || not (outputFinished text)
                then Left (IncompleteStdout (job result))
                else Right (outputText text)
    other -> Left (Unsuccessful other)

-- | Read complete retained stdout without starting or waiting for execution.
readStdout :: (Member Commands effects) => Job -> Eff effects (Either OutputIssue Text)
readStdout retained@(Job key) = do
  observed <- send (CommandStatusWith key)
  case observed of
    Left failure -> pure (Left (OutputUnavailable retained failure))
    Right (CommandFinished result) -> case commandOutcome result of
      CommandExited 0 -> collect 0 []
      other -> pure (Left (Unsuccessful other))
    Right _ -> pure (Left (StillRunning retained))
  where
    collect cursor chunks = do
      observed <- send (CommandReadWith key Stdout (OutputOffset cursor))
      case observed of
        Left failure -> pure (Left (OutputUnavailable retained failure))
        Right page
          | outputLossy page -> pure (Left (InvalidOutputEncoding retained))
          | outputStart page /= cursor || outputLostBytes page /= 0 -> pure (Left (IncompleteStdout retained))
          | outputEnd page == outputAvailableEnd page && outputFinished page ->
              pure (Right (T.concat (reverse (outputText page : chunks))))
          | outputEnd page <= cursor -> pure (Left (IncompleteStdout retained))
          | otherwise -> collect (outputEnd page) (outputText page : chunks)

decodeWith :: (Text -> Either e a) -> Either OutputIssue Text -> Either (DecodeIssue e) a
decodeWith _ (Left issue) = Left (OutputProblem issue)
decodeWith decode (Right text) = either (Left . DecodeProblem) Right (decode text)

asJSON :: (FromJSON a) => Text -> Either Text a
asJSON = eitherDecode

status :: (Member Commands effects) => Job -> Eff effects CommandStatus
status (Job key) = checked <$> send (CommandStatusWith key)

-- | Navigate retained stdout from its beginning; reading never executes again.
output :: (Member Commands effects) => Job -> Eff effects OutputPage
output = readOutput Stdout

next :: (Member Commands effects) => OutputPage -> Eff effects OutputPage
next = nextPage

readOutput :: (Member Commands effects) => CommandStream -> Job -> Eff effects OutputPage
readOutput stream retained = readPage retained stream OutputBeginning

tailOutput :: (Member Commands effects) => CommandStream -> Job -> Eff effects OutputPage
tailOutput stream retained = readPage retained stream OutputTail

nextPage :: (Member Commands effects) => OutputPage -> Eff effects OutputPage
nextPage OutputPage {pageJob = retained, pageStream = stream, pageDetails = details} =
  readPage retained stream (OutputOffset (outputEnd details))

readPage :: (Member Commands effects) => Job -> CommandStream -> CommandPosition -> Eff effects OutputPage
readPage retained@(Job key) stream position =
  OutputPage retained stream . checked <$> send (CommandReadWith key stream position)

pageText :: OutputPage -> Text
pageText = outputText . pageDetails

sendInput :: (Member Commands effects) => Job -> Text -> Eff effects ()
sendInput (Job key) text = checked <$> send (CommandInputWith key text)

closeInput :: (Member Commands effects) => Job -> Eff effects ()
closeInput (Job key) = checked <$> send (CommandCloseInputWith key)

resize :: (Member Commands effects) => Job -> Int -> Int -> Eff effects ()
resize (Job key) rows columns = checked <$> send (CommandResizeWith key rows columns)

cancel :: (Member Commands effects) => Job -> Eff effects ()
cancel (Job key) = checked <$> send (CommandCancelWith key)

completion :: Job -> R.EventSource CommandResult
completion = R.command

instance WorkbenchDisplay RunResult where
  workbenchDisplay = displayWith 65536
  workbenchDisplayWithout keys = displayWithout keys 65536

instance WorkbenchDisplay OutputPage where
  workbenchDisplay = displayWith 65536

instance WorkbenchDisplay CommandOutput where
  workbenchDisplay = displayWith 65536

instance Display RunResult where
  displayWithout keys budget result@Finished {completedJob = Job key, commandResult = outcome}
    | key `elem` keys = renderText budget (resultHeading outcome <> " · output retained")
    | otherwise = displayWith budget result
  displayWith budget result = case result of
    Finished {commandResult = outcome, capturedOutput = captured} ->
      let heading = resultHeading outcome <> "\n"
          (body, omitted) = displayOutput (max 0 (budget - T.length heading)) captured
          (text, clipped) = renderText budget (heading <> body)
       in (text, omitted || clipped)

instance Display OutputPage where
  displayWith budget OutputPage {pageStream = stream, pageDetails = details} =
    renderText budget (outputHeading (T.pack (show stream)) details)

instance Display CommandOutput where
  displayWith = displayOutput

instance Display CommandPage where
  displayWith budget = renderText budget . outputHeading "output"

displayOutput :: Int -> CommandOutput -> (Text, Bool)
displayOutput budget captured =
  let out = commandStdout captured
      err = commandStderr captured
      hasError = not (T.null (outputText err)) || outputAvailableEnd err > 0
      outBudget = if hasError then budget `div` 2 else budget
      (a, omittedA) = diagnostic outBudget "stdout" out
      (b, omittedB) = if hasError then diagnostic (max 0 (budget - T.length a)) "stderr" err else ("", False)
   in (a <> b, omittedA || omittedB)
  where
    diagnostic limit stream details =
      let full = outputHeading stream details
       in if T.length full <= limit
            then (full, False)
            else
              let marker = outputMetadata stream details <> " · display tail; full capture retained\n"
                  allowance = max 0 (limit - T.length marker)
                  (text, _) = renderText limit (marker <> T.takeEnd allowance (outputText details))
               in (text, True)

resultHeading :: CommandResult -> Text
resultHeading result =
  "Finished · "
    <> T.pack (show (commandOutcome result))
    <> case commandCleanup result of
      CommandClean -> ""
      other -> " · cleanup: " <> T.pack (show other)

outputHeading :: Text -> CommandPage -> Text
outputHeading stream page = outputMetadata stream page <> "\n" <> outputText page <> "\n"

outputMetadata :: Text -> CommandPage -> Text
outputMetadata stream page =
  stream
    <> " · bytes "
    <> number (outputStart page)
    <> "–"
    <> number (outputEnd page)
    <> " of "
    <> number (outputAvailableEnd page)
    <> (if outputRetainedStart page > 0 then " · retention loss before byte " <> number (outputRetainedStart page) else "")
    <> (if outputLostBytes page > 0 then " · retention gap: " <> number (outputLostBytes page) <> " bytes" else "")
    <> (if outputStart page > outputRetainedStart page && outputLostBytes page == 0 then " · earlier available output not in capture; read from job" else "")
    <> (if outputEnd page < outputAvailableEnd page then " · more available" else if outputFinished page then " · EOF" else " · current end; running")
    <> (if outputLossy page then " · lossy UTF-8" else "")
    <> (if outputLeadingFragment page then " · leading line fragment" else "")
    <> (if outputTrailingFragment page then " · trailing line fragment" else "")
  where
    number = T.pack . show

instance Display OutputIssue where
  displayWith budget issue = renderText budget $ case issue of
    IncompleteStdout retained ->
      "Command finished; this capture is incomplete. Awaiting again does not enlarge it. Use Cmd.readStdout with your existing job binding, or Cmd.job applied to your result; Cmd.output navigates retained output. Retention gaps are explicit. Job: " <> T.pack (show retained)
    StillRunning retained ->
      "Command still running: " <> T.pack (show retained) <> ". Continue observing the same job with Cmd.await."
    other -> T.pack (show other)
