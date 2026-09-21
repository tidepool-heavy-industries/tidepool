module Fidelity.ExtractRequest (checks) where

import Fidelity.Harness (Check, check)
import Tidepool.ExtractRequest
  ( InspectionProvenance(..), InspectionRequest(..), RequestField(..)
  , StructuredInspection(..), StructuredNameNamespace(..)
  , StructuredNameScope(..), WorkerRequest(..)
  , workerArgv, workerRequestFromArgv )

checks :: IO [Check]
checks = pure
  [ check "typed request decodes fields and a binary generation" typedRequestDecodes
  , check "typed inspection request retains query and output" typedInspectionDecodes
  , check "structured inspection retains scope namespace and provenance" structuredInspectionDecodes
  , check "prepared turn field decodes beside turn mode" preparedTurnDecodes
  , check "activation preview field decodes as an internal request" activationPreviewDecodes
  , check "strict inspection field preserves request-level diagnostics" strictInspectionDecodes
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
    , InspectSearch "Response result -> Await (Settlement result)"
    , InspectOut "inspection.cbor"
    , InspectTypeBatch "type-batch/Expr.hs"
    ]) of
  Right (Just request) ->
    requestInspections request ==
      [ InspectTypeOf "fmap"
      , InspectNameInfo "Maybe"
      , InspectModule "Tidepool.Actors.Shoal" True
      , InspectTypeSearch "Response result -> Await (Settlement result)"
      ]
      && requestInspectOut request == Just "inspection.cbor"
      && requestInspectTypeBatch request == Just "type-batch/Expr.hs"
  _ -> False

structuredInspectionDecodes :: Bool
structuredInspectionDecodes = case workerRequestFromArgv
  (workerArgv
    [ Input "Expr.hs"
    , InspectStructuredInfo current
    , InspectStructuredType public
    , InspectOut "inspection.cbor"
    ]) of
  Right (Just request) -> requestInspections request ==
    [InspectStructuredInfoOf current, InspectStructuredTypeOf public]
  _ -> False
  where
    provenance = InspectionProvenance 17 "scope-fingerprint"
    current = StructuredInspection
      StructuredCurrentScope StructuredAnyName "WorkProgress" provenance
    public = StructuredInspection
      (StructuredPublicModule "Project.Work") StructuredTypeName "Task" provenance

preparedTurnDecodes :: Bool
preparedTurnDecodes = case (decoded [Input "turn.txt", Turn], decoded [Input "turn.txt", Turn, PreparedTurn]) of
  (Right (Just plain), Right (Just prepared)) ->
    not (requestPreparedTurn plain) && requestTurn prepared && requestPreparedTurn prepared
  _ -> False
  where
    decoded = workerRequestFromArgv . workerArgv

activationPreviewDecodes :: Bool
activationPreviewDecodes = case workerRequestFromArgv (workerArgv [ActivationPreview]) of
  Right (Just request) -> requestActivationPreview request
  _ -> False

strictInspectionDecodes :: Bool
strictInspectionDecodes = case workerRequestFromArgv (workerArgv [InspectionStrict]) of
  Right (Just request) -> requestInspectionStrict request
  _ -> False

typedRequestDecodes :: Bool
typedRequestDecodes = case workerRequestFromArgv ["--worker-request-v8", payload] of
  Right (Just request) -> requestFiles request == ["x"] && requestBindGen request == Just 42
  _ -> False
  where
    payload = "5450524551303038020000000101000000780b2a00000000000000"

wrongVersionRejected :: Bool
wrongVersionRejected = all rejected
  [ "5450524551303031", "5450524551303032", "5450524551303033"
  , "5450524551303034", "5450524551303035", "5450524551303036"
  , "5450524551303037"
  ]
  where
    rejected magic = isLeft (workerRequestFromArgv ["--worker-request-v8", magic ++ "00000000"])

retiredFlagsRejected :: Bool
retiredFlagsRejected = all rejected
  [ "--worker-request-v1", "--worker-request-v2", "--worker-request-v3"
  , "--worker-request-v4", "--worker-request-v5", "--worker-request-v6"
  , "--worker-request-v7"
  ]
  where
    rejected flag = isLeft (workerRequestFromArgv [flag, validPayload])
    validPayload = "545052455130303800000000"

unknownTagRejected :: Bool
unknownTagRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v8", "545052455130303801000000ff"])

retiredTagsRejected :: Bool
retiredTagsRejected = all retired [9, 10, 14]
  where
    retired tag = workerRequestFromArgv
      ["--worker-request-v8", "545052455130303801000000" ++ byteHex tag]
      == Left ("worker request: retired field tag " ++ show tag)
    byteHex n = ["0123456789abcdef" !! (n `div` 16), "0123456789abcdef" !! (n `mod` 16)]

truncatedFieldRejected :: Bool
truncatedFieldRejected = isLeft (workerRequestFromArgv
  ["--worker-request-v8", "545052455130303801000000010500000078"])

isLeft :: Either a b -> Bool
isLeft (Left _) = True
isLeft (Right _) = False
