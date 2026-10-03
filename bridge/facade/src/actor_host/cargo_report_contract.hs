{-# LANGUAGE MonoLocalBinds, FlexibleContexts, OverloadedRecordDot, OverloadedStrings #-}

module CargoReportContract where

import Control.Monad.Freer (Eff, Member)
import qualified Tidepool.Cargo as Cargo
import Tidepool.Effects (Exec, ExecError (..), M)

cargoCheckInOpenRow :: Member Exec effects => Eff effects (Either Cargo.CargoError Cargo.CargoReport)
cargoCheckInOpenRow = Cargo.cargoCheck []

cargoClippyInOpenRow :: Member Exec effects => Eff effects (Either Cargo.CargoError Cargo.CargoReport)
cargoClippyInOpenRow = Cargo.cargoClippy []

result :: M (Bool, Bool, Bool, Bool)
result = do
  nonzero <- cargoCheckInOpenRow
  malformedZero <- cargoClippyInOpenRow
  malformedNonzero <- Cargo.cargoCheck ["--fixture-nonzero"]
  executionFailure <- Cargo.cargoClippy ["--fixture-failure"]
  let nonzeroRetained = case nonzero of
        Right report -> report.exitCode == 101 && report.stderr == "compiler failed" && length report.messages == 1
        Left _ -> False
      executionRetained = case executionFailure of
        Left (Cargo.CargoExecutionError (ExecBadDir "unavailable")) -> True
        _ -> False
  pure (nonzeroRetained, isInvalidJson malformedZero, isInvalidJson malformedNonzero, executionRetained)
  where
    isInvalidJson (Left (Cargo.CargoInvalidJson 3 _)) = True
    isInvalidJson _ = False
