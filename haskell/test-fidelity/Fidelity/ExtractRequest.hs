module Fidelity.ExtractRequest (checks) where

import Fidelity.Harness (Check, check)
import Tidepool.ExtractRequest
  ( InspectionRequest(..), RequestField(..), WorkerRequest(..)
  , workerArgv, workerRequestFromArgv )

checks :: IO [Check]
checks = pure
  [ check "typed request decodes fields and a binary generation" typedRequestDecodes
  , check "typed inspection request retains query and output" typedInspectionDecodes
  , check "unknown request versions are rejected" wrongVersionRejected
  , check "retired request flags are rejected" retiredFlagsRejected
  , check "unknown field tags are rejected" unknownTagRejected
  , check "retired session-bind field tags are rejected explicitly" retiredTagsRejected
  , check "truncated fields are rejected" truncatedFieldRejected
  ]

typedInspectionDecodes :: Bool
typedInspectionDecodes = case workerRequestFromArgv
  (workerArgv
    [ Input "Expr.hs"
    , InspectType "fmap"
    , InspectInfo "Maybe"
    , InspectBrowseExpanded "Tidepool.Actors.Shoal"
    , InspectOut "inspection.cbor"
    ]) of
  Right (Just request) ->
    requestInspections request ==
      [ InspectTypeOf "fmap"
      , InspectNameInfo "Maybe"
      , InspectModule "Tidepool.Actors.Shoal" True
      ]
      && requestInspectOut request == Just "inspection.cbor"
  _ -> False

typedRequestDecodes :: Bool
typedRequestDecodes = case workerRequestFromArgv ["--worker-request-v6", payload] of
  Right (Just request) -> requestFiles request == ["x"] && requestBindGen request == Just 42
  _ -> False
  where
    payload = "5450524551303036020000000101000000780b2a00000000000000"

wrongVersionRejected :: Bool
wrongVersionRejected = all rejected
  ["5450524551303031", "5450524551303033", "5450524551303034", "5450524551303035"]
  where
    rejected magic = isLeft (workerRequestFromArgv ["--worker-request-v6", magic ++ "00000000"])

retiredFlagsRejected :: Bool
retiredFlagsRejected = all rejected
  [ "--worker-request-v1", "--worker-request-v2", "--worker-request-v3"
  , "--worker-request-v4", "--worker-request-v5"
  ]
  where
    rejected flag = isLeft (workerRequestFromArgv [flag, validPayload])
    validPayload = "545052455130303600000000"

unknownTagRejected :: Bool
unknownTagRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v6", "545052455130303601000000ff"])

retiredTagsRejected :: Bool
retiredTagsRejected = all retired [9, 10, 14]
  where
    retired tag = workerRequestFromArgv
      ["--worker-request-v6", "545052455130303601000000" ++ byteHex tag]
      == Left ("worker request: retired field tag " ++ show tag)
    byteHex n = ["0123456789abcdef" !! (n `div` 16), "0123456789abcdef" !! (n `mod` 16)]

truncatedFieldRejected :: Bool
truncatedFieldRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v6", "545052455130303601000000010500000078"])

isLeft :: Either a b -> Bool
isLeft (Left _) = True
isLeft (Right _) = False
