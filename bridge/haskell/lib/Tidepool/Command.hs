{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE UndecidableInstances #-}

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
    tryStart,
    run,
    await,
    Observation (..),
    PresentedObservation (..),
    observe,
    observeWith,
    quiet,
    job,
    status,
    stdout,
    stderr,
    failure,
    renderCommandError,
    readStdout,
    readStderr,
    readCommand,
    Capture (..),
    StreamCapture (..),
    decodeWith,
    asJSON,
    OutputPage,
    CommandStream (..),
    output,
    next,
    readOutput,
    readPage,
    tryPage,
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
import Tidepool.Inspection
  ( Display (..),
    DisplayPage,
    DisplayTree (..),
    PageDisplay (..),
    WorkbenchDisplay (..),
    pageWithContinuation,
    rawText,
  )
import Tidepool.QQ.Bash (bash)

data RunResult
  = Finished {completedJob :: Job, commandResult :: CommandResult, capturedOutput :: CommandOutput}
  deriving (Eq, Show)

-- | Bounded observation, independent of the lifetime of the process.
data Observation = Observation {waitMilliseconds :: Int, outputBytes :: Int}
  deriving (Eq, Show)

-- | One immutable view taken after an observation wait.  A custom command
-- presenter can inspect the endpoints in 'presentedOutput' and read exactly
-- those retained ranges before anything is shown.  Output refusal stays data:
-- a command may have a useful status even while its streams are unavailable.
data PresentedObservation = PresentedObservation
  { presentedJob :: Job,
    presentedStatus :: CommandStatus,
    presentedOutput :: Either CommandError CommandOutput,
    presentedByteBudget :: Int
  }
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

-- | One stream's retained output, said in a way a reader cannot mistake.
--
-- There is deliberately no total accessor from this to 'Text': a caller that
-- wants the bytes matches, and in matching confronts the two ways a read ends
-- short. Only 'CaptureComplete' carries evidence — every byte the stream
-- produced, through end of file. The 'Text' in the other two constructors is a
-- display excerpt: what had been read when the read stopped, discontiguous if
-- retention had already dropped something, and never the whole stream.
data StreamCapture
  = -- | Every retained byte, contiguous from zero, through end of file.
    CaptureComplete Text
  | -- | The page that stopped the read, then the excerpt read before it.
    -- The page carries the protocol's own account of why: 'outputLostBytes'
    -- for a retention gap, 'outputLossy' for a replacement-character decode,
    -- 'outputFinished' with 'outputEnd' short of 'outputAvailableEnd' for a
    -- stream that has not ended, 'outputRetainedStart' for what rotated away.
    CapturePartial CommandPage Text
  | -- | The command service would not serve the next page, then the excerpt.
    CaptureRefused CommandError Text
  deriving (Eq, Show)

-- | Both streams of one finished command, each said separately, beside the
-- outcome and cleanup the command actually reported.
--
-- Capture failure is per stream: a lossy stderr says nothing about stdout, so
-- the two are not collapsed into one verdict. 'capturedResult' is the command's
-- own report and is never reinterpreted here — a complete capture of a command
-- that exited 3 is a successful capture of a failed command, and this type
-- keeps those two facts apart.
data Capture = Capture
  { capturedJob :: Job,
    capturedResult :: CommandResult,
    capturedStdout :: StreamCapture,
    capturedStderr :: StreamCapture
  }
  deriving (Eq, Show)

-- | The convenience form of a command operation: a refusal ends the cell.
--
-- Every one of these has a @try@ sibling that returns the refusal as a value,
-- which is what to reach for when a refusal is an ordinary outcome. When the
-- cell does end here, it ends saying what was refused and what to do, rather
-- than showing a bare constructor: @CommandUnauthorized@ alone told a reader
-- nothing about which authority was missing.
checked :: Either CommandError a -> a
checked = either (error . T.unpack . renderCommandError) id

-- | What a refused command says, in words rather than a constructor.
renderCommandError :: CommandError -> Text
renderCommandError failure = case failure of
  CommandUnauthorized ->
    "this actor may not run commands: its effect row does not include Commands, \
    \or its role does not grant them"
  CommandUnavailable detail ->
    "no command service is available to this actor: " <> detail
  CommandInvalid detail -> "the command itself is not runnable: " <> detail
  CommandOutputPending ->
    "the command has not opened its output streams yet; await it, or observe it later"
  CommandInputRejected detail -> "input was not sent: " <> detail
  CommandInputAcceptedCloseUnconfirmed detail ->
    "input was sent but closing the stream is unconfirmed: " <> detail

start :: (Member Commands effects) => Command -> Eff effects Job
start = fmap checked . tryStart

-- | Start a command, returning the refusal instead of failing the cell.
tryStart :: (Member Commands effects) => Command -> Eff effects (Either CommandError Job)
tryStart (Command spec) = fmap Job <$> send (CommandStartWith spec)

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
  send (CommandPresentWith key (CommandVisible ("session_id: " <> key <> "\n" <> resultHeading (commandResult result)) 65536))
  pure result

-- | Wait briefly and display newly available output, retaining the same job.
-- Unlike foreground 'await', an observation deadline returns the live status
-- normally, so authored handlers can continue composing effects.
observe :: (Member Commands effects) => Observation -> Job -> Eff effects CommandStatus
observe Observation {waitMilliseconds = milliseconds, outputBytes = bytes} (Job key) = do
  current <- checked <$> send (CommandAwaitWith key milliseconds)
  let heading = case current of
        CommandFinished result -> resultHeading result
        CommandQueued -> "terminal: no · queued (process not started)"
        CommandStarting -> "terminal: no · starting"
        CommandRunning -> "terminal: no · running"
        CommandStopping -> "terminal: no · stopping; cancellation not yet confirmed"
  let next = case current of
        CommandFinished _ -> ""
        _ -> "\nnext: observe the same job with write_stdin; read_output for retained output"
  send (CommandPresentWith key (CommandVisible ("session_id: " <> key <> "\n" <> heading <> next) bytes))
  pure current

-- | Observe once, then let ordinary Haskell prepare what the tool returns
-- before command output is presented.  The callback receives stream endpoints
-- frozen immediately after the wait.  The small presentation emitted here is
-- intentionally output-free: for a hosted command tool it preserves the
-- retained @Cmd.Job@ binding, while the callback's result is the only command
-- text the tool body returns.  Explicit page reads remain independent.
observeWith ::
  (Member Commands effects) =>
  Observation ->
  Job ->
  (PresentedObservation -> Eff effects Text) ->
  Eff effects (CommandStatus, Text)
observeWith options@Observation {waitMilliseconds = milliseconds} retained@(Job key) prepare = do
  current <- checked <$> send (CommandAwaitWith key milliseconds)
  -- Bound the first materialized pages by the caller's display budget.  Each
  -- page still carries the frozen stream endpoints, so a presenter can make
  -- an explicit, independently bounded page request when it needs more.
  output <- send (CommandOutputWith key (max 0 (min (16 * 1024) (outputBytes options))))
  prepared <- prepare (PresentedObservation retained current output (outputBytes options))
  send
    ( CommandPresentWith
        key
        (CommandVisible ("session_id: " <> key <> "\noutput prepared from frozen stream endpoints; raw pages remain retained") 512)
    )
  pure (current, prepared)

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

-- | Everything the command wrote to standard error, whatever its exit status.
--
-- 'stdout' deliberately refuses a non-zero exit, because a caller asking for a
-- command's output usually wants the output of a command that worked. Asking
-- why one failed is the opposite case and just as common, and most programs
-- say why on standard error — so this does not gate on the outcome. Reading a
-- failure through 'capturedOutput' and 'commandStderr' by hand is what dogfood
-- run 7's merge actor did not do, and it reported a confident wrong cause as a
-- result.
stderr :: RunResult -> Text
stderr Finished {capturedOutput = captured} = outputText (commandStderr captured)

-- | A short account of a command that did not exit 0: how it ended, and the
-- tail of whatever it said about that. 'Nothing' when it succeeded.
--
-- > Just detail -> block ("git update-ref failed: " <> detail)
failure :: RunResult -> Maybe Text
failure result@Finished {commandResult = outcome} = case commandOutcome outcome of
  CommandExited 0 -> Nothing
  other -> Just (T.pack (show other) <> said)
  where
    said = case filter (not . T.null) (map T.strip [stderr result, spoken]) of
      [] -> ""
      (text : _) -> ": " <> T.takeEnd 400 text
    spoken = outputText (commandStdout (capturedOutput result))

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

-- | Read complete retained stderr, whatever the command's exit status.
--
-- The sibling of 'readStdout' that was missing. It does not gate on the
-- outcome, for the reason 'stderr' gives: a failed command is exactly when its
-- diagnostic stream is wanted. It does not reduce to @Either OutputIssue Text@
-- either, because a stream that could not be read whole has more to say than
-- one refusal constructor — see 'StreamCapture'.
readStderr :: (Member Commands effects) => Job -> Eff effects StreamCapture
readStderr retained = captureStream retained Stderr

-- | Both streams of a retained command, its outcome and its cleanup, in one
-- call, valid when the command failed. Reading never executes anything again.
--
-- This is the paging loop that a caller otherwise writes by hand: it drives
-- 'tryPage' from byte zero to end of file and inspects each page's retention,
-- decode and end-of-file signals before it will call a stream complete. Those
-- signals survive into the result rather than being flattened, so a convenient
-- call cannot turn an incomplete capture into an apparently complete string.
--
-- 'Left' is reserved for the command as a whole: still running, or a status
-- the service would not report. A finished command always gives 'Right', even
-- when both of its streams failed to capture.
--
-- > Right capture <- Cmd.readCommand job
-- > case Cmd.capturedStderr capture of
-- >   Cmd.CaptureComplete said -> block (Cmd.capturedResult capture, said)
-- >   incomplete -> block incomplete
readCommand :: (Member Commands effects) => Job -> Eff effects (Either OutputIssue Capture)
readCommand retained@(Job key) = do
  observed <- send (CommandStatusWith key)
  case observed of
    Left failure -> pure (Left (OutputUnavailable retained failure))
    Right (CommandFinished result) ->
      fmap Right $
        Capture retained result
          <$> captureStream retained Stdout
          <*> captureStream retained Stderr
    Right _ -> pure (Left (StillRunning retained))

-- | Page one stream from byte zero, stopping at the first page that proves the
-- capture cannot be complete. Shared by 'readStderr' and 'readCommand'.
captureStream :: (Member Commands effects) => Job -> CommandStream -> Eff effects StreamCapture
captureStream retained stream = collect 0 []
  where
    collect cursor chunks = do
      observed <- tryPage retained stream (OutputOffset cursor)
      case fmap pageDetails observed of
        Left failure -> pure (CaptureRefused failure (assembled chunks))
        Right page
          | outputLossy page || outputLostBytes page /= 0 || outputStart page /= cursor ->
              pure (CapturePartial page (assembled (outputText page : chunks)))
          | outputEnd page == outputAvailableEnd page && outputFinished page ->
              pure (CaptureComplete (assembled (outputText page : chunks)))
          | outputEnd page <= cursor -> pure (CapturePartial page (assembled chunks))
          | otherwise -> collect (outputEnd page) (outputText page : chunks)
    assembled = T.concat . reverse

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
readPage retained stream position = checked <$> tryPage retained stream position

-- | Read one page, returning the refusal instead of ending the cell. What
-- 'readPage', 'readOutput' and 'next' are built from, and what a capture that
-- must survive a refused read drives directly.
tryPage :: (Member Commands effects) => Job -> CommandStream -> CommandPosition -> Eff effects (Either CommandError OutputPage)
tryPage retained@(Job key) stream position =
  fmap (OutputPage retained stream) <$> send (CommandReadWith key stream position)

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

instance WorkbenchDisplay Capture where
  workbenchDisplay = displayWith 65536

instance WorkbenchDisplay StreamCapture where
  workbenchDisplay = displayWith 65536

-- | A capture renders its outcome first, then each stream separately, and an
-- incomplete stream says so before any of its text is shown.
instance Display Capture where
  displayWith budget capture =
    let heading = resultHeading (capturedResult capture) <> "\n"
        remaining = max 0 (budget - T.length heading)
        (out, omittedOut) = captureDisplay "stdout" (remaining `div` 2) (capturedStdout capture)
        (err, omittedErr) = captureDisplay "stderr" (max 0 (remaining - T.length out)) (capturedStderr capture)
        (text, clipped) = rawText budget (heading <> out <> err)
     in (text, omittedOut || omittedErr || clipped)

instance Display StreamCapture where
  displayWith = captureDisplay "output"

captureDisplay :: Text -> Int -> StreamCapture -> (Text, Bool)
captureDisplay stream budget capture =
  let (marker, body) = case capture of
        CaptureComplete text -> (stream <> " · complete capture", text)
        CapturePartial page text ->
          ("INCOMPLETE capture · display excerpt · " <> outputMetadata stream page, text)
        CaptureRefused failure text ->
          (stream <> " · INCOMPLETE capture · display excerpt · " <> renderCommandError failure, text)
      header = marker <> "\n"
      allowance = max 0 (budget - T.length header)
      (shown, omitted) =
        if T.length body <= allowance then (body, False) else (T.takeEnd allowance body, True)
      (text, clipped) = rawText budget (header <> shown <> "\n")
   in (text, omitted || clipped)

instance Display RunResult where
  displayTree Finished {commandResult = outcome, capturedOutput = captured} =
    Concat [TextLeaf (resultHeading outcome <> "\n"), displayTree captured]
  displayWithout keys budget result@Finished {completedJob = Job key, commandResult = outcome}
    | key `elem` keys = rawText budget (resultHeading outcome <> " · output retained")
    | otherwise = displayWith budget result
  displayWith budget result = case result of
    Finished {commandResult = outcome, capturedOutput = captured} ->
      let heading = resultHeading outcome <> "\n"
          (body, omitted) = displayOutput (max 0 (budget - T.length heading)) captured
          (text, clipped) = rawText budget (heading <> body)
       in (text, omitted || clipped)

instance Display OutputPage where
  displayTree OutputPage {pageStream = stream, pageDetails = details} =
    TextLeaf (outputHeading (T.pack (show stream)) details)
  displayWith budget OutputPage {pageStream = stream, pageDetails = details} =
    rawText budget (outputHeading (T.pack (show stream)) details)

instance Display CommandOutput where
  displayTree captured =
    Concat
      [ TextLeaf (outputHeading "stdout" (commandStdout captured)),
        TextLeaf (outputHeading "stderr" (commandStderr captured))
      ]
  displayWith = displayOutput

instance Display CommandPage where
  displayTree = TextLeaf . outputHeading "output"
  displayWith budget = rawText budget . outputHeading "output"

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
                  (text, _) = rawText limit (marker <> T.takeEnd allowance (outputText details))
               in (text, True)

resultHeading :: CommandResult -> Text
resultHeading result =
  "terminal: yes · "
    <> T.pack (show (commandOutcome result))
    <> case commandCleanup result of
      CommandClean -> " · cleanup: clean\nnext: inspect outcome and output; read_output for omitted diagnostics"
      other -> " · cleanup: " <> T.pack (show other) <> "\nnext: inspect cleanup and retained job before releasing resources"

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
    <> (if outputStart page > outputRetainedStart page && outputLostBytes page == 0 then " · earlier retained output available" else "")
    <> (if outputEnd page < outputAvailableEnd page then " · more available" else if outputFinished page then " · EOF" else " · current end; running")
    <> (if outputLossy page then " · lossy UTF-8" else "")
    <> (if outputLeadingFragment page then " · leading line fragment" else "")
    <> (if outputTrailingFragment page then " · trailing line fragment" else "")
  where
    number = T.pack . show

instance Display OutputIssue where
  displayWith budget issue = rawText budget $ case issue of
    IncompleteStdout retained ->
      "Command finished; this capture is incomplete. Awaiting again does not enlarge it. Use Cmd.readStdout with your existing job binding, or Cmd.job applied to your result; Cmd.output navigates retained output. Retention gaps are explicit. Job: " <> T.pack (show retained)
    StillRunning retained ->
      "Command still running: " <> T.pack (show retained) <> ". Continue observing the same job with Cmd.await."
    other -> T.pack (show other)

-- Reading a continuation uses the retained job and cursor. It never calls run,
-- start, or await, and drains this page's text before requesting another page.
instance (Member Commands effects) => PageDisplay effects OutputPage where
  displayPage budget page = outputDisplayPage budget page Nothing

outputDisplayPage :: (Member Commands effects) => Int -> OutputPage -> Maybe (Eff effects (DisplayPage effects)) -> DisplayPage effects
outputDisplayPage budget page following =
  let details = pageDetails page
      unread = outputEnd details < outputAvailableEnd details || not (outputFinished details)
      continuation =
        if unread
          then Just (do later <- next page; pure (outputDisplayPage 8192 later following))
          else following
   in pageWithContinuation budget (displayTree page) continuation

instance (Member Commands effects) => PageDisplay effects RunResult where
  displayPage budget result@Finished {completedJob = retained, capturedOutput = captured} =
    pageWithContinuation budget (displayTree result) (remainingOutput retained captured)
  displayPageWithout keys budget result@Finished {completedJob = retained@(Job key), commandResult = outcome}
    | key `elem` keys =
        let stderr = Just (do page <- readOutput Stderr retained; pure (outputDisplayPage 8192 page Nothing))
            allOutput = Just (do page <- output retained; pure (outputDisplayPage 8192 page stderr))
         in pageWithContinuation budget (TextLeaf (resultHeading outcome <> " · output retained")) allOutput
    | otherwise = displayPage budget result

remainingOutput :: (Member Commands effects) => Job -> CommandOutput -> Maybe (Eff effects (DisplayPage effects))
remainingOutput retained captured =
  missing
    Stdout
    (commandStdout captured)
    (missing Stderr (commandStderr captured) Nothing)
  where
    missing stream details following
      | outputStart details > 0 =
          Just
            ( do
                page <- readOutput stream retained
                pure (outputDisplayPage 8192 page following)
            )
      | outputEnd details < outputAvailableEnd details || not (outputFinished details) =
          Just
            ( do
                page <- readPage retained stream (OutputOffset (outputEnd details))
                pure (outputDisplayPage 8192 page following)
            )
      | otherwise = following
