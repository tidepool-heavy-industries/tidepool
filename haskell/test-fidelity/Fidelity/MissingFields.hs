-- | Partial records remain valid source with retained diagnostics. Warning
-- policy must not silently reject a workbench unit or erase its warning.
module Fidelity.MissingFields (checks) where

import Fidelity.Harness (Check, check, extractBindingWithWarnings)
import Data.List (isInfixOf)

checks :: IO [Check]
checks = sequence [missingFieldCheck, completeRecordCheck]

missingFieldCheck :: IO Check
missingFieldCheck = do
  result <- extractBindingWithWarnings
    "missing-record-field-warning" "MissingRecordField"
    (fixtureSource "ConfigRecord { configName = \"partial\" }") "config"
  pure $ case result of
    Left err -> check ("partial record should extract with a warning: " ++ err) False
    Right (_, warnings) -> check
      ("missing record field survives as a named warning: " ++ show warnings)
      (any (isInfixOf "distinctiveRequiredLimit") warnings)

completeRecordCheck :: IO Check
completeRecordCheck = do
  result <- extractBindingWithWarnings
    "missing-record-field-complete-control" "MissingRecordField"
    (fixtureSource "ConfigRecord { configName = \"complete\", distinctiveRequiredLimit = 7 }") "config"
  pure $ case result of
    Left err -> check ("complete record extracts successfully: " ++ err) False
    Right (_, warnings) -> check "complete record has no missing-field warning"
      (not (any (isInfixOf "distinctiveRequiredLimit") warnings))

fixtureSource :: String -> String
fixtureSource construction = unlines
  [ "module MissingRecordField where"
  , ""
  , "data ConfigRecord = ConfigRecord"
  , "  { configName :: String"
  , "  , distinctiveRequiredLimit :: Int"
  , "  }"
  , ""
  , "config :: ConfigRecord"
  , "config = " ++ construction
  ]
