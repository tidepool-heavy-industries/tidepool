{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Tidepool.Command.Tools
  ( ShellTools (..),
    Execute (..),
    WriteInput (..),
    ReadOutput (..),
    CancelCommand (..),
    Stream (..),
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
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import Tidepool.Command.Types (Job (..))
import Tidepool.Effects.Core (CommandPresentation (..), Commands (..))
import Tidepool.Inspection (Display (..))

data Execute = Execute
  { cmd :: Text,
    workdir :: Maybe Text,
    environment :: Maybe (Map.Map Text Text),
    memory_mib :: Maybe Int,
    tty :: Maybe Bool,
    stdin :: Maybe Bool,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int,
    intent :: Maybe Text,
    focus :: Maybe Text
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
  { bash :: mode :- Call Execute Text,
    writeStdin :: mode :- Call WriteInput Text,
    readOutput :: mode :- Call ReadOutput Text,
    cancelCommand :: mode :- Call CancelCommand Text
  }
  deriving (Generic)

-- | A composable policy for one command observation.  The shared shell owns
-- validation, process/input handling and retained jobs; a workspace may only
-- replace how a successful observation is prepared for display.
type ObservationPresenter effects = Maybe Text -> Maybe Text -> Maybe Text -> Cmd.Observation -> Cmd.Job -> Eff effects Text

tools :: (Member Cmd.Commands effects) => ShellTools (AsServerT (Eff effects))
tools = toolsWith defaultPresenter

toolsWith :: (Member Cmd.Commands effects) => ObservationPresenter effects -> ShellTools (AsServerT (Eff effects))
toolsWith presenter =
  ShellTools
    { bash =
        tool
          "Execute Bash once; no shell profiles. Optional workdir/environment, memory_mib (default 256), tty or piped stdin. Observe 0..300000ms (default 30000); output max_output_bytes clamps to 1024..32768 bytes (default 32768; any positive value is accepted). Returns session_id and a retained Cmd.Job. Observation expiry leaves execution alive: use write_stdin to observe, read_output to recover output. intent supplies the command's purpose to its presenter. focus, when present, filters the output to the sections relevant to that text (example: \"the failing test and its assertion\"); omit for plain bounded output."
          (executeWith presenter),
      writeStdin =
        tool
          "Observe an existing session_id; optional chars sends input first. Empty/omitted chars only observes. close_stdin sends final bytes then EOF (pipes only). Write acknowledgment does not prove consumption; never replay uncertain input. PTYs accept control characters. Observe 0..300000ms (default 250); output max_output_bytes clamps to 1024..32768 bytes (any positive value is accepted)."
          (writeInputWith presenter),
      readOutput =
        tool
          "Read retained output; no execution, waiting, or consumption. Defaults: Stdout, byte offset 0, 8192 bytes; max_output_bytes clamps to 1024..32768 including metadata (any positive value is accepted). Contiguous pages report next_offset, EOF/current end, and retention gaps. Use Stderr for diagnostics."
          readRetained,
      cancelCommand =
        tool
          "Request cancellation, then observe the same session_id (0..300000ms, default 250). Works for queued jobs, pipes, and PTYs. Acknowledgment is not terminal/cleanup confirmation. Repeated cancellation preserves the outcome; output remains retained."
          cancelRetained
    }

-- Validate observation options before starting a process or sending input.
-- An invalid option is reported as text; nothing is started or sent.
-- max_output_bytes is clamped into the supported range rather than rejected:
-- any positive request is honored, just at whatever budget the range allows,
-- and the presented output already carries a recovery hint when it is cut
-- short by that budget.
observation :: Int -> Maybe Int -> Maybe Int -> Either Text Cmd.Observation
observation defaultWait wait limit
  | milliseconds < 0 || milliseconds > 300000 = Left "Rejected · nothing started or sent · yield_time_ms must be 0..300000"
  | requested <= 0 = Left "Rejected · nothing started or sent · max_output_bytes must be positive"
  | otherwise = Right (Cmd.Observation milliseconds bytes)
  where
    milliseconds = fromMaybe defaultWait wait
    requested = fromMaybe 32768 limit
    bytes = max 1024 (min 32768 requested)

execute :: (Member Cmd.Commands effects) => Execute -> Eff effects Text
execute = executeWith defaultPresenter

executeWith :: (Member Cmd.Commands effects) => ObservationPresenter effects -> Execute -> Eff effects Text
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
      focus = focus
    } =
    case observation 30000 wait limit of
      Left rejection -> pure rejection
      Right options -> do
        let command =
              maybe id Cmd.inDirectory directory $
                Cmd.withEnvironment (maybe [] Map.toList env) $
                  Cmd.withMemory (Cmd.MiB (fromMaybe 256 memory)) $
                    input (Cmd.bashCommand script)
            input =
              if fromMaybe False terminal
                then Cmd.withTerminal
                else if fromMaybe False pipe then Cmd.withStdin else id
        started <- Cmd.tryStart command
        case started of
          Left Cmd.CommandUnauthorized ->
            pure "Rejected · command not started · this actor has no command authority"
          Left issue -> pure $ "Rejected · command not started · " <> T.pack (show issue)
          Right retained -> do
            presenter (Just script) purpose focus options retained

writeInput :: (Member Cmd.Commands effects) => WriteInput -> Eff effects Text
writeInput = writeInputWith defaultPresenter

writeInputWith :: (Member Cmd.Commands effects) => ObservationPresenter effects -> WriteInput -> Eff effects Text
writeInputWith presenter WriteInput {session_id = key, chars = input, close_stdin = close, yield_time_ms = wait, max_output_bytes = limit} =
  case observation 250 wait limit of
    Left rejection -> pure rejection
    Right options -> do
      let text = fromMaybe "" input
          eof = fromMaybe False close
      receipt <- case (T.null text, eof) of
        (True, False) -> pure (Right ())
        (True, True) -> send (CommandCloseInputWith key)
        (False, False) -> send (CommandInputWith key text)
        (False, True) -> send (CommandFinishInputWith key text)
      case receipt of
        Left (Cmd.CommandInputRejected detail) ->
          pure $ "session_id: " <> key <> "\nRejected · input not submitted (including chars); EOF not submitted · " <> detail
        Left Cmd.CommandUnauthorized ->
          pure $ "session_id: " <> key <> "\nRejected · input not submitted (including chars); EOF not submitted · input control is not authorized"
        Left (Cmd.CommandInputAcceptedCloseUnconfirmed detail) ->
          pure $ "session_id: " <> key <> "\nBackend acknowledged the write; child consumption is unknown. EOF unconfirmed: " <> detail <> "\nRetry close-only with write_stdin(close_stdin=true), without chars. Do not resend these bytes."
        Left issue -> pure $ "session_id: " <> key <> "\nInput submission unconfirmed: " <> T.pack (show issue) <> "\nInspect the same job before recovery. Do not replay input after an uncertain acknowledgment."
        Right () ->
          let receipt =
                if T.null text
                  then ""
                  else "Input acknowledged by backend; child consumption is unknown."
              closed = if eof then "Stdin is closed." else ""
           in -- Sending bytes (or EOF) is an irreversible action. Return its
              -- acknowledgment directly: a later, optional output presentation
              -- must not turn a successful write into an apparent failed call.
              if not (T.null text) || eof
                then pure $ T.intercalate "\n" (filter (not . T.null) [receipt, closed])
                -- Empty input is an observation poll and keeps the configured
                -- presenter behavior.
                else presenter Nothing Nothing Nothing options (Job key)

cancelRetained :: (Member Cmd.Commands effects) => CancelCommand -> Eff effects Text
cancelRetained CancelCommand {session_id = key, yield_time_ms = wait, max_output_bytes = limit} =
  case observation 250 wait limit of
    Left rejection -> pure rejection
    Right options -> do
      receipt <- send (CommandCancelWith key)
      case receipt of
        Left issue -> pure $ "session_id: " <> key <> "\nCancellation unconfirmed: " <> T.pack (show issue) <> "\nInspect the same job; do not start a replacement."
        Right () -> do
          current <- Cmd.observe options (Job key)
          pure $ case current of
            Cmd.CommandFinished _ -> ""
            _ -> "Cancellation requested; terminal outcome and cleanup are not yet confirmed."

readRetained :: (Member Cmd.Commands effects) => ReadOutput -> Eff effects Text
readRetained ReadOutput {session_id = key, stream = selected, offset = position, max_output_bytes = limit} =
  case observation 0 (Just 0) (Just (fromMaybe 8192 limit)) of
    Left rejection -> pure rejection
    Right Cmd.Observation {outputBytes = budget} -> do
      let selectedStream = case selected of
            Just Stderr -> Cmd.Stderr
            _ -> Cmd.Stdout
          offset = fromMaybe 0 position
          contentBudget = budget - 512
          read bytes = send (CommandReadWith key selectedStream (Cmd.OutputSlice offset bytes))
      first <- read contentBudget
      -- Replacement characters can expand invalid UTF-8. Ask the byte owner for
      -- a smaller page rather than inventing offsets from decoded text.
      result <- case first of
        Right page | utf8Bytes (Cmd.outputText page) > contentBudget -> read (max 1 (contentBudget `div` 3))
        _ -> pure first
      case result of
        Left Cmd.CommandOutputPending -> pure $ "session_id: " <> key <> "\nNo output yet; streams are starting."
        Left issue -> pure $ "session_id: " <> key <> "\nOutput unavailable: " <> T.pack (show issue) <> "\nInspect the same job; do not rerun the command."
        Right details -> do
          -- A read does not start or wait on anything, but a confirmed page
          -- still names the job's retained Haskell binding, same as every
          -- other direct command tool. The small budget only covers this
          -- heading; the page itself is never refetched here.
          _ <- send (CommandPresentWith key (CommandVisible ("Read retained output · session_id: " <> key <> "\nnext: read_output at next_offset; EOF describes this stream, not command success") 512))
          let (text, _) = displayWith budget details
          pure $ T.pack (show selectedStream) <> " · " <> text <> "\nnext_offset: " <> T.pack (show (Cmd.outputEnd details))

utf8Bytes :: Text -> Int
utf8Bytes = T.foldl' (\n c -> n + width c) 0
  where
    width c
      | ord c < 0x80 = 1
      | ord c < 0x800 = 2
      | ord c < 0x10000 = 3
      | otherwise = 4

defaultPresenter :: (Member Cmd.Commands effects) => ObservationPresenter effects
defaultPresenter _ _ _ options retained = Cmd.observe options retained >> pure ""
