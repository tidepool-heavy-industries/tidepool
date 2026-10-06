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
    CommandState (..),
    CommandOperation (..),
    OutputStreamFact (..),
    ReceiptDisposition (..),
    EofDisposition (..),
    SideEffectDisposition (..),
    CancellationDisposition (..),
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
import Tidepool.Inspection (Display (..), DisplayTree (..), WorkbenchDisplay (..))

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
        stream :: OutputStreamFact,
        start_offset :: Int,
        end_offset :: Int,
        next_offset :: Int,
        retained_end :: Int,
        at_eof :: Bool,
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
        session_reference :: Maybe Text,
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

data CommandOperation = OpBash | OpWriteStdin | OpReadOutput | OpCancelCommand
  deriving (Eq, Generic, FromJSON, JsonSchema, ToJSON)

data OutputStreamFact = OutputStdout | OutputStderr
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
          "Run Bash once; no shell profiles. Default: wait for terminal completion under invocation ownership. yield_time_ms (0..300000) observes for that long, then retains a running job under actor ownership with no completion notice; use write_stdin to observe it. background:true returns immediately and wakes you on completion with status, output tail and starting commit; ignores focus/yield_time_ms/max_output_bytes. Receipts identify session_id and retained Cmd.Job. Inspect that job after uncertainty; rerunning starts new work. With a selecting presenter, focus names the evidence wanted, e.g. \"failing test and assertion\"; intent supplies purpose to the presenter. read_output pages retained bytes. Options: workdir; environment:[{name,value}] (last duplicate wins); memory_mib positive, default 1024; tty:true or stdin:true for input, mutually exclusive. max_output_bytes: positive, clamped 1024..32768, default 32768. Presented excerpts can omit evidence; page the relevant stream before drawing conclusions."
          (executeWith presenter),
      writeStdin =
        presentWith presentation $ tool
          "Continue an existing session_id. With empty/omitted chars and no close_stdin, observe its state and output for yield_time_ms (0..300000, default 250); no new command starts. With chars or close_stdin:true, send input and return only its write/EOF receipt: acknowledgment proves acceptance, not child consumption. Do not replay uncertain input; accepted bytes and confirmed EOF are separate facts. close_stdin sends final bytes then EOF for pipes; PTYs use control characters and reject close_stdin. Input requires a job started with stdin:true or tty:true. Use a later observation to inspect progress. max_output_bytes is positive and clamps to 1024..32768 bytes (default 32768)."
          (writeInputWith presenter),
      readOutput =
        presentWith presentation $ tool
          "Recover output omitted from a command result or inspect a retained stream by session_id without rerunning, waiting or consuming it. Defaults: stream \"Stdout\", byte offset 0, max_output_bytes 8192; use \"Stderr\" for diagnostics. Offset must be nonnegative; positive byte budgets clamp to 1024..32768 including metadata. Continue from next_offset, not the decoded text length. Current end is not EOF: a running stream may produce more. Pages report retention gaps and lossy decoding; omitted or lost bytes limit what a quiet page can establish. EOF describes this stream, not command success or cleanup. Use write_stdin with no input to observe job state."
          readRetained,
      cancelCommand =
        presentWith presentation $ tool
          "Stop work identified by session_id: request cancellation, then observe that same job for yield_time_ms (0..300000, default 250). Works for queued jobs, pipes and PTYs. Acceptance of cancellation, terminal outcome and cleanup are separate facts; inspect the receipt and use write_stdin with no input if the outcome is still unresolved. Cancellation does not undo completed effects. Repeated cancellation preserves the recorded outcome; output remains available through read_output. max_output_bytes is positive and clamps to 1024..32768 bytes (default 32768)."
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
renderOptionError issue = "nothing started or sent · " <> detail
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
      Left rejection -> pure (rejected OpBash (renderOptionError rejection) NoSideEffect)
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
            pure (rejected OpBash "this actor has no command authority" NoSideEffect)
          Left issue -> pure $ rejected OpBash (T.pack (show issue)) NoSideEffect
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
                  binding <- Cmd.retainJobBinding retained
                  let blockingOptions = options {Cmd.waitMilliseconds = -1}
                  boundedResult (Cmd.outputBytes options) . attachBinding key binding <$> presenter (Just script) purpose focus (budgetForBinding blockingOptions binding) retained
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
        Left rejection -> pure (rejected OpWriteStdin (renderOptionError rejection) NoSideEffect)
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
    Left rejection -> pure (rejected OpCancelCommand (renderOptionError rejection) NoSideEffect)
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
              prefix = case status of
                Right current -> "Cancellation requested. " <> statusText current
                Left _ -> "Cancellation requested; terminal outcome and cleanup could not be confirmed."
          pure (boundedResult budget (result receipt prefix))

readRetained :: (Member Cmd.Commands effects) => ReadOutput -> Eff effects CommandToolResult
readRetained ReadOutput {session_id = key, stream = selected, offset = position, max_output_bytes = limit} =
  case readOptions position limit of
    Left rejection -> pure (rejected OpReadOutput (renderOptionError rejection) NoSideEffect)
    Right (offset, Cmd.Observation {outputBytes = budget}) -> do
      let selectedStream = case selected of
            Just Stderr -> Cmd.Stderr
            _ -> Cmd.Stdout
          contentBudget = budget - 512
          read bytes = send (CommandReadWith key selectedStream (Cmd.OutputSlice offset bytes))
      first <- read contentBudget
      -- Replacement characters can expand invalid UTF-8. Ask the byte owner for
      -- a smaller page rather than inventing offsets from decoded text.
      pageResult <- case first of
        Right page | utf8Bytes (Cmd.outputText page) > contentBudget -> read (max 1 (contentBudget `div` 3))
        _ -> pure first
      case pageResult of
        Left Cmd.CommandOutputPending -> pure $ boundedResult budget (result (OutputPage key (outputStreamFact selectedStream) offset offset offset offset False 0 False 0 False) "No output yet; streams are starting.")
        Left issue ->
          let detail = T.pack (show issue)
              recovery = readRecovery key selectedStream offset
              prefix = "Output unavailable: "
              detailBudget = max 0 (budget - utf8Bytes (prefix <> recovery))
              shownDetail = boundedErrorDetail detailBudget detail
           in pure $ boundedResult budget (result (Rejected OpReadOutput (Just key) detail NoSideEffect) (prefix <> shownDetail <> recovery))
        Right details -> do
          let eof = Cmd.outputFinished details && Cmd.outputEnd details == Cmd.outputAvailableEnd details
              stream = outputStreamFact selectedStream
              payload = Cmd.outputText details
              start = Cmd.outputStart details
              end = Cmd.outputEnd details
              availableEnd = Cmd.outputAvailableEnd details
              retainedStart = Cmd.outputRetainedStart details
              lost = Cmd.outputLostBytes details
              lossy = Cmd.outputLossy details
              header = streamName selectedStream <> " · bytes " <> number start <> ".." <> number end <> " of " <> number availableEnd <> (if eof then " · EOF" else if end < availableEnd then " · more available" else " · current end") <> (if retainedStart > 0 then " · retained from " <> number retainedStart else "") <> (if lost > 0 then " · lost " <> number lost <> " bytes" else "") <> (if lossy then " · lossy UTF-8" else "") <> "\n"
              clipped = "\n[payload clipped by max_output_bytes]"
              pageTail = if eof then "" else "\nnext_offset: " <> number end
              payloadBudget = max 0 (budget - utf8Bytes (header <> pageTail))
              didClip = utf8Bytes payload > payloadBudget
              shownPayload = if didClip then utf8Prefix (max 0 (payloadBudget - utf8Bytes clipped)) payload <> clipped else payload
              text = header <> shownPayload <> pageTail
              complete = start == 0 && retainedStart == 0 && eof && not lossy && lost == 0 && not didClip
              page = OutputPage key stream start end end availableEnd eof lost lossy (lineCount payload) complete
          pure (result page text)

defaultPresenter :: (Member Cmd.Commands effects) => ObservationPresenter effects
defaultPresenter command _ _ options retained = do
  (_, shown) <- Cmd.observeWith options retained render
  pure shown
  where
    render observed = case Cmd.presentedOutput observed of
      Left issue ->
        pure
          ( observedResult retained (Cmd.presentedStatus observed) Nothing Nothing 0 False
              (boundedErrorPresentation (Cmd.presentedByteBudget observed) (heading observed <> "\nOutput unavailable: ") (Cmd.renderCommandError issue) (readRecovery (jobKey retained) Cmd.Stdout 0))
          )
      Right output -> do
        let out = Cmd.commandStdout output
            err = Cmd.commandStderr output
            pages = [(Cmd.Stdout, out), (Cmd.Stderr, err)]
            included = filter (showPage . snd) pages
            headingText = heading observed <> "\n"
            metadataBytes = sum [utf8Bytes (pageHeader stream page <> "\n") + if T.null (Cmd.outputText page) then 0 else 1 | (stream, page) <- included]
            incomplete = filter (not . pageComplete . snd) pages
            outputBytes = sum [utf8Bytes (Cmd.outputText page) | (_, page) <- included]
            maximumRecovery = recoveryText True included
            availableWithMaximumRecovery = max 0 (Cmd.presentedByteBudget observed - utf8Bytes headingText - metadataBytes - utf8Bytes maximumRecovery)
            clipNeeded = outputBytes > availableWithMaximumRecovery
            recoveryPages = if clipNeeded then included else incomplete
            recovery = recoveryText clipNeeded recoveryPages
            availableForPayload = max 0 (Cmd.presentedByteBudget observed - utf8Bytes headingText - metadataBytes - utf8Bytes recovery)
            willClip = outputBytes > availableForPayload
            clippingMarker = "\n[payload clipped by max_output_bytes]"
            payloadBudget = max 0 (availableForPayload - if willClip then utf8Bytes clippingMarker else 0)
            shownPages = allocatePayload payloadBudget included
            payload = T.concat [pageHeader stream page <> "\n" <> shown <> (if T.null shown then "" else "\n") | ((stream, page), shown) <- zip included shownPages]
            text = headingText <> payload <> (if willClip then clippingMarker else "") <> recovery
            linesShown = sum (map lineCount shownPages)
            complete = null incomplete && not willClip
         in pure (observedResult retained (Cmd.presentedStatus observed) (Just (Cmd.outputAvailableEnd out)) (Just (Cmd.outputAvailableEnd err)) linesShown complete text)
    heading observed =
      (if maybe False (const True) command then "session_id: " <> jobKey retained <> "\n" else "")
        <> statusText (Cmd.presentedStatus observed)

    showPage page = Cmd.outputAvailableEnd page > 0 || not (T.null (Cmd.outputText page)) || not (Cmd.outputFinished page)

    pageComplete page =
      Cmd.outputFinished page
        && Cmd.outputStart page == 0
        && Cmd.outputRetainedStart page == 0
        && Cmd.outputEnd page == Cmd.outputAvailableEnd page
        && Cmd.outputLostBytes page == 0
        && not (Cmd.outputLossy page)

    pageHeader stream page =
      streamName stream
        <> " · bytes " <> number (Cmd.outputStart page) <> ".." <> number (Cmd.outputEnd page) <> " of " <> number (Cmd.outputAvailableEnd page)
        <> (if Cmd.outputFinished page && Cmd.outputEnd page == Cmd.outputAvailableEnd page then " · EOF" else if Cmd.outputEnd page < Cmd.outputAvailableEnd page then " · more available" else " · current end")
        <> (if Cmd.outputRetainedStart page > 0 then " · retained from " <> number (Cmd.outputRetainedStart page) else "")
        <> (if Cmd.outputLostBytes page > 0 then " · lost " <> number (Cmd.outputLostBytes page) <> " bytes" else "")
        <> (if Cmd.outputLossy page then " · lossy UTF-8" else "")

    recoveryText clipped pages =
      T.concat
        [ "\nContinue with read_output(session_id=\"" <> jobKey retained <> "\", stream=\"" <> streamArgument stream <> "\", offset=" <> number offset <> ")."
        | (stream, page) <- pages,
          let offset = if clipped || pageComplete page then Cmd.outputStart page else Cmd.outputEnd page
        ]

    streamArgument Cmd.Stdout = "Stdout"
    streamArgument Cmd.Stderr = "Stderr"

    allocatePayload budget pages = allocate budget (sum [utf8Bytes (Cmd.outputText page) | (_, page) <- pages]) pages
    allocate _ _ [] = []
    allocate budget _ [(_, page)] = [utf8Prefix budget (Cmd.outputText page)]
    allocate budget total ((_, page) : rest) =
      let pageBytes = utf8Bytes (Cmd.outputText page)
          share = if total <= 0 then 0 else fromInteger (toInteger budget * toInteger pageBytes `div` toInteger total)
          shown = utf8Prefix share (Cmd.outputText page)
       in shown : allocate (max 0 (budget - utf8Bytes shown)) (max 0 (total - pageBytes)) rest

readRecovery :: Text -> Cmd.CommandStream -> Int -> Text
readRecovery key selectedStream offset =
  "\nRecover retained output with read_output(session_id=\"" <> key <> "\", stream=\"" <> argument <> "\", offset=" <> number offset <> "). Do not rerun the command."
  where
    argument = case selectedStream of
      Cmd.Stdout -> "Stdout"
      Cmd.Stderr -> "Stderr"

boundedErrorPresentation :: Int -> Text -> Text -> Text -> Text
boundedErrorPresentation budget prefix detail recovery =
  let detailBudget = max 0 (budget - utf8Bytes (prefix <> recovery))
   in prefix <> boundedErrorDetail detailBudget detail <> recovery

boundedErrorDetail :: Int -> Text -> Text
boundedErrorDetail budget detail
  | utf8Bytes detail <= budget = detail
  | otherwise =
      let marker = "\n[error detail shortened]"
       in utf8Prefix (max 0 (budget - utf8Bytes marker)) detail <> marker

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

outputStreamFact :: Cmd.CommandStream -> OutputStreamFact
outputStreamFact Cmd.Stdout = OutputStdout
outputStreamFact Cmd.Stderr = OutputStderr

number :: Int -> Text
number = T.pack . show

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
      let suffix = "\n[presentation truncated]"
          bounded = utf8Prefix (max 0 (budget - utf8Bytes suffix)) text <> suffix
          boundedFacts = case semantic of
            ObservedCommand {session_id = key, state = currentState, successful = success, outcome = commandOutcome, cleanup = commandCleanup, stdout_end = stdoutEnd, stderr_end = stderrEnd, payload_lines = linesShown, retained_binding = binding} ->
              ObservedCommand key currentState success commandOutcome commandCleanup stdoutEnd stderrEnd linesShown False binding
            OutputPage {session_id = key, stream = selectedStream, start_offset = start, end_offset = end, next_offset = next, retained_end = retained, at_eof = atEnd, lost_bytes = lost, lossy = isLossy, payload_lines = linesShown} ->
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
