module Fidelity.ExtractRequest (checks) where

import Fidelity.Harness (Check, check)
import Tidepool.ExtractRequest (WorkerRequest(..), workerRequestFromArgv)

checks :: IO [Check]
checks = pure
  [ check "typed request decodes fields and a binary generation" typedRequestDecodes
  , check "unknown request versions are rejected" wrongVersionRejected
  , check "unknown field tags are rejected" unknownTagRejected
  , check "truncated fields are rejected" truncatedFieldRejected
  ]

typedRequestDecodes :: Bool
typedRequestDecodes = case workerRequestFromArgv ["--worker-request-v1", payload] of
  Right (Just request) -> requestFiles request == ["x"] && requestBindGen request == Just 42
  _ -> False
  where
    payload = "5450524551303031020000000101000000780b2a00000000000000"

wrongVersionRejected :: Bool
wrongVersionRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v1", "424144564552303100000000"])

unknownTagRejected :: Bool
unknownTagRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v1", "545052455130303101000000ff"])

truncatedFieldRejected :: Bool
truncatedFieldRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v1", "545052455130303101000000010500000078"])

isLeft :: Either a b -> Bool
isLeft (Left _) = True
isLeft (Right _) = False
