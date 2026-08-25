-- | Missing record fields are fatal in user modules compiled by the extract
-- pipeline.  Without this policy GHC builds a partial value whose exception
-- can remain hidden until the extracted program is already running.
module Fidelity.MissingFields (checks) where

import Fidelity.Harness (Check, check, extractBinding, extractError)

checks :: IO [Check]
checks = sequence [missingFieldCheck, completeRecordCheck]

missingFieldCheck :: IO Check
missingFieldCheck = do
  (ok, err) <-
    extractError
      "missing-record-field-fatal"
      "MissingRecordField"
      (fixtureSource "ConfigRecord { configName = \"unsafe\" }")
      "config"
      "distinctiveRequiredLimit"
  pure $ check ("missing record field fails extraction and names the field: " ++ err) ok

-- | Anti-vacuity control: the same record and target extract successfully
-- when the distinctively named field is supplied.
completeRecordCheck :: IO Check
completeRecordCheck = do
  result <-
    extractBinding
      "missing-record-field-complete-control"
      "MissingRecordField"
      (fixtureSource "ConfigRecord { configName = \"safe\", distinctiveRequiredLimit = 7 }")
      "config"
  pure $ case result of
    Left err -> check ("complete record extracts successfully (anti-vacuity control): " ++ err) False
    Right _ -> check "complete record extracts successfully (anti-vacuity control)" True

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
