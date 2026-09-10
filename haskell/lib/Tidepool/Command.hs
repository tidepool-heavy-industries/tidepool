{-# LANGUAGE FlexibleContexts #-}
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
    job,
    status,
    stdout,
    decodeWith,
    asJSON,
    OutputPage,
    CommandStream (..),
    readOutput,
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

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Tidepool.Actor.Record as R
import Tidepool.Aeson.FromJSON (FromJSON, eitherDecode)
import Tidepool.Command.Types
import Tidepool.Effects.Core
  ( CommandCleanup (..),
    CommandError (..),
    CommandInput (..),
    CommandOutcome (..),
    CommandOutput (..),
    CommandPage (..),
    CommandPosition (..),
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
  | Pending Job
  | Unavailable {unavailableJob :: Job, observedResult :: Maybe CommandResult, observationError :: CommandError}
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

-- | Brief observation; a pending or unavailable result retains the same job.
run :: (Member Commands effects) => Command -> Eff effects RunResult
run command = start command >>= observe 1000

-- | Wait for terminal execution, then capture output. Interruption stops the
-- observation, not the independently owned command. Reuse the same Job.
await :: (Member Commands effects) => Job -> Eff effects RunResult
await = observe (-1)

observe :: (Member Commands effects) => Int -> Job -> Eff effects RunResult
observe milliseconds retained@(Job key) = do
  observed <- send (CommandAwaitWith key milliseconds)
  case observed of
    Left failure -> pure (Unavailable retained Nothing failure)
    Right (CommandFinished result) -> do
      captured <- send (CommandOutputWith key 8192)
      pure $ either (Unavailable retained (Just result)) (Finished retained result) captured
    Right _ -> pure (Pending retained)

job :: RunResult -> Job
job Finished {completedJob = retained} = retained
job (Pending retained) = retained
job Unavailable {unavailableJob = retained} = retained

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
stdout (Pending retained) = Left (StillRunning retained)
stdout Unavailable {unavailableJob = retained, observationError = failure} = Left (OutputUnavailable retained failure)

decodeWith :: (Text -> Either e a) -> Either OutputIssue Text -> Either (DecodeIssue e) a
decodeWith _ (Left issue) = Left (OutputProblem issue)
decodeWith decode (Right text) = either (Left . DecodeProblem) Right (decode text)

asJSON :: (FromJSON a) => Text -> Either Text a
asJSON = eitherDecode

status :: (Member Commands effects) => Job -> Eff effects CommandStatus
status (Job key) = checked <$> send (CommandStatusWith key)

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
  workbenchDisplay = displayWith 4096

instance WorkbenchDisplay OutputPage where
  workbenchDisplay = displayWith 8192

instance WorkbenchDisplay CommandOutput where
  workbenchDisplay = displayWith 4096

instance Display RunResult where
  displayWith budget result = case result of
    Finished {commandResult = outcome, capturedOutput = captured} ->
      let heading = resultHeading outcome <> "\n"
          (body, omitted) = displayOutput (max 0 (budget - T.length heading)) captured
          (text, clipped) = renderText budget (heading <> body)
       in (text, omitted || clipped)
    Pending retained -> renderText budget ("Pending · " <> T.pack (show retained) <> " · use Cmd.job / Cmd.await")
    Unavailable {unavailableJob = retained, observedResult = observed, observationError = failure} ->
      renderText budget ("Observation unavailable · " <> T.pack (show retained) <> "\n" <> T.pack (show observed) <> "\n" <> T.pack (show failure))

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
