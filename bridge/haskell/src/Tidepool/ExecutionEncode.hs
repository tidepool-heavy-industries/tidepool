{-# LANGUAGE BangPatterns #-}

-- | Deterministic CBOR encoding for the internal prepared-execution schema.
-- The reader owns validation; this module preserves the already-normalized
-- table order and uses only definite-length arrays and primitive leaves.
module Tidepool.ExecutionEncode
  ( encodeSymbol, encodeWireProgram, encodeProjectedGroup, encodeModuleProducts
  , ModuleProductEncoding, prepareModuleProductEncoding
  , moduleProductInput, moduleProductBytes, encodeModuleProductInventory
  , ProjectedGroupEncoding, prepareProjectedGroupEncoding, prepareModuleProductEncodingFromGroups
  , projectedGroupEncodingBytes
  ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Data.ByteString (ByteString)
import Data.Foldable (fold)
import Data.List (mapAccumL)
import Data.IntMap.Strict qualified as IntMap
import Data.Sequence (Seq, (|>))
import Data.Sequence qualified as Seq
import Data.Text (Text)
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
  , encodeTypeGraph (programTypes program)
  , list encodeSiteRow (programSites program)
  , list encodeConstructorReply (programConstructorReplies program)
  , case programJsonLayout program of
      Nothing -> tag 0
      Just layout -> tagged 1 [encodeJsonLayout layout]
  ]
 where
  envelope = programEnvelope program
  ((_, frames), bindings) = mapAccumL encodeTopGroup (0, Seq.empty) (programBindings program)

-- | Entry-free, independently decodable definition arena for one original
-- STG group. The body uses the same table grammar as a prepared program, but
-- no target entry is selected or serialized. The owning module/version and
-- exact import owners are attached by the product inventory before caching.
encodeProjectedGroup :: ProjectedGroup -> ByteString
encodeProjectedGroup group = toStrictByteString $ array
  [ encodeString "TPGRP"
  , encodeWord64 1
  , encodeWord32 (projectedOriginalOrdinal group)
  , list encodeSymbol (projectedBinders group)
  , encodeWord64 (envelopeSchemaVersion envelope)
  , encodeString (envelopeProjectionProfile envelope)
  , encodeString (envelopeToolchain envelope)
  , encodeWord64 (envelopeExecutionAbiVersion envelope)
  , encodeTarget (envelopeTarget envelope)
  , list encodeSignature (projectedSignatures body)
  , list encodeGlobal (projectedGlobals body)
  , list encodeConstructor (projectedConstructors body)
  , list encodeOperation (projectedOperations body)
  , encodeListLen (fromIntegral (Seq.length frames)) <> fold frames
  , list id bindings
  , encodeTypeGraph (projectedTypes body)
  , list encodeSiteRow (projectedSites body)
  , list encodeConstructorReply (projectedConstructorReplies body)
  , case projectedJsonLayout body of
      Nothing -> tag 0
      Just layout -> tagged 1 [encodeJsonLayout layout]
  ]
 where
  body = projectedBody group
  envelope = projectedEnvelope body
  ((_, frames), bindings) = mapAccumL encodeTopGroup (0, Seq.empty)
    (projectedBindings body)

-- | The worker's neutral product sidecar. Interface bytes and every group
-- inhabit the same bounded document, so a cache owner cannot pair a group's
-- definitions with a different GHC interface by listing separate files.
encodeModuleProducts :: [(Text, Text, ByteString, [ProjectedGroup])] -> ByteString
encodeModuleProducts = encodeModuleProductInventory . map prepareModuleProductEncoding

-- | Retain canonical group bytes and the singleton document with the original
-- product owner. Fields stay lazy: inspecting owner/interface/group evidence
-- must not demand serialization, and an aggregate inventory need not demand
-- singleton documents. This value has the same lifetime as its product.
data ModuleProductEncoding = ModuleProductEncoding
  { moduleProductInput :: (Text, Text, ByteString, [ProjectedGroup])
  , moduleProductGroupBytes :: [ByteString]
  , moduleProductBytes :: ByteString
  }

-- Encoded payload and semantic group are issued together by this encoder.
-- Retained raw projection can share these leaves without callers pairing an
-- unrelated byte string with a group's certification evidence.
data ProjectedGroupEncoding = ProjectedGroupEncoding ProjectedGroup ByteString

prepareProjectedGroupEncoding :: ProjectedGroup -> ProjectedGroupEncoding
prepareProjectedGroupEncoding group = ProjectedGroupEncoding group (encodeProjectedGroup group)

projectedGroupEncodingBytes :: ProjectedGroupEncoding -> ByteString
projectedGroupEncodingBytes (ProjectedGroupEncoding _ bytes) = bytes

prepareModuleProductEncodingFromGroups
  :: Text -> Text -> ByteString -> [ProjectedGroupEncoding] -> ModuleProductEncoding
prepareModuleProductEncodingFromGroups unit owner interface groups = encoded
  where
    encoded = ModuleProductEncoding
      (unit,owner,interface,[group | ProjectedGroupEncoding group _ <- groups])
      [bytes | ProjectedGroupEncoding _ bytes <- groups]
      (encodeModuleProductInventory [encoded])

prepareModuleProductEncoding :: (Text, Text, ByteString, [ProjectedGroup]) -> ModuleProductEncoding
prepareModuleProductEncoding (unit,owner,interface,groups) =
  prepareModuleProductEncodingFromGroups unit owner interface (map prepareProjectedGroupEncoding groups)

-- | Aggregate and singleton TPMOD documents share group payloads, while each
-- preserves its own module-list framing and exact interface bytes.
encodeModuleProductInventory :: [ModuleProductEncoding] -> ByteString
encodeModuleProductInventory modules = toStrictByteString $ array
  [ encodeString "TPMOD"
  , encodeWord64 1
  , list encodeModule modules
  ]
 where
  encodeModule encodedProduct =
    let (unit, name, interface, _) = moduleProductInput encodedProduct
    in array
      [ encodeString unit
      , encodeString name
      , encodeBytes interface
      , list encodeBytes (moduleProductGroupBytes encodedProduct)
      ]

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
      JsonDecodeIdentity left right -> tagged 4
        [encodeConstructorId left, encodeConstructorId right]
      JsonEncodeIdentity -> tag 5
  , encodeSignatureId (operationSignature operation)
  ]

encodeJsonLayout :: JsonLayout ConstructorId -> Encoding
encodeJsonLayout layout = array (map encodeConstructorId
  [ jsonObject layout, jsonArray layout, jsonString layout, jsonNumber layout
  , jsonBool layout, jsonNull layout, jsonMapBin layout, jsonMapTip layout
  , jsonTrue layout, jsonFalse layout, jsonCons layout, jsonNil layout
  , jsonScientific layout, jsonIntegerSmall layout, jsonIntegerPositive layout
  , jsonIntegerNegative layout, jsonText layout, jsonInt layout ])

encodeTypeGraph :: TypeGraph -> Encoding
encodeTypeGraph graph = array
  [ encodeWord 1
  , list encodeTypeNode (IntMap.elems (typeGraphNodes graph))
  , list encodeTypeEdge
      [ TypeEdge (TypeNodeId (fromIntegral source)) target role
      | (source, outgoing) <- IntMap.toAscList (typeGraphEdges graph)
      , (role, target) <- outgoing ]
  ]

encodeTypeNode :: TypeNode -> Encoding
encodeTypeNode node = case node of
  TypeRoot domain binders rendered -> tagged 0
    [ encodeWord (case domain of ClosedRoot -> 0; ConstructorSchemeRoot -> 1)
    , list (encodeWord . sourceFlag) binders, encodeString rendered ]
  TypeDeclaration identity parameters form restriction -> tagged 1
    [ encodeSymbol identity, list (encodeWord . parameterFlag) parameters
    , encodeDeclarationForm form
    , encodeWord (case restriction of UnrestrictedSyntax -> 0; EffectHead -> 1) ]
  TypeConstructorTemplate constructor -> tagged 2 [encodeConstructorId constructor]
  TypeBound index -> tagged 3 [encodeWord32 index]
  TypeNominalApplication -> tag 4
  TypeApplication -> tag 5
  TypeFunction flag -> tagged 6 [encodeWord (functionFlag flag)]
  TypeForAll flag -> tagged 7 [encodeWord (forallFlag flag)]
  TypeLiteral literal -> tagged 8 $ case literal of
    NaturalTypeLiteral value -> [encodeWord 0, encodeString value]
    SymbolTypeLiteral value -> [encodeWord 1, encodeString value]
    CharacterTypeLiteral value -> [encodeWord 2, encodeWord (fromIntegral (fromEnum value))]
 where
  sourceFlag SourceSpecified = 1
  sourceFlag SourceInferred = 2
  parameterFlag NamedRequired = 0
  parameterFlag NamedSpecified = 1
  parameterFlag NamedInferred = 2
  parameterFlag AnonymousVisible = 3
  functionFlag TypeToType = 0
  functionFlag TypeToConstraint = 1
  functionFlag ConstraintToType = 2
  functionFlag ConstraintToConstraint = 3
  forallFlag ForAllRequired = 0
  forallFlag ForAllSpecified = 1
  forallFlag ForAllInferred = 2

encodeDeclarationForm :: DeclarationFormF RuntimeRep -> Encoding
encodeDeclarationForm form = case form of
  DataDeclaration -> tag 0
  NewtypeDeclaration arity -> tagged 1 [encodeWord32 arity]
  TextDeclaration -> tag 2
  IntegerDeclaration -> tag 3
  NaturalDeclaration -> tag 4
  ScalarDeclaration rep -> tagged 5 [encodeRep rep]
  OpaqueDeclaration headKind reason -> tagged 6
    [ encodeWord (case headKind of NominalConstructor -> 0; NominalFamily -> 1)
    , encodeString reason ]

encodeTypeEdge :: TypeEdgeF RuntimeRep -> Encoding
encodeTypeEdge edge = array
  [ encodeTypeNodeId (typeEdgeSource edge), encodeTypeNodeId (typeEdgeTarget edge)
  , case typeEdgeRole edge of
      TypeBinderKind index -> tagged 0 [encodeWord32 index]
      TypeBody -> tag 1
      TypeHead -> tag 2
      TypeArgument index -> tagged 3 [encodeWord32 index]
      TypeFunctionEdge -> tag 4
      TypeApplyArgument -> tag 5
      TypeMultiplicity -> tag 6
      TypeDomain -> tag 7
      TypeCodomain -> tag 8
      TypeKind -> tag 9
      TypeConstructor index -> tagged 10 [encodeWord32 index]
      TypeField index rep -> tagged 11 [encodeWord32 index, encodeRep rep]
      TypeAliasRhs -> tag 12
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

encodeConstructorReply :: (ConstructorId, ConstructorReply) -> Encoding
encodeConstructorReply (constructor, reply) = array
  [ encodeConstructorId constructor
  , case reply of
      StaticReply node -> tagged 0 [encodeTypeNodeId node]
      ReplyAtSite -> tag 1
  ]

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
