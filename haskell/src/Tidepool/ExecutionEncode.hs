-- | Deterministic CBOR encoding for the internal prepared-execution schema.
-- The reader owns validation; this module preserves the already-normalized
-- table order and uses only definite-length arrays and primitive leaves.
module Tidepool.ExecutionEncode (encodeWireProgram) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Data.ByteString (ByteString)
import Data.Foldable (fold)
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
  , list (encodeGroup encodeTopBinding) (programBindings program)
  , encodeValueId (programEntry program)
  ]
 where
  envelope = programEnvelope program

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
  , list encodeRep (signatureResults signature)
  ]

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
  [ encodeString (operationIdentity operation)
  , encodeSignatureId (operationSignature operation)
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
  CharLiteral codepoint -> tagged 3 [encodeWord32 codepoint]
  BytesLiteral bytes -> tagged 4 [encodeBytes bytes]
  NullAddressLiteral -> tag 5

encodeAtom :: Atom -> Encoding
encodeAtom atom = case atom of
  Ref ref -> tagged 0 [encodeValueRef ref]
  Scalar scalar -> tagged 1 [encodeScalar scalar]
  Void -> tag 2
  Rubbish rep -> tagged 3 [encodeRep rep]

encodeGroup :: (a -> Encoding) -> Group a -> Encoding
encodeGroup encode group = case group of
  NonRecursive value -> tagged 0 [encode value]
  Recursive values -> tagged 1 [list encode values]

encodeHeapBinding :: HeapBinding -> Encoding
encodeHeapBinding binding = array
  [encodeValueId (heapBindingId binding), encodeHeapRhs (heapBindingRhs binding)]

encodeHeapRhs :: HeapRhs -> Encoding
encodeHeapRhs rhs = case rhs of
  Bytes bytes -> tagged 3 [encodeBytes bytes]
  Function signature parameters captures body -> tagged 0
    [ encodeSignatureId signature
    , list encodeValueId parameters
    , list encodeValueRef captures
    , encodeExpr body
    ]
  Thunk signature update captures body -> tagged 1
    [ encodeSignatureId signature
    , encodeWord (case update of Memoize -> 0; SingleEntry -> 1)
    , list encodeValueRef captures
    , encodeExpr body
    ]
  Constructor constructor fields -> tagged 2
    [encodeConstructorId constructor, list encodeAtom fields]

encodeJoinBinding :: JoinBinding -> Encoding
encodeJoinBinding (JoinBinding join signature parameters body) = array
  [ encodeJoinId join
  , encodeSignatureId signature
  , list encodeValueId parameters
  , encodeExpr body
  ]

encodePattern :: AlternativePattern -> Encoding
encodePattern pattern_ = case pattern_ of
  DefaultPattern -> tag 0
  ConstructorPattern constructor -> tagged 1 [encodeConstructorId constructor]
  LiteralPattern literal -> tagged 2 [encodeScalar literal]

encodeAlternative :: Alternative -> Encoding
encodeAlternative (Alternative pattern_ binders body) = array
  [encodePattern pattern_, list encodeValueId binders, encodeExpr body]

encodeExpr :: Expr -> Encoding
encodeExpr expr = case expr of
  Return atoms -> tagged 0 [list encodeAtom atoms]
  Enter atom signature -> tagged 1 [encodeAtom atom, encodeSignatureId signature]
  Call callee signature arguments -> tagged 2
    [encodeAtom callee, encodeSignatureId signature, list encodeAtom arguments]
  Operation operation arguments -> tagged 3
    [encodeOperationId operation, list encodeAtom arguments]
  Construct constructor fields -> tagged 4
    [encodeConstructorId constructor, list encodeAtom fields]
  Case scrutinee binder results kind alternatives -> tagged 5
    [ encodeExpr scrutinee
    , encodeValueId binder
    , list encodeRep results
    , encodeCaseKind kind
    , list encodeAlternative alternatives
    ]
  Let bindings body -> tagged 6 [encodeGroup encodeHeapBinding bindings, encodeExpr body]
  LetJoins bindings body -> tagged 7 [encodeGroup encodeJoinBinding bindings, encodeExpr body]
  Jump join arguments -> tagged 8 [encodeJoinId join, list encodeAtom arguments]

encodeCaseKind :: CaseKind -> Encoding
encodeCaseKind kind = case kind of
  AlgebraicCase family -> tagged 0 [encodeSymbol family]
  PrimitiveCase rep -> tagged 1 [encodeRep rep]
  MultiValueCase -> tag 2
  PolymorphicCase -> tag 3

encodeTopBinding :: TopBinding -> Encoding
encodeTopBinding (TopBinding identity binding) = array
  [encodeSymbol identity, encodeHeapBinding binding]

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
