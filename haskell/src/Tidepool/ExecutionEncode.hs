{-# LANGUAGE BangPatterns #-}

-- | Deterministic CBOR encoding for the internal prepared-execution schema.
-- The reader owns validation; this module preserves the already-normalized
-- table order and uses only definite-length arrays and primitive leaves.
module Tidepool.ExecutionEncode (encodeWireProgram) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Data.ByteString (ByteString)
import Data.Foldable (fold)
import Data.List (mapAccumL)
import Data.Sequence (Seq, (|>))
import Data.Sequence qualified as Seq
import Data.Word (Word64)
import Tidepool.ExecutionSchema

encodeWireProgram :: WireProgram -> ByteString
encodeWireProgram program = toStrictByteString $ array
  [ encodeString "TPSTG"
  , encodeWord64 (envelopeSchemaVersion envelope)
  , encodeString (envelopeProjectionProfile envelope)
  , encodeString (envelopeToolchain envelope)
  , encodeWord64 (envelopeExecutionAbiVersion envelope)
  , encodeTarget (envelopeTarget envelope)
  , list encodeSignature (programSignatures program)
  , list encodeGlobal (programGlobals program)
  , list encodeConstructor (programConstructors program)
  , list encodeOperation (programOperations program)
  , encodeListLen (fromIntegral (Seq.length frames)) <> fold frames
  , list id bindings
  , encodeValueId (programEntry program)
  , list encodeTypeNode (programTypes program)
  , list encodeSiteRow (programSites program)
  , list encodeVerbSite (programVerbSites program)
  ]
 where
  envelope = programEnvelope program
  ((_, frames), bindings) = mapAccumL encodeTopGroup (0, Seq.empty) (programBindings program)

array :: [Encoding] -> Encoding
array fields = encodeListLen (fromIntegral (length fields)) <> fold fields

list :: (a -> Encoding) -> [a] -> Encoding
list encode values = encodeListLen (fromIntegral (length values)) <> foldMap encode values

encodeTarget :: TargetDescriptor -> Encoding
encodeTarget target = array
  [ encodeWord (case targetArchitecture target of X86_64 -> 0; Aarch64 -> 1)
  , encodeWord (case targetEndianness target of LittleEndian -> 0; BigEndian -> 1)
  , encodeWord8 (targetPointerWidth target)
  , encodeWord8 (targetWordWidth target)
  , encodeString (targetAbi target)
  , list encodeString (targetFeatures target)
  ]

encodeSymbol :: SymbolIdentity -> Encoding
encodeSymbol symbol = array
  [ encodeString (symbolUnit symbol)
  , encodeString (symbolModule symbol)
  , encodeString (symbolNamespace symbol)
  , encodeString (symbolOccurrence symbol)
  , case symbolRecordParent symbol of
      Nothing -> tag 0
      Just parent -> tagged 1 [encodeString parent]
  ]

encodeRep :: RuntimeRep -> Encoding
encodeRep rep = case rep of
  VoidRep -> tag 0
  LiftedRefRep -> tag 1
  UnliftedRefRep -> tag 2
  AddressRep -> tag 3
  IntRep bits -> tagged 4 [encodeWord8 bits]
  WordRep bits -> tagged 5 [encodeWord8 bits]
  FloatRep bits -> tagged 6 [encodeWord8 bits]

encodeSignature :: Signature -> Encoding
encodeSignature signature = array
  [ list encodeRep (signatureArguments signature)
  , encodeResultContract (signatureResults signature)
  ]

encodeResultContract :: ResultContract -> Encoding
encodeResultContract contract = case contract of
  Returns reps -> tagged 0 [list encodeRep reps]
  NoSuccess -> tag 1
  CallerResult -> tag 2

encodeFieldLayout :: FieldLayout -> Encoding
encodeFieldLayout field = array
  [encodeRep (fieldRep field), encodeWord32 (fieldOffset field)]

encodeLayout :: CheckedLayout -> Encoding
encodeLayout layout = array
  [ list encodeFieldLayout (layoutFields layout)
  , encodeWord32 (layoutAlignment layout)
  , encodeWord32 (layoutPayloadSize layout)
  , list encodeBool (layoutRootMask layout)
  ]

encodeConstructor :: ConstructorDecl -> Encoding
encodeConstructor constructor = array
  [ encodeSymbol (constructorIdentity constructor)
  , encodeSymbol (constructorFamily constructor)
  , list encodeRep (constructorFieldReps constructor)
  , list encodeBool (constructorStrictFields constructor)
  , encodeLayout (constructorLayout constructor)
  , encodeRep (constructorResultRep constructor)
  , encodeWord32 (constructorTag constructor)
  , encodeWord32 (constructorFamilySize constructor)
  , encodeWord64 (constructorHostId constructor)
  ]

encodeGlobal :: GlobalDecl -> Encoding
encodeGlobal global = array
  [ encodeSymbol (globalIdentity global)
  , encodeRep (globalRep global)
  , case globalEntrySignature global of
      Nothing -> tag 0
      Just signature -> tagged 1 [encodeSignatureId signature]
  , encodeBool (globalRequiredEvaluated global)
  , case globalRequiredGeneration global of
      Nothing -> tag 0
      Just generation -> tagged 1 [encodeWord64 generation]
  ]

encodeOperation :: OperationDecl -> Encoding
encodeOperation operation = array
  [ case operationIdentity operation of
      PrimOpIdentity name -> tagged 0 [encodeString name]
      IntrinsicIdentity symbol CCall -> tagged 1 [encodeString symbol, tag 0]
      CapabilityIdentity name -> tagged 2 [encodeString name]
      WiredInErrorIdentity kind -> tagged 3 [encodeWord (fromIntegral (fromEnum kind))]
      JsonDecodeIdentity layout left right -> tagged 4
        [encodeJsonLayout layout, encodeConstructorId left, encodeConstructorId right]
      JsonEncodeIdentity layout -> tagged 5 [encodeJsonLayout layout]
  , encodeSignatureId (operationSignature operation)
  ]

encodeJsonLayout :: JsonLayout ConstructorId -> Encoding
encodeJsonLayout layout = array (map encodeConstructorId
  [ jsonObject layout, jsonArray layout, jsonString layout, jsonNumber layout
  , jsonBool layout, jsonNull layout, jsonMapBin layout, jsonMapTip layout
  , jsonTrue layout, jsonFalse layout, jsonCons layout, jsonNil layout
  , jsonScientific layout, jsonIntegerSmall layout, jsonIntegerPositive layout
  , jsonIntegerNegative layout, jsonText layout, jsonInt layout ])

encodeTypeNode :: TypeNode -> Encoding
encodeTypeNode node = case node of
  TypeData family arguments rows -> tagged 0
    [ encodeSymbol family
    , list encodeTypeNodeId arguments
    , list encodeCtorRow rows
    ]
  TypeText -> tag 1
  TypeInteger -> tag 2
  TypeNatural -> tag 3
  TypeScalar rep -> tagged 4 [encodeRep rep]
  TypeUnconstructible reason rendered -> tagged 5
    [encodeString reason, encodeString rendered]

encodeCtorRow :: CtorRow -> Encoding
encodeCtorRow row = array
  [ encodeConstructorId (rowConstructor row)
  , list encodeTypeNodeId (rowFields row)
  ]

encodeSiteRow :: SiteRow -> Encoding
encodeSiteRow site = array
  [ encodeWord64 (siteId site)
  , encodeString (siteOrigin site)
  , encodeWord64 (siteOrdinal site)
  , encodeWord $ case siteDelivery site of
      HostAnswer -> 0
      LiveReentry -> 1
      ExitCellFill -> 2
      TerminalCapture -> 3
  , encodeTypeNodeId (siteWire site)
  , list encodeTypeNodeId (siteInputs site)
  ]

encodeVerbSite :: (ConstructorId, Word64) -> Encoding
encodeVerbSite (constructor, site) =
  array [encodeConstructorId constructor, encodeWord64 site]

encodeValueRef :: ValueRef -> Encoding
encodeValueRef ref = case ref of
  Local value -> tagged 0 [encodeValueId value]
  Global global -> tagged 1 [encodeGlobalId global]

encodeScalar :: ScalarLiteral -> Encoding
encodeScalar scalar = case scalar of
  IntLiteral bits bytes -> tagged 0 [encodeWord8 bits, encodeBytes bytes]
  WordLiteral bits bytes -> tagged 1 [encodeWord8 bits, encodeBytes bytes]
  FloatLiteral bits bytes -> tagged 2 [encodeWord8 bits, encodeBytes bytes]
  BytesLiteral bytes -> tagged 4 [encodeBytes bytes]
  NullAddressLiteral -> tag 5

encodeAtom :: Atom -> Encoding
encodeAtom atom = case atom of
  Ref ref -> tagged 0 [encodeValueRef ref]
  Scalar scalar -> tagged 1 [encodeScalar scalar]
  Void -> tag 2
  Rubbish rep -> tagged 3 [encodeRep rep]

type FlatState = (Int, Seq Encoding)

encodeTopGroup :: FlatState -> Group TopBinding -> (FlatState, Encoding)
encodeTopGroup state group = case group of
  NonRecursive binding ->
    let (next, encoded) = encodeTopBinding state binding
    in (next, tagged 0 [encoded])
  Recursive bindings ->
    let (next, encoded) = mapAccumL encodeTopBinding state bindings
    in (next, tagged 1 [list id encoded])

encodeTopBinding :: FlatState -> TopBinding -> (FlatState, Encoding)
encodeTopBinding state (TopBinding identity (HeapBinding value rhs)) =
  let (next, encodedRhs) = encodeTopRhs state rhs
  in (next, array [encodeSymbol identity, array [encodeValueId value, encodedRhs]])

encodeTopRhs :: FlatState -> HeapRhs -> (FlatState, Encoding)
encodeTopRhs state rhs = case rhs of
  Bytes bytes -> (state, tagged 3 [encodeBytes bytes])
  Function signature parameters captures body ->
    let (next, root) = flattenExprTree state body
    in (next, tagged 0 [encodeSignatureId signature, list encodeValueId parameters,
      list encodeValueRef captures, encodeNodeIndex root])
  Thunk signature update captures body ->
    let (next, root) = flattenExprTree state body
    in (next, tagged 1 [encodeSignatureId signature,
      encodeWord (case update of Memoize -> 0; SingleEntry -> 1),
      list encodeValueRef captures, encodeNodeIndex root])
  Constructor constructor fields ->
    (state, tagged 2 [encodeConstructorId constructor, list encodeAtom fields])

encodePattern :: AlternativePattern -> Encoding
encodePattern pattern_ = case pattern_ of
  DefaultPattern -> tag 0
  ConstructorPattern constructor -> tagged 1 [encodeConstructorId constructor]
  LiteralPattern literal -> tagged 2 [encodeScalar literal]

-- All bodies share one program-wide postorder arena. A local closure, join,
-- or alternative contributes a child frame before its owning parent. Keeping
-- the worklist explicit bounds the Haskell call stack.
data ExprWork = Visit Expr | Finish Expr Int

flattenExprTree :: FlatState -> Expr -> (FlatState, Int)
flattenExprTree (first, frames) root = walk first frames [] [Visit root]
 where
  walk :: Int -> Seq Encoding -> [Int] -> [ExprWork] -> (FlatState, Int)
  walk !next !encoded [rootIndex] [] = ((next, encoded), rootIndex)
  walk !_ !_ _ [] = error "prepared body has no unique root"
  walk !next !encoded !results (Visit expr : work) =
    let children = exprChildren expr
    in walk next encoded results
      (map Visit children ++ Finish expr (length children) : work)
  walk !next !encoded !results (Finish expr arity : work) =
    let (reversedChildren, remaining) = splitAt arity results
        frame = encodeExprFrame expr (reverse reversedChildren)
    in walk (next + 1) (encoded |> frame) (next : remaining) work

exprChildren :: Expr -> [Expr]
exprChildren expr = case expr of
  Case scrutinee _ _ _ alternatives ->
    scrutinee : [body | Alternative _ _ body <- alternatives]
  Let bindings body -> heapGroupBodies bindings ++ [body]
  LetJoins bindings body -> joinGroupBodies bindings ++ [body]
  _ -> []

heapGroupBodies :: Group HeapBinding -> [Expr]
heapGroupBodies group = case group of
  NonRecursive binding -> heapBindingBodies binding
  Recursive bindings -> concatMap heapBindingBodies bindings

heapBindingBodies :: HeapBinding -> [Expr]
heapBindingBodies (HeapBinding _ rhs) = case rhs of
  Function _ _ _ body -> [body]
  Thunk _ _ _ body -> [body]
  _ -> []

joinGroupBodies :: Group JoinBinding -> [Expr]
joinGroupBodies group = case group of
  NonRecursive (JoinBinding _ _ _ body) -> [body]
  Recursive bindings -> [body | JoinBinding _ _ _ body <- bindings]

encodeExprFrame :: Expr -> [Int] -> Encoding
encodeExprFrame expr children = case expr of
  Return atoms -> tagged 0 [list encodeAtom atoms]
  Enter atom signature -> tagged 1 [encodeAtom atom, encodeSignatureId signature]
  Call callee signature arguments -> tagged 2
    [encodeAtom callee, encodeSignatureId signature, list encodeAtom arguments]
  Operation operation arguments -> tagged 3
    [encodeOperationId operation, list encodeAtom arguments]
  Construct constructor fields -> tagged 4
    [encodeConstructorId constructor, list encodeAtom fields]
  Case _ binder resultContract kind alternatives -> case children of
    scrutineeIndex : alternativeIndices -> tagged 5
      [ encodeNodeIndex scrutineeIndex
      , encodeValueId binder
      , encodeResultContract resultContract
      , encodeCaseKind kind
      , list id (zipWith encodeAlternativeFrame alternatives alternativeIndices)
      ]
    [] -> error "case frame has no scrutinee"
  Let bindings _ -> case encodeHeapGroupFrame bindings children of
    (encodedBindings, [bodyIndex]) ->
      tagged 6 [encodedBindings, encodeNodeIndex bodyIndex]
    _ -> error "let frame has no unique body"
  LetJoins bindings _ -> case encodeJoinGroupFrame bindings children of
    (encodedBindings, [bodyIndex]) ->
      tagged 7 [encodedBindings, encodeNodeIndex bodyIndex]
    _ -> error "let-joins frame has no unique body"
  Jump join arguments -> tagged 8 [encodeJoinId join, list encodeAtom arguments]

encodeNodeIndex :: Int -> Encoding
encodeNodeIndex = encodeWord . fromIntegral

encodeAlternativeFrame :: Alternative -> Int -> Encoding
encodeAlternativeFrame (Alternative pattern_ binders _) bodyIndex = array
  [encodePattern pattern_, list encodeValueId binders, encodeNodeIndex bodyIndex]

encodeHeapGroupFrame :: Group HeapBinding -> [Int] -> (Encoding, [Int])
encodeHeapGroupFrame group indices = case group of
  NonRecursive binding ->
    let (encoded, rest) = encodeHeapBindingFrame binding indices
    in (tagged 0 [encoded], rest)
  Recursive bindings ->
    let (rest, encoded) = mapAccumL encodeOne indices bindings
    in (tagged 1 [list id encoded], rest)
 where
  encodeOne remaining binding =
    let (encoded, rest) = encodeHeapBindingFrame binding remaining
    in (rest, encoded)

encodeHeapBindingFrame :: HeapBinding -> [Int] -> (Encoding, [Int])
encodeHeapBindingFrame (HeapBinding value rhs) indices =
  let (encodedRhs, rest) = encodeHeapRhsFrame rhs indices
  in (array [encodeValueId value, encodedRhs], rest)

encodeHeapRhsFrame :: HeapRhs -> [Int] -> (Encoding, [Int])
encodeHeapRhsFrame rhs indices = case rhs of
  Bytes bytes -> (tagged 3 [encodeBytes bytes], indices)
  Function signature parameters captures _ ->
    let (bodyIndex, rest) = takeIndex indices
    in (tagged 0 [encodeSignatureId signature, list encodeValueId parameters,
      list encodeValueRef captures, encodeNodeIndex bodyIndex], rest)
  Thunk signature update captures _ ->
    let (bodyIndex, rest) = takeIndex indices
    in (tagged 1 [encodeSignatureId signature,
      encodeWord (case update of Memoize -> 0; SingleEntry -> 1),
      list encodeValueRef captures, encodeNodeIndex bodyIndex], rest)
  Constructor constructor fields ->
    (tagged 2 [encodeConstructorId constructor, list encodeAtom fields], indices)

encodeJoinGroupFrame :: Group JoinBinding -> [Int] -> (Encoding, [Int])
encodeJoinGroupFrame group indices = case group of
  NonRecursive binding ->
    let (encoded, rest) = encodeJoinBindingFrame binding indices
    in (tagged 0 [encoded], rest)
  Recursive bindings ->
    let (rest, encoded) = mapAccumL encodeOne indices bindings
    in (tagged 1 [list id encoded], rest)
 where
  encodeOne remaining binding =
    let (encoded, rest) = encodeJoinBindingFrame binding remaining
    in (rest, encoded)

encodeJoinBindingFrame :: JoinBinding -> [Int] -> (Encoding, [Int])
encodeJoinBindingFrame (JoinBinding join signature parameters _) indices =
  let (bodyIndex, rest) = takeIndex indices
  in (array [encodeJoinId join, encodeSignatureId signature,
    list encodeValueId parameters, encodeNodeIndex bodyIndex], rest)

takeIndex :: [Int] -> (Int, [Int])
takeIndex (index : rest) = (index, rest)
takeIndex [] = error "prepared frame child index missing"

encodeCaseKind :: CaseKind -> Encoding
encodeCaseKind kind = case kind of
  AlgebraicCase family -> tagged 0 [encodeSymbol family]
  PrimitiveCase rep -> tagged 1 [encodeRep rep]
  MultiValueCase -> tag 2
  PolymorphicCase -> tag 3

tagged :: Word -> [Encoding] -> Encoding
tagged constructor fields = array (encodeWord constructor : fields)

tag :: Word -> Encoding
tag constructor = tagged constructor []

encodeValueId :: ValueId -> Encoding
encodeValueId (ValueId value) = encodeWord32 value

encodeJoinId :: JoinId -> Encoding
encodeJoinId (JoinId value) = encodeWord32 value

encodeGlobalId :: GlobalId -> Encoding
encodeGlobalId (GlobalId value) = encodeWord32 value

encodeConstructorId :: ConstructorId -> Encoding
encodeConstructorId (ConstructorId value) = encodeWord32 value

encodeOperationId :: OperationId -> Encoding
encodeOperationId (OperationId value) = encodeWord32 value

encodeSignatureId :: SignatureId -> Encoding
encodeSignatureId (SignatureId value) = encodeWord32 value

encodeTypeNodeId :: TypeNodeId -> Encoding
encodeTypeNodeId (TypeNodeId value) = encodeWord32 value
