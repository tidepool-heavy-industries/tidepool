module Fidelity.ExtractRequest (checks) where

import Fidelity.Harness (Check, check)
import Tidepool.ExtractRequest (WorkerRequest(..), workerRequestFromArgv)

checks :: IO [Check]
checks = pure
  [ check "typed request decodes fields and a binary generation" typedRequestDecodes
  , check "unknown request versions are rejected" wrongVersionRejected
  , check "retired request flags are rejected" retiredFlagsRejected
  , check "unknown field tags are rejected" unknownTagRejected
  , check "retired session-bind field tags are rejected explicitly" retiredTagsRejected
  , check "truncated fields are rejected" truncatedFieldRejected
  ]

typedRequestDecodes :: Bool
typedRequestDecodes = case workerRequestFromArgv ["--worker-request-v3", payload] of
  Right (Just request) -> requestFiles request == ["x"] && requestBindGen request == Just 42
  _ -> False
  where
    payload = "5450524551303033020000000101000000780b2a00000000000000"

wrongVersionRejected :: Bool
wrongVersionRejected = all rejected ["5450524551303031", "5450524551303032"]
  where
    rejected magic = isLeft (workerRequestFromArgv ["--worker-request-v3", magic ++ "00000000"])

retiredFlagsRejected :: Bool
retiredFlagsRejected = all rejected ["--worker-request-v1", "--worker-request-v2"]
  where
    rejected flag = isLeft (workerRequestFromArgv [flag, validPayload])
    validPayload = "545052455130303300000000"

unknownTagRejected :: Bool
unknownTagRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v3", "545052455130303301000000ff"])

retiredTagsRejected :: Bool
retiredTagsRejected = all retired [9, 10, 14]
  where
    retired tag = workerRequestFromArgv
      ["--worker-request-v3", "545052455130303301000000" ++ byteHex tag]
      == Left ("worker request: retired field tag " ++ show tag)
    byteHex n = ["0123456789abcdef" !! (n `div` 16), "0123456789abcdef" !! (n `mod` 16)]

truncatedFieldRejected :: Bool
truncatedFieldRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v3", "545052455130303301000000010500000078"])

isLeft :: Either a b -> Bool
isLeft (Left _) = True
isLeft (Right _) = False
