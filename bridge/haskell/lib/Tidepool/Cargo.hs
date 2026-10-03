{-# LANGUAGE MonoLocalBinds, DuplicateRecordFields, FlexibleContexts, OverloadedStrings, ScopedTypeVariables, OverloadedRecordDot #-}

-- | Cargo effect module: typed wrappers over 'runArgv' that parse
-- @--message-format=json@ line-delimited output into 'CargoReport' values.
--
-- Lens-free: deconstruct 'Value' with @KM.lookup@ + case on
-- @Object@\/@Array@\/@String@, not optics (@^?@\/@key@\/_String@).
module Tidepool.Cargo
  ( CargoError (..)
  , CargoReport (..)
  , cargoReportFrom
  , cargoCheck
  , cargoClippy
  , cargoMetadata
  ) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson.Value (Value)
import Tidepool.Aeson.FromJSON (eitherDecode)
import Tidepool.Records (Proc (..))
import Tidepool.Effects (Exec, ExecError, M, runArgv)
import qualified Tidepool.Shell as Shell

-- | The command failed to execute, or a non-empty JSON output line was invalid.
-- Line indices are one-based and include blank lines in the original output.
data CargoError
  = CargoExecutionError ExecError
  | CargoInvalidJson Int Text
  deriving (Eq, Show)

-- | Parsed Cargo output. A nonzero 'exitCode' remains data so callers can
-- inspect compiler diagnostics and stderr before deciding how to proceed.
data CargoReport = CargoReport
  { exitCode :: Int,
    stderr :: Text,
    messages :: [Value]
  }
  deriving (Eq, Show)

-- | Run @cargo check --message-format=json [extras]@.
--
-- > Right report <- cargoCheck []
-- > let diagnostics = report.messages
-- >     failed = report.exitCode /= 0
cargoCheck :: Member Exec effects => [Text] -> Eff effects (Either CargoError CargoReport)
cargoCheck extras = runCargoJson ("check" : "--message-format=json" : extras)

-- | Run @cargo clippy --message-format=json [extras]@. Same result as
-- 'cargoCheck', including nonzero exits as reports.
cargoClippy :: Member Exec effects => [Text] -> Eff effects (Either CargoError CargoReport)
cargoClippy extras = runCargoJson ("clippy" : "--message-format=json" : extras)

-- | Run @cargo metadata --format-version=1@ and return the parsed 'Value'.
-- This keeps its existing throwing contract.
cargoMetadata :: M Value
cargoMetadata = Shell.shJson ["cargo", "metadata", "--format-version=1"]

runCargoJson :: Member Exec effects => [Text] -> Eff effects (Either CargoError CargoReport)
runCargoJson subArgs = do
  result <- runArgv ("cargo" : subArgs)
  pure (cargoReportFrom result)

-- | Interpret captured Cargo execution data without imposing an exit-code
-- policy. Useful when a caller has retained a 'Proc' and wants the same
-- strict JSON-line parsing as 'cargoCheck'.
cargoReportFrom :: Either ExecError Proc -> Either CargoError CargoReport
cargoReportFrom result = do
  proc <- either (Left . CargoExecutionError) Right result
  parsed <- traverse parseLine (zip [1 ..] (T.lines proc.stdout))
  pure
    ( CargoReport
        { exitCode = proc.exitCode,
          stderr = proc.stderr,
          messages = [value | Just value <- parsed]
        }
    )
  where
    parseLine (_, line) | T.null line = Right Nothing
    parseLine (lineIndex, line) =
      either (Left . CargoInvalidJson lineIndex) (Right . Just) (eitherDecode line :: Either Text Value)
