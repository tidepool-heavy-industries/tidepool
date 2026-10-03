{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Tidepool.Command.Tools
  ( ShellTools (..),
    Execute (..),
    EnvironmentEntry (..),
    WriteInput (..),
    ReadOutput (..),
    CancelCommand (..),
    Stream (..),
    CommandOptionError (..),
    CommandToolFacts (..),
    CommandToolResult (..),
    CommandOutcomeFact (..),
    CommandCleanupFact (..),
    observedResult,
    ObservationPresenter,
    tools,
    toolsWith,
    execute,
    executeWith,
    writeInput,
    writeInputWith,
    readRetained,
    cancelRetained,
  )
where

import Control.Monad.Freer (Eff, Member, send)
import Data.Char (ord)
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Aeson.Value (ToJSON (..))
import qualified Tidepool.Command as Cmd
import Tidepool.Command.Types (Job (..))
import Tidepool.Effects.Core (Commands (..))
import Tidepool.Inspection (Display (..))

data Execute = Execute
  { cmd :: Text,
    workdir :: Maybe Text,
    environment :: Maybe [EnvironmentEntry],
    memory_mib :: Maybe Int,
    tty :: Maybe Bool,
    stdin :: Maybe Bool,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int,
    intent :: Maybe Text,
    focus :: Maybe Text,
    background :: Maybe Bool
  }
  deriving (Generic, FromJSON, JsonSchema)

data EnvironmentEntry = EnvironmentEntry
  { name :: Text,
    value :: Text
  }
  deriving (Generic, FromJSON, JsonSchema)

data WriteInput = WriteInput
  { session_id :: Text,
    chars :: Maybe Text,
    close_stdin :: Maybe Bool,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

data CancelCommand = CancelCommand
  { session_id :: Text,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

data Stream = Stdout | Stderr
  deriving (Generic, FromJSON, JsonSchema)

data CommandOptionError
  = InvalidMemoryMiB Int
  | ConflictingInputOptions
  | InvalidYieldTime Int
  | InvalidOutputBytes Int
  | InvalidOutputOffset Int
  deriving (Eq, Show)

-- | Bounded facts about a named command-tool reply. Presentation text is kept
-- beside these facts for the installed-tool presenter, but is deliberately
-- excluded from semantic JSON so stdout is never copied into both channels.
data CommandToolResult = CommandToolResult
  { facts :: CommandToolFacts,
    presentation :: Text
  }
  deriving (Generic)

data CommandToolFacts
  = ObservedCommand
      { session_id :: Text,
        state :: CommandState,
        successful :: Maybe Bool,
        outcome :: Maybe CommandOutcomeFact,
        cleanup :: Maybe CommandCleanupFact,
        stdout_end :: Maybe Int,
        stderr_end :: Maybe Int,
        payload_lines :: Int,
        complete :: Bool,
        retained_binding :: Maybe Text
      }
  | InputReceipt
      { session_id :: Text,
        disposition :: ReceiptDisposition,
        input_bytes :: Int,
        eof :: EofDisposition
      }
  | OutputPage
      { session_id :: Text,
        stream :: Cmd.CommandStream,
        start_offset :: Int,
        end_offset :: Int,
        next_offset :: Int,
        retained_end :: Int,
        eof :: Bool,
        lost_bytes :: Int,
        lossy :: Bool,
        payload_lines :: Int,
        complete :: Bool
      }
  | Cancellation
      { session_id :: Text,
        requested :: CancellationDisposition,
        observed_state :: Maybe CommandState,
        successful :: Maybe Bool,
        outcome :: Maybe CommandOutcomeFact,
        cleanup :: Maybe CommandCleanupFact
      }
  | Rejected
      { operation :: CommandOperation,
        session_id :: Maybe Text,
        reason :: Text,
        side_effect :: SideEffectDisposition
      }
  deriving (Generic, FromJSON)

data CommandState = Queued | Starting | Running | Stopping | Finished
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data ReceiptDisposition = Acknowledged | Unconfirmed | NotAccepted
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data EofDisposition = NotRequested | Confirmed | UnconfirmedEof
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data SideEffectDisposition = NoSideEffect | MayHaveOccurred
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data CancellationDisposition = Accepted | UnconfirmedCancellation | NotAcceptedCancellation
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data CommandOperation = Bash | WriteStdin | ReadOutput | CancelCommand
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data CommandOutcomeFact
  = OutcomeExited Int
  | OutcomeSignalled Int
  | OutcomeOutOfMemory Int
  | OutcomeCancelled
  | OutcomeFailed
  | OutcomeUnconfirmed
  deriving (Eq, Show, Generic, FromJSON, JsonSchema, ToJSON)

data CommandCleanupFact = CleanupClean | CleanupRetained | CleanupUnknown
  deriving (Eq, Show, Generic, FromJSON, JsonSchema, ToJSON)

instance ToJSON CommandToolFacts

instance ToJSON CommandToolResult where
  toJSON CommandToolResult {facts = semantic} = toJSON semantic

instance JsonSchema CommandToolFacts

instance JsonSchema CommandToolResult where
  jsonSchema _ = jsonSchema (Proxy :: Proxy CommandToolFacts)

instance Display CommandToolResult where
  displayTree = TextLeaf . presentation

instance WorkbenchDisplay CommandToolResult where
  workbenchDisplay value = (presentation value, False)

data ReadOutput = ReadOutput
  { session_id :: Text,
    stream :: Maybe Stream,
    offset :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

-- Schemas and handlers travel together through the ordinary tools-record DSL.
-- These definitions can be selected or reused by project-authored tool records.
data ShellTools mode = ShellTools
  { bash :: mode :- Call Execute CommandToolResult,
    writeStdin :: mode :- Call WriteInput CommandToolResult,
    readOutput :: mode :- Call ReadOutput CommandToolResult,
    cancelCommand :: mode :- Call CancelCommand CommandToolResult
  }
  deriving (Generic)

-- | A composable policy for one command observation.  The shared shell owns
-- validation, process/input handling and retained jobs; a workspace may only
-- replace how a successful observation is prepared for display.
type ObservationPresenter effects = Maybe Text -> Maybe Text -> Maybe Text -> Cmd.Observation -> Cmd.Job -> Eff effects CommandToolResult

tools :: (Member Cmd.Commands effects) => ShellTools (AsServerT (Eff effects))
tools = toolsWith defaultPresenter

toolsWith :: (Member Cmd.Commands effects) => ObservationPresenter effects -> ShellTools (AsServerT (Eff effects))
toolsWith presenter =
  ShellTools
    { bash =
        presentWith presentation $ tool
          "Execute Bash once; no shell profiles. Optional workdir and environment list of {name,value} entries; later duplicate names win. memory_mib (positive, default 1024); tty or piped stdin (mutually exclusive). By default the invocation owns the command until terminal completion, then presents output once. yield_time_ms (0..300000) opts into bounded observation and actor ownership if still running; no automatic notice. max_output_bytes clamps to 1024..32768 (default 32768). Returns session_id and a retained Cmd.Job. Use focus when output may be large or is a failing build/test: say what you are looking for (\"the failing test and its assertion\") and the result keeps the relevant sections and names what was omitted. Without focus, output is head/tail truncated; read_output pages retained output by byte range without rerunning. background: true returns at once (focus, yield_time_ms, max_output_bytes ignored); a notice with exit status, output tail and starting commit wakes you when it finishes. intent gives the command's purpose to its presenter."
          (executeWith presenter),
      writeStdin =
        presentWith presentation $ tool
          "Send input to an existing session_id or observe it. With chars or close_stdin, return the write/EOF receipt only; do not infer child consumption from acknowledgment or replay uncertain input. close_stdin sends final bytes then EOF for pipes; PTYs use control characters and reject close_stdin. With empty/omitted chars and no close_stdin, observe the job for 0..300000ms (default 250); max_output_bytes clamps to 1024..32768 bytes."
          (writeInputWith presenter),
      readOutput =
        presentWith presentation $ tool
          "Read retained output; no execution, waiting, or consumption. Defaults: Stdout, nonnegative byte offset 0, 8192 bytes; max_output_bytes clamps to 1024..32768 including metadata (any positive value is accepted). Contiguous pages report next_offset, EOF/current end, and retention gaps. Use Stderr for diagnostics."
          readRetained,
      cancelCommand =
        presentWith presentation $ tool
          "Request cancellation, then observe the same session_id (0..300000ms, default 250). Works for queued jobs, pipes, and PTYs. Acknowledgment is not terminal/cleanup confirmation. Repeated cancellation preserves the outcome; output remains retained."
          (cancelRetainedWith presenter)
    }

-- Validate observation options before starting or observing; committed stdin
-- writes do not also perform an observation.
-- Tools render typed rejections only at their Text result boundary.
-- max_output_bytes is clamped into the supported range rather than rejected:
-- any positive request is honored, just at whatever budget the range allows,
-- and the presented output already carries a recovery hint when it is cut
-- short by that budget.
observation :: Int -> Maybe Int -> Maybe Int -> Either CommandOptionError Cmd.Observation
observation defaultWait wait limit
  | milliseconds < 0 || milliseconds > 300000 = Left (InvalidYieldTime milliseconds)
  | requested <= 0 = Left (InvalidOutputBytes requested)
  | otherwise = Right (Cmd.Observation milliseconds bytes)
  where
    milliseconds = fromMaybe defaultWait wait
    requested = fromMaybe 32768 limit
    bytes = max 1024 (min 32768 requested)

outputBudget :: Maybe Int -> Int
outputBudget requested = max 1024 (min 32768 (fromMaybe 32768 requested))

executeOptions :: Maybe Int -> Maybe Bool -> Maybe Bool -> Maybe Int -> Maybe Int -> Either CommandOptionError (Int, Cmd.Observation)
executeOptions memory terminal pipe wait limit = do
  options <- observation 0 wait limit
  let memoryMiB = fromMaybe 1024 memory
  if memoryMiB <= 0 || memoryMiB > maxBound `div` (1024 * 1024)
    then Left (InvalidMemoryMiB memoryMiB)
    else
      if fromMaybe False terminal && fromMaybe False pipe
        then Left ConflictingInputOptions
        else Right (memoryMiB, options)

readOptions :: Maybe Int -> Maybe Int -> Either CommandOptionError (Int, Cmd.Observation)
readOptions position limit = do
  options <- observation 0 (Just 0) (Just (fromMaybe 8192 limit))
  let offset = fromMaybe 0 position
  if offset < 0
    then Left (InvalidOutputOffset offset)
    else Right (offset, options)

renderOptionError :: CommandOptionError -> Text
renderOptionError issue = "Rejected · nothing started or sent · " <> detail
  where
    detail = case issue of
      InvalidMemoryMiB _ -> "memory_mib must be positive and fit Int bytes"
      ConflictingInputOptions -> "tty and stdin cannot both be true"
      InvalidYieldTime _ -> "yield_time_ms must be 0..300000"
      InvalidOutputBytes _ -> "max_output_bytes must be positive"
      InvalidOutputOffset _ -> "offset must be nonnegative"

execute :: (Member Cmd.Commands effects) => Execute -> Eff effects CommandToolResult
execute = executeWith defaultPresenter

executeWith :: (Member Cmd.Commands effects) => ObservationPresenter effects -> Execute -> Eff effects CommandToolResult
executeWith presenter
  Execute
    { cmd = script,
      workdir = directory,
      environment = env,
      memory_mib = memory,
      tty = terminal,
      stdin = pipe,
      yield_time_ms = wait,
      max_output_bytes = limit,
      intent = purpose,
      focus = focus,
      background = detached
    } =
    let inBackground = fromMaybe False detached
        observationWait = if inBackground then Nothing else wait
        observationLimit = if inBackground then Nothing else limit
     in case executeOptions memory terminal pipe observationWait observationLimit of
      Left rejection -> pure (rejected Bash (renderOptionError rejection) NoSideEffect)
      Right (memoryMiB, options) -> do
        let environmentVariables =
              Map.toList . Map.fromList $
                [ (variableName, variableValue)
                  | EnvironmentEntry {name = variableName, value = variableValue} <- fromMaybe [] env
                ]
            command =
              maybe id Cmd.inDirectory directory $
                Cmd.withEnvironment environmentVariables $
                  Cmd.withMemory (Cmd.MiB memoryMiB) $
                    input (Cmd.bashCommand script)
            input =
              if fromMaybe False terminal
                then Cmd.withTerminal
                else if fromMaybe False pipe then Cmd.withStdin else id
            reportedCommand = if inBackground then Cmd.withSource command else command
        started <- if inBackground then Cmd.tryBackground reportedCommand else Cmd.tryStart command
        case started of
          Left Cmd.CommandUnauthorized ->
            pure (rejected Bash "this actor has no command authority" NoSideEffect)
          Left issue -> pure $ rejected Bash (T.pack (show issue)) NoSideEffect
      Right retained@(Job key)
            | inBackground -> do
                binding <- Cmd.retainJobBinding retained
                current <- Cmd.status retained
                let currentFacts = facts (observedResult retained current Nothing Nothing 0 False "")
                    factsWithBinding = case currentFacts of
                      ObservedCommand {session_id = observedKey, state = currentState, successful = success, outcome = commandOutcome, cleanup = commandCleanup, stdout_end = stdoutEnd, stderr_end = stderrEnd, payload_lines = linesShown, complete = isComplete} ->
                        ObservedCommand observedKey currentState success commandOutcome commandCleanup stdoutEnd stderrEnd linesShown isComplete (Just binding)
                      other -> other
                pure (boundedResult (Cmd.outputBytes options) (result factsWithBinding ("session_id: " <> key <> "\nretained as " <> binding <> " :: Cmd.Job\nStarted in background; its completion notice will wake you. Keep working; do not poll. Use read_output to inspect output or cancel_command to stop it.")))
            | otherwise -> case wait of
                Nothing -> do
                  _ <- Cmd.await retained
                  binding <- Cmd.retainJobBinding retained
                  boundedResult (Cmd.outputBytes options) . attachBinding key binding <$> presenter (Just script) purpose focus (budgetForBinding options binding) retained
                Just _ -> do
                  binding <- Cmd.retainJobBinding retained
                  shown <- boundedResult (Cmd.outputBytes options) . attachBinding key binding <$> presenter (Just script) purpose focus (budgetForBinding options binding) retained
                  current <- Cmd.status retained
                  case current of
                    Cmd.CommandFinished _ -> pure shown
                    _ -> Cmd.detach retained >> pure shown

writeInput :: (Member Cmd.Commands effects) => WriteInput -> Eff effects CommandToolResult
writeInput = writeInputWith defaultPresenter

writeInputWith :: (Member Cmd.Commands effects) => ObservationPresenter effects -> WriteInput -> Eff effects CommandToolResult
writeInputWith presenter WriteInput {session_id = key, chars = input, close_stdin = close, yield_time_ms = wait, max_output_bytes = limit} =
  let text = fromMaybe "" input
      eof = fromMaybe False close
      observe = case observation 250 wait limit of
        Left rejection -> pure (rejected WriteStdin (renderOptionError rejection) NoSideEffect)
        Right options -> boundedResult (Cmd.outputBytes options) <$> presenter Nothing Nothing Nothing options (Job key)
      submit = do
        receipt <- case (T.null text, eof) of
          (True, False) -> pure (Right ())
          (True, True) -> send (CommandCloseInputWith key)
          (False, False) -> send (CommandInputWith key text)
          (False, True) -> send (CommandFinishInputWith key text)
        case receipt of
          Left (Cmd.CommandInputRejected detail) ->
          pure $ boundedResult (outputBudget limit) (result (InputReceipt key NotAccepted 0 NotRequested) ("Rejected · input not submitted (including chars); EOF not submitted · " <> detail))
          Left Cmd.CommandUnauthorized ->
            pure $ boundedResult (outputBudget limit) (result (InputReceipt key NotAccepted 0 NotRequested) "Rejected · input not submitted (including chars); EOF not submitted · input control is not authorized")
          Left (Cmd.CommandInputAcceptedCloseUnconfirmed detail) ->
            pure $ boundedResult (outputBudget limit) (result (InputReceipt key Acknowledged (utf8Bytes text) UnconfirmedEof) ("Backend acknowledged the write; child consumption is unknown. EOF unconfirmed: " <> detail <> "\nRetry close-only with write_stdin(close_stdin=true), without chars. Do not resend these bytes."))
          Left issue -> pure $ boundedResult (outputBudget limit) (result (InputReceipt key Unconfirmed (utf8Bytes text) (if eof then UnconfirmedEof else NotRequested)) ("Input submission unconfirmed: " <> T.pack (show issue) <> "\nInspect the same job before recovery. Do not replay input after an uncertain acknowledgment."))
          Right () ->
            let receipt =
                  if T.null text
                    then ""
                    else "Input acknowledged by backend; child consumption is unknown."
                closed = if eof then "Stdin is closed." else ""
             in -- Sending bytes (or EOF) is an irreversible action. Return its
                -- acknowledgment directly: a later, optional output presentation
                -- must not turn a successful write into an apparent failed call.
                pure $ boundedResult (outputBudget limit) (result (InputReceipt key Acknowledged (utf8Bytes text) (if eof then Confirmed else NotRequested)) (T.intercalate "\n" (filter (not . T.null) [receipt, closed])))
   in if T.null text && not eof then observe else submit

cancelRetained :: (Member Cmd.Commands effects) => CancelCommand -> Eff effects CommandToolResult
cancelRetained = cancelRetainedWith defaultPresenter

cancelRetainedWith :: (Member Cmd.Commands effects) => ObservationPresenter effects -> CancelCommand -> Eff effects CommandToolResult
cancelRetainedWith _presenter CancelCommand {session_id = key, yield_time_ms = wait, max_output_bytes = limit} =
  case observation 250 wait limit of
    Left rejection -> pure (rejected CancelCommand (renderOptionError rejection) NoSideEffect)
    Right (Cmd.Observation {waitMilliseconds = waitMilliseconds, outputBytes = budget}) -> do
      receipt <- send (CommandCancelWith key)
      case receipt of
        Left issue ->
          let disposition = case issue of
                Cmd.CommandUnauthorized -> NotAcceptedCancellation
                Cmd.CommandInvalid _ -> NotAcceptedCancellation
                _ -> UnconfirmedCancellation
           in pure $ boundedResult budget (result (Cancellation key disposition Nothing Nothing Nothing Nothing) ("Cancellation unconfirmed: " <> T.pack (show issue) <> "\nInspect the same job; do not start a replacement."))
        Right () -> do
          -- Cancellation is already accepted. A later wait/status failure must
          -- not turn that irreversible acknowledgment into a failed tool call.
          status <- send (CommandAwaitWith key waitMilliseconds)
          let (currentState, succeeded, outcome, cleanup) = case status of
                Right current -> (Just (commandState current), statusSuccess current, statusOutcome current, statusCleanup current)
                Left _ -> (Nothing, Nothing, Nothing, Nothing)
              receipt = Cancellation key Accepted currentState succeeded outcome cleanup
              prefix = case currentState of
                Just Finished -> "Cancellation was requested; terminal outcome is recorded."
                _ -> "Cancellation requested; terminal outcome and cleanup are not yet confirmed."
          pure (boundedResult budget (result receipt prefix))

readRetained :: (Member Cmd.Commands effects) => ReadOutput -> Eff effects CommandToolResult
readRetained ReadOutput {session_id = key, stream = selected, offset = position, max_output_bytes = limit} =
  case readOptions position limit of
    Left rejection -> pure (rejected ReadOutput (renderOptionError rejection) NoSideEffect)
    Right (offset, Cmd.Observation {outputBytes = budget}) -> do
      let selectedStream = case selected of
            Just Stderr -> Cmd.Stderr
            _ -> Cmd.Stdout
          contentBudget = budget - 512
          read bytes = send (CommandReadWith key selectedStream (Cmd.OutputSlice offset bytes))
      first <- read contentBudget
      -- Replacement characters can expand invalid UTF-8. Ask the byte owner for
      -- a smaller page rather than inventing offsets from decoded text.
      result <- case first of
        Right page | utf8Bytes (Cmd.outputText page) > contentBudget -> read (max 1 (contentBudget `div` 3))
        _ -> pure first
      case result of
        Left Cmd.CommandOutputPending -> pure $ boundedResult budget (result (OutputPage key selectedStream offset offset offset offset False 0 False 0 False) "No output yet; streams are starting.")
        Left issue -> pure $ boundedResult budget (result (Rejected ReadOutput (Just key) (T.pack (show issue)) NoSideEffect) ("Output unavailable: " <> T.pack (show issue) <> "\nDo not rerun the command to recover this retained page."))
        Right details -> do
          let eof = Cmd.outputFinished details && Cmd.outputEnd details == Cmd.outputAvailableEnd details
              complete = Cmd.outputStart details == 0 && eof && not (Cmd.outputLossy details) && Cmd.outputLostBytes details == 0
              payload = Cmd.outputText details
              text = streamName selectedStream <> " · " <> payload <> "\nnext_offset: " <> T.pack (show (Cmd.outputEnd details))
              page = OutputPage key selectedStream (Cmd.outputStart details) (Cmd.outputEnd details) (Cmd.outputEnd details) (Cmd.outputAvailableEnd details) eof (Cmd.outputLostBytes details) (Cmd.outputLossy details) (lineCount payload) complete
          pure $ boundedResult budget (result page text)

defaultPresenter :: (Member Cmd.Commands effects) => ObservationPresenter effects
defaultPresenter _ _ _ options retained = do
  (_, shown) <- Cmd.observeWith options retained render
  pure shown
  where
    render observed = case Cmd.presentedOutput observed of
      Left issue -> pure (observedResult retained (Cmd.presentedStatus observed) Nothing Nothing 0 False (statusText (Cmd.presentedStatus observed) <> "\nOutput unavailable: " <> Cmd.renderCommandError issue))
      Right output -> do
        let heading = "session_id: " <> jobKey retained <> "\n" <> statusText (Cmd.presentedStatus observed)
            payloadBudget = max 0 (Cmd.presentedByteBudget observed - utf8Bytes (heading <> "\n"))
            (firstBody, initiallyOmitted) = displayWith payloadBudget output
            recovery = if initiallyOmitted then "\nRecover retained output with read_output(session_id=\"" <> jobKey retained <> "\", stream=\"Stdout\" or \"Stderr\", offset=<next_offset>)." else ""
            body = if initiallyOmitted then fst (displayWith (max 0 (payloadBudget - utf8Bytes recovery)) output) else firstBody
            omitted = initiallyOmitted
            out = Cmd.commandStdout output
            err = Cmd.commandStderr output
            linesShown = lineCount (Cmd.outputText out) + lineCount (Cmd.outputText err)
            text = heading <> "\n" <> body <> recovery
         in pure (observedResult retained (Cmd.presentedStatus observed) (Just (Cmd.outputAvailableEnd out)) (Just (Cmd.outputAvailableEnd err)) linesShown (not omitted) text)

observedResult :: Cmd.Job -> Cmd.CommandStatus -> Maybe Int -> Maybe Int -> Int -> Bool -> Text -> CommandToolResult
observedResult job status outEnd errEnd linesShown isComplete text =
  result
    (ObservedCommand (jobKey job) (commandState status) (statusSuccess status) (statusOutcome status) (statusCleanup status) outEnd errEnd linesShown isComplete Nothing)
    text

result :: CommandToolFacts -> Text -> CommandToolResult
result semantic text = CommandToolResult semantic text

attachBinding :: Text -> Text -> CommandToolResult -> CommandToolResult
attachBinding key binding CommandToolResult {facts = ObservedCommand {session_id = observedKey, state = currentState, successful = success, outcome = commandOutcome, cleanup = commandCleanup, stdout_end = stdoutEnd, stderr_end = stderrEnd, payload_lines = linesShown, complete = isComplete}, presentation = text} =
  let bindingLine = "retained as " <> binding <> " :: Cmd.Job"
      (first, rest) = T.breakOn "\n" text
      withReference = if key == observedKey && "session_id: " `T.isPrefixOf` first then first <> "\n" <> bindingLine <> rest else bindingLine <> "\n" <> text
   in result (ObservedCommand observedKey currentState success commandOutcome commandCleanup stdoutEnd stderrEnd linesShown isComplete (Just binding)) withReference
attachBinding _ _ other = other

rejected :: CommandOperation -> Text -> SideEffectDisposition -> CommandToolResult
rejected name reason effect = result (Rejected name Nothing reason effect) ("Rejected · " <> reason)

jobKey :: Cmd.Job -> Text
jobKey (Job key) = key

commandState :: Cmd.CommandStatus -> CommandState
commandState Cmd.CommandQueued = Queued
commandState Cmd.CommandStarting = Starting
commandState Cmd.CommandRunning = Running
commandState Cmd.CommandStopping = Stopping
commandState (Cmd.CommandFinished _) = Finished

statusSuccess :: Cmd.CommandStatus -> Maybe Bool
statusSuccess (Cmd.CommandFinished value) = Just (Cmd.commandOutcome value == Cmd.CommandExited 0)
statusSuccess _ = Nothing

statusOutcome :: Cmd.CommandStatus -> Maybe CommandOutcomeFact
statusOutcome (Cmd.CommandFinished value) = Just (outcomeFact (Cmd.commandOutcome value))
statusOutcome _ = Nothing

statusCleanup :: Cmd.CommandStatus -> Maybe CommandCleanupFact
statusCleanup (Cmd.CommandFinished value) = Just (cleanupFact (Cmd.commandCleanup value))
statusCleanup _ = Nothing

outcomeFact :: Cmd.CommandOutcome -> CommandOutcomeFact
outcomeFact outcome = case outcome of
  Cmd.CommandExited code -> OutcomeExited code
  Cmd.CommandSignalled signal -> OutcomeSignalled signal
  Cmd.CommandOutOfMemory limit -> OutcomeOutOfMemory limit
  Cmd.CommandCancelled -> OutcomeCancelled
  Cmd.CommandFailed _ -> OutcomeFailed
  Cmd.CommandUnconfirmed _ -> OutcomeUnconfirmed

cleanupFact :: Cmd.CommandCleanup -> CommandCleanupFact
cleanupFact cleanup = case cleanup of
  Cmd.CommandClean -> CleanupClean
  Cmd.CommandRetained -> CleanupRetained
  Cmd.CommandCleanupUnknown _ -> CleanupUnknown

statusText :: Cmd.CommandStatus -> Text
statusText Cmd.CommandQueued = "terminal: no · queued"
statusText Cmd.CommandStarting = "terminal: no · starting"
statusText Cmd.CommandRunning = "terminal: no · running"
statusText Cmd.CommandStopping = "terminal: no · stopping; cancellation not confirmed"
statusText current@(Cmd.CommandFinished _) = "terminal: yes · " <> maybe "unknown" (T.pack . show) (statusOutcome current) <> " · cleanup: " <> maybe "unknown" (T.pack . show) (statusCleanup current)

streamName :: Cmd.CommandStream -> Text
streamName Cmd.Stdout = "stdout"
streamName Cmd.Stderr = "stderr"

lineCount :: Text -> Int
lineCount text
  | T.null text = 0
  | otherwise = length (T.lines text)

utf8Bytes :: Text -> Int
utf8Bytes = T.foldl' (\n c -> n + if ord c < 0x80 then 1 else if ord c < 0x800 then 2 else if ord c < 0x10000 then 3 else 4) 0

boundedResult :: Int -> CommandToolResult -> CommandToolResult
boundedResult budget CommandToolResult {facts = semantic, presentation = text}
  | utf8Bytes text <= budget = CommandToolResult semantic text
  | otherwise =
      let suffix = "\n[presentation truncated; structured facts remain available]"
          bounded = utf8Prefix (max 0 (budget - utf8Bytes suffix)) text <> suffix
          boundedFacts = case semantic of
            ObservedCommand {session_id = key, state = currentState, successful = success, outcome = commandOutcome, cleanup = commandCleanup, stdout_end = stdoutEnd, stderr_end = stderrEnd, payload_lines = linesShown, retained_binding = binding} ->
              ObservedCommand key currentState success commandOutcome commandCleanup stdoutEnd stderrEnd linesShown False binding
            OutputPage {session_id = key, stream = selectedStream, start_offset = start, end_offset = end, next_offset = next, retained_end = retained, eof = atEnd, lost_bytes = lost, lossy = isLossy, payload_lines = linesShown} ->
              OutputPage key selectedStream start end next retained atEnd lost isLossy linesShown False
            other -> other
       in CommandToolResult boundedFacts bounded

utf8Prefix :: Int -> Text -> Text
utf8Prefix budget = go budget
  where
    go remaining text = case T.uncons text of
      Nothing -> ""
      Just (c, rest)
        | width c <= remaining -> T.cons c (go (remaining - width c) rest)
        | otherwise -> ""
    width c
      | ord c < 0x80 = 1
      | ord c < 0x800 = 2
      | ord c < 0x10000 = 3
      | otherwise = 4

budgetForBinding :: Cmd.Observation -> Text -> Cmd.Observation
budgetForBinding observation binding =
  observation {Cmd.outputBytes = max 0 (Cmd.outputBytes observation - utf8Bytes ("retained as " <> binding <> " :: Cmd.Job\n"))}
