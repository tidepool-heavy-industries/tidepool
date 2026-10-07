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
    withSource,
    inDirectory,
    withEnvironment,
    withArguments,
    withStdin,
    withTerminal,
    start,
    tryStart,
    tryStartWith,
    retain,
    Lifetime,
    detach,
    tryDetach,
    background,
    tryBackground,
    awaitFinished,
    CommandReport (..),
    CommandSource (..),
    run,
    await,
    Observation (..),
    PresentedObservation (..),
    observe,
    observeCompletion,
    observeWith,
    quiet,
    job,
    status,
    retainJobBinding,
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
import Tidepool.Agent.Watch.Internal (Await (..), AwaitPlan (..), AwaitNode (..), AwaitDependency (..), Watches (..), requireObserved)
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
    CommandReport (..),
    CommandResult (..),
    CommandSource (..),
    CommandSpec (..),
    CommandStatus (..),
    CommandStream (..),
    Commands (..),
    WorkerLifetime,
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
  = Finished {completedJob :: Job, commandResult :: CommandResult, capturedOutput :: Either CommandError CommandOutput}
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

-- | Convenience refusals stop the continuation, even when its result is
-- discarded. Existing @try@ variants retain the refusal as a typed value.
checked :: Either CommandError a -> Eff effects a
checked = either (error . T.unpack . renderCommandError) pure

-- | What a refused command says, in words rather than a constructor.
renderCommandError :: CommandError -> Text
renderCommandError failure = case failure of
  CommandUnauthorized ->
    "this command operation is not authorized: starting requires command authority; \
    \input, resize, detachment and cancellation require the job's owner. Shared handles allow \
    \status and output reads, not control; ask the owner to perform the operation"
  CommandUnavailable detail ->
    "command resource or service unavailable: " <> detail
  CommandInvalid detail -> "the command itself is not runnable: " <> detail
  CommandOutputPending ->
    "the command has not opened its output streams yet; await it, or observe it later"
  CommandInputRejected detail -> "input was not sent: " <> detail
  CommandInputAcceptedCloseUnconfirmed detail ->
    "input was sent but closing the stream is unconfirmed: " <> detail

-- | Start work owned by this invocation. Await it or explicitly detach before
-- returning; scope exit cancels unfinished owned work and retains cleanup.
start :: (Member Commands effects) => Command -> Eff effects Job
start command = tryStart command >>= checked

-- | Start a command, returning the refusal instead of failing the cell.
tryStart :: (Member Commands effects) => Command -> Eff effects (Either CommandError Job)
tryStart (Command spec) = fmap Job <$> send (CommandStartWith spec)

-- | Cleanup lifetime, shared with subagents and requests.
type Lifetime = WorkerLifetime

-- | Start with explicit cleanup membership; defaults of 'tryStart' remain
-- invocation ownership. Selecting a scope checks its runtime admission gate.
tryStartWith :: Member Commands effects => Lifetime -> Command -> Eff effects (Either CommandError Job)
tryStartWith lifetime (Command spec) = fmap Job <$> send (CommandStartOwnedWith spec lifetime)

-- | Move cleanup membership while preserving the job's controlling actor and
-- construction provenance. Returning a Job does not transfer its lifetime.
retain :: Member Commands effects => Job -> Lifetime -> Eff effects (Either CommandError ())
retain (Job key) lifetime = send (CommandRetainWith key lifetime)

-- | Transfer an owned job to this actor's lifetime. Borrowed handles cannot
-- detach or cancel another owner's work.
detach :: (Member Commands effects) => Job -> Eff effects ()
detach job = tryDetach job >>= checked

tryDetach :: (Member Commands effects) => Job -> Eff effects (Either CommandError ())
tryDetach (Job key) = send (CommandDetachWith key)

-- | Start actor-owned work and return at once. This explicitly detaches its
-- lifetime from the invocation. When it finishes, a settlement
-- notice wakes this actor, unless a watch on 'awaitFinished' takes that wake
-- over. The notice and the report carry the commit the command started at;
-- the checkout is not guarded while it runs. A job running when the host
-- restarts is not recovered and sends no notice.
background :: (Member Commands effects) => Command -> Eff effects Job
background command = tryBackground command >>= checked

-- | Start in the background, returning the refusal instead of failing the cell.
tryBackground :: (Member Commands effects) => Command -> Eff effects (Either CommandError Job)
tryBackground (Command spec) = fmap Job <$> send (CommandBackgroundWith spec)

-- | Ready when the job has finished, with its outcome, cleanup, output
-- completeness, a diagnostic tail, and the source it started at. Compose it
-- with request readiness and use it with 'await', 'watch' or 'route'. A report is evidence about the commit the command
-- started at; it says nothing about a later revision.
awaitFinished :: Job -> Await CommandReport
awaitFinished (Job key) =
  Await (AwaitPlan [LeafNode (AwaitCommand key)] 0) (\watchId _ _ -> requireObserved <$> send (ObserveWatchCommandWith watchId key))

-- | Run once and suspend until terminal completion, preserving the continuation.
run :: (Member Commands effects) => Command -> Eff effects RunResult
run command = start command >>= await

-- | Suspend until the retained job is terminal. The process outcome and cleanup
-- survive an unavailable output transport; explicit observation owns presentation.
await :: (Member Commands effects) => Job -> Eff effects RunResult
await retained@(Job key) = do
  observation <- send (CommandWaitWith key) >>= checked
  pure (Finished retained (observedCommandResult observation) (observedCommandOutput observation))

-- | Wait briefly and display newly available output, retaining the same job.
-- An observation deadline returns the live status normally.
observe :: (Member Commands effects) => Observation -> Job -> Eff effects CommandStatus
observe Observation {waitMilliseconds = milliseconds, outputBytes = bytes} (Job key) = do
  current <- send (CommandAwaitWith key milliseconds) >>= checked
  presentStatus key bytes False current

-- | Observe an owned job for a bounded interval and request a completion
-- notice if it is still running. The notice does not detach the job: await it
-- or explicitly 'detach' before leaving its invocation. Foreign observers can
-- use 'observe' or 'awaitFinished' with a watch.
observeCompletion :: (Member Commands effects) => Observation -> Job -> Eff effects CommandStatus
observeCompletion Observation {waitMilliseconds = milliseconds, outputBytes = bytes} (Job key) = do
  current <- send (CommandAwaitAndNotifyWith key milliseconds) >>= checked
  presentStatus key bytes True current

presentStatus :: (Member Commands effects) => Text -> Int -> Bool -> CommandStatus -> Eff effects CommandStatus
presentStatus key bytes completionNotice current = do
  let heading = case current of
        CommandFinished result -> resultHeading result
        CommandQueued -> "terminal: no · queued (process not started)"
        CommandStarting -> "terminal: no · starting"
        CommandRunning -> "terminal: no · running"
        CommandStopping -> "terminal: no · stopping; cancellation not yet confirmed"
  let next = case current of
        CommandFinished _ -> ""
        _
          | completionNotice -> "\nA completion notice or an existing watch will wake you; keep working. read_output can inspect retained output."
          | otherwise -> "\nnext: observe the same job with write_stdin; read_output for retained output"
  send (CommandPresentWith key (CommandVisible ("session_id: " <> key <> "\n" <> heading <> next) bytes))
  pure current

-- | Observe once, then let ordinary Haskell prepare the tool's returned value.
-- The callback receives stream endpoints frozen immediately after the wait.
-- This helper has no implicit presentation side effect; a named tool's
-- returned presenter owns the model-visible text and retention binding.
observeWith ::
  (Member Commands effects) =>
  Observation ->
  Job ->
  (PresentedObservation -> Eff effects a) ->
  Eff effects (CommandStatus, a)
observeWith options@Observation {waitMilliseconds = milliseconds} retained@(Job key) prepare = do
  current <- send (CommandAwaitWith key milliseconds) >>= checked
  presentObserved options retained prepare current

presentObserved ::
  (Member Commands effects) =>
  Observation ->
  Job ->
  (PresentedObservation -> Eff effects a) ->
  CommandStatus ->
  Eff effects (CommandStatus, a)
presentObserved options retained@(Job key) prepare current = do
  -- Bound the first materialized pages by the caller's display budget.  Each
  -- page still carries the frozen stream endpoints, so a presenter can make
  -- an explicit, independently bounded page request when it needs more.
  output <- send (CommandOutputWith key (max 0 (min (16 * 1024) (outputBytes options))))
  prepared <- prepare (PresentedObservation retained current output (outputBytes options))
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
stdout result@Finished {commandResult = outcome, capturedOutput = observed} =
  case commandOutcome outcome of
    CommandExited 0 -> do
      captured <- either (Left . OutputUnavailable (job result)) Right observed
      let text = commandStdout captured
      if outputLossy text
        then Left (InvalidOutputEncoding (job result))
        else
          if outputStart text /= 0 || outputEnd text /= outputAvailableEnd text || outputLostBytes text /= 0 || not (outputFinished text)
            then Left (IncompleteStdout (job result))
            else Right (outputText text)
    other -> Left (Unsuccessful other)

-- | Retained stderr, whatever the exit status. Output availability is independent
-- of the process outcome; 'readStderr' can recover a complete paged capture.
stderr :: RunResult -> Either OutputIssue Text
stderr result@Finished {capturedOutput = observed} =
  outputText . commandStderr <$> either (Left . OutputUnavailable (job result)) Right observed

-- | A short account of failure, preserving the outcome when output is unavailable.
failure :: RunResult -> Maybe Text
failure Finished {commandResult = outcome, capturedOutput = observed} = case commandOutcome outcome of
  CommandExited 0 -> Nothing
  other -> Just (outcomeText other <> said)
  where
    said = case observed of
      Left issue -> ": output unavailable: " <> renderCommandError issue
      Right captured -> case filter (not . T.null) (map T.strip [outputText (commandStderr captured), outputText (commandStdout captured)]) of
        [] -> ""
        (text : _) -> ": " <> T.takeEnd 400 text

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
status (Job key) = send (CommandStatusWith key) >>= checked

-- | Name an existing command job in this actor's resident Haskell context.
-- The host resolves ownership and returns the binding's source-level name.
retainJobBinding :: (Member Commands effects) => Job -> Eff effects Text
retainJobBinding (Job key) = send (CommandRetainJobWith key) >>= checked

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
readPage retained stream position = tryPage retained stream position >>= checked

-- | Read one page, returning the refusal instead of ending the cell. What
-- 'readPage', 'readOutput' and 'next' are built from, and what a capture that
-- must survive a refused read drives directly.
tryPage :: (Member Commands effects) => Job -> CommandStream -> CommandPosition -> Eff effects (Either CommandError OutputPage)
tryPage retained@(Job key) stream position =
  fmap (OutputPage retained stream) <$> send (CommandReadWith key stream position)

pageText :: OutputPage -> Text
pageText = outputText . pageDetails

sendInput :: (Member Commands effects) => Job -> Text -> Eff effects ()
sendInput (Job key) text = send (CommandInputWith key text) >>= checked

closeInput :: (Member Commands effects) => Job -> Eff effects ()
closeInput (Job key) = send (CommandCloseInputWith key) >>= checked

resize :: (Member Commands effects) => Job -> Int -> Int -> Eff effects ()
resize (Job key) rows columns = send (CommandResizeWith key rows columns) >>= checked

cancel :: (Member Commands effects) => Job -> Eff effects ()
cancel (Job key) = send (CommandCancelWith key) >>= checked

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
    Concat [TextLeaf (resultHeading outcome <> "\n"), either (TextLeaf . ("output unavailable: " <>) . renderCommandError) displayTree captured]
  displayWithout keys budget result@Finished {completedJob = Job key, commandResult = outcome}
    | key `elem` keys = rawText budget (resultHeading outcome <> " · output retained")
    | otherwise = displayWith budget result
  displayWith budget result = case result of
    Finished {commandResult = outcome, capturedOutput = captured} ->
      let heading = resultHeading outcome <> "\n"
          (body, omitted) = either (rawText (max 0 (budget - T.length heading)) . ("output unavailable: " <>) . renderCommandError) (displayOutput (max 0 (budget - T.length heading))) captured
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
    <> outcomeText (commandOutcome result)
    <> case commandCleanup result of
      CommandClean -> " · cleanup: clean\nnext: inspect outcome and output; read_output for omitted diagnostics"
      other -> " · cleanup: " <> T.pack (show other) <> "\nnext: inspect cleanup and retained job before releasing resources"

-- | Model-facing rendering of a command outcome. 'CommandOutOfMemory' and
-- 'CommandSignalled' get their own text so a caller reads a rerun hint and an
-- applied limit instead of guessing what an exit code of 137 or the like
-- means; every other outcome keeps its derived 'Show'.
outcomeText :: CommandOutcome -> Text
outcomeText (CommandOutOfMemory limit) =
  "out of memory · memory_mib=" <> T.pack (show limit) <> " exceeded · rerun with a larger memory_mib"
outcomeText (CommandSignalled signal) =
  "killed by signal " <> T.pack (show signal)
outcomeText other = T.pack (show other)

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
    pageWithContinuation budget (displayTree result) (either (const Nothing) (remainingOutput retained) captured)
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
