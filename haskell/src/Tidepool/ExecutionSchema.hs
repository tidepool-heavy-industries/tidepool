{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DeriveTraversable #-}

-- | The prepared-STG execution schema. GHC values are projected into these
-- finite semantic types before bytes cross into Rust; this is not an
-- introspection API and contains no rendered GHC syntax.
module Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), TargetDescriptor(..)
  , ProgramEnvelope(..), SymbolIdentity(..), RuntimeRep(..), ResultContract(..), Signature(..)
  , ValueId(..), JoinId(..), GlobalId(..), ConstructorId(..), OperationId(..)
  , SignatureId(..), ValueRef(..), ScalarLiteral(..), Atom(..), Group(..)
  , UpdatePolicy(..), HeapBinding(..), HeapRhs(..), JoinBinding(..)
  , AlternativePattern(..), Alternative(..), CaseKind(..), Expr(..), FieldLayout(..)
  , CheckedLayout(..), ConstructorDecl(..), GlobalDecl(..), OperationDecl(..)
  , OperationIdentity(..), JsonLayout(..), WiredInErrorKind(..), ForeignConvention(..)
  , TopBinding(..), WireProgram(..), schemaVersion, executionAbiVersion
  , TypeNodeId(..), CtorRow(..), TypeNode(..), SiteDelivery(..), SiteRow(..)
  ) where

import Data.ByteString (ByteString)
import Data.Text (Text)
import Data.Word (Word32, Word64, Word8)
import GHC.Generics (Generic)

schemaVersion, executionAbiVersion :: Word64
schemaVersion = 13
executionAbiVersion = 6

newtype ValueId = ValueId Word32 deriving stock (Eq, Ord, Show, Generic)
newtype JoinId = JoinId Word32 deriving stock (Eq, Ord, Show, Generic)
newtype GlobalId = GlobalId Word32 deriving stock (Eq, Ord, Show, Generic)
newtype ConstructorId = ConstructorId Word32 deriving stock (Eq, Ord, Show, Generic)
newtype OperationId = OperationId Word32 deriving stock (Eq, Ord, Show, Generic)
newtype SignatureId = SignatureId Word32 deriving stock (Eq, Ord, Show, Generic)
newtype TypeNodeId = TypeNodeId Word32 deriving stock (Eq, Ord, Show, Generic)

data Architecture = X86_64 | Aarch64 deriving stock (Eq, Ord, Show, Generic)
data Endianness = LittleEndian | BigEndian deriving stock (Eq, Ord, Show, Generic)
data TargetDescriptor = TargetDescriptor
  { targetArchitecture :: Architecture, targetEndianness :: Endianness
  , targetPointerWidth :: Word8, targetWordWidth :: Word8
  , targetAbi :: Text, targetFeatures :: [Text]
  } deriving stock (Eq, Show, Generic)
data ProgramEnvelope = ProgramEnvelope
  { envelopeSchemaVersion :: Word64, envelopeProjectionProfile :: Text
  , envelopeToolchain :: Text, envelopeExecutionAbiVersion :: Word64
  , envelopeTarget :: TargetDescriptor
  } deriving stock (Eq, Show, Generic)
data SymbolIdentity = SymbolIdentity
  { symbolUnit :: Text, symbolModule :: Text, symbolNamespace :: Text
  , symbolOccurrence :: Text
  , symbolRecordParent :: Maybe Text
  } deriving stock (Eq, Ord, Show, Generic)

data RuntimeRep = VoidRep | LiftedRefRep | UnliftedRefRep | AddressRep
  | IntRep Word8 | WordRep Word8 | FloatRep Word8
  deriving stock (Eq, Ord, Show, Generic)

-- | CallerResult is a nonzero-arity callable convention, instantiated by
-- each caller; it is distinct from a body that cannot return successfully.
data ResultContract = Returns [RuntimeRep] | NoSuccess | CallerResult
  deriving stock (Eq, Ord, Show, Generic)

data Signature = Signature
  { signatureArguments :: [RuntimeRep]
  , signatureResults :: ResultContract
  }
  deriving stock (Eq, Show, Generic)
data FieldLayout = FieldLayout { fieldRep :: RuntimeRep, fieldOffset :: Word32 }
  deriving stock (Eq, Show, Generic)
data CheckedLayout = CheckedLayout
  { layoutFields :: [FieldLayout], layoutAlignment :: Word32
  , layoutPayloadSize :: Word32, layoutRootMask :: [Bool]
  } deriving stock (Eq, Show, Generic)
data ConstructorDecl = ConstructorDecl
  { constructorIdentity :: SymbolIdentity, constructorFamily :: SymbolIdentity
  , constructorResultRep :: RuntimeRep
  , constructorFieldReps :: [RuntimeRep], constructorStrictFields :: [Bool]
  , constructorLayout :: CheckedLayout
  , constructorTag :: Word32, constructorFamilySize :: Word32
  , constructorHostId :: Word64
  } deriving stock (Eq, Show, Generic)
data GlobalDecl = GlobalDecl
  { globalIdentity :: SymbolIdentity, globalRep :: RuntimeRep
  , globalEntrySignature :: Maybe SignatureId
  , globalRequiredEvaluated :: Bool, globalRequiredGeneration :: Maybe Word64
  } deriving stock (Eq, Show, Generic)
-- | Operation signatures distinguish instantiated uses of one identity.
-- Catalogued missing capabilities have their own identity; other unresolved
-- foreign calls remain projection errors, never primop names.
data ForeignConvention = CCall deriving stock (Eq, Ord, Show, Generic)
data JsonLayout a = JsonLayout
  { jsonObject :: a, jsonArray :: a, jsonString :: a, jsonNumber :: a
  , jsonBool :: a, jsonNull :: a, jsonMapBin :: a, jsonMapTip :: a
  , jsonTrue :: a, jsonFalse :: a, jsonCons :: a, jsonNil :: a
  , jsonScientific :: a, jsonIntegerSmall :: a, jsonIntegerPositive :: a
  , jsonIntegerNegative :: a, jsonText :: a, jsonInt :: a
  } deriving stock (Eq, Ord, Show, Functor, Foldable, Traversable, Generic)
data OperationIdentity
  = PrimOpIdentity Text
  | IntrinsicIdentity Text ForeignConvention
  | JsonDecodeIdentity (JsonLayout ConstructorId) ConstructorId ConstructorId
  | JsonEncodeIdentity (JsonLayout ConstructorId)
  | CapabilityIdentity Text
  | WiredInErrorIdentity WiredInErrorKind
  deriving stock (Eq, Ord, Show, Generic)
-- | Declaration order is the stable wire-tag order and mirrors GHC's
-- authoritative wired-in error keys.
data WiredInErrorKind
  = WiredPatternMatch
  | WiredNonExhaustiveGuards
  | WiredRecordSelector
  | WiredRecordConstruction
  | WiredNoMethodBinding
  | WiredDeferredType
  | WiredImpossible
  | WiredImpossibleConstraint
  | WiredAbsent
  | WiredAbsentConstraint
  | WiredAbsentSumField
  deriving stock (Eq, Ord, Show, Enum, Bounded, Generic)
data OperationDecl = OperationDecl { operationIdentity :: OperationIdentity, operationSignature :: SignatureId }
  deriving stock (Eq, Show, Generic)

data ValueRef = Local ValueId | Global GlobalId deriving stock (Eq, Show, Generic)
data ScalarLiteral = IntLiteral Word8 ByteString | WordLiteral Word8 ByteString
  | FloatLiteral Word8 ByteString | BytesLiteral ByteString | NullAddressLiteral
  deriving stock (Eq, Show, Generic)
-- | Rubbish retains its physical representation, not a fabricated scalar value.
-- TYPE versus CONSTRAINT is erased after GHC resolves that representation.
data Atom = Ref ValueRef | Scalar ScalarLiteral | Void | Rubbish RuntimeRep
  deriving stock (Eq, Show, Generic)
data Group a = NonRecursive a | Recursive [a] deriving stock (Eq, Show, Generic)
-- | Thunk policies only: ReEntrant is Function, and JumpedTo belongs to joins.
data UpdatePolicy = Memoize | SingleEntry deriving stock (Eq, Show, Generic)
data HeapBinding = HeapBinding { heapBindingId :: ValueId, heapBindingRhs :: HeapRhs }
  deriving stock (Eq, Show, Generic)
data HeapRhs = Bytes ByteString | Function SignatureId [ValueId] [ValueRef] Expr
  | Thunk SignatureId UpdatePolicy [ValueRef] Expr | Constructor ConstructorId [Atom]
  deriving stock (Eq, Show, Generic)
data JoinBinding = JoinBinding JoinId SignatureId [ValueId] Expr deriving stock (Eq, Show, Generic)
data AlternativePattern = DefaultPattern | ConstructorPattern ConstructorId
  | LiteralPattern ScalarLiteral deriving stock (Eq, Show, Generic)
data Alternative = Alternative AlternativePattern [ValueId] Expr deriving stock (Eq, Show, Generic)
-- | Authoritative post-unarisation case classification. Algebraic identity
-- establishes family agreement, not a proof that every constructor is listed.
data CaseKind = AlgebraicCase SymbolIdentity | PrimitiveCase RuntimeRep
  | MultiValueCase | PolymorphicCase deriving stock (Eq, Show, Generic)
data Expr = Return [Atom] | Enter Atom SignatureId | Call Atom SignatureId [Atom] | Operation OperationId [Atom]
  | Construct ConstructorId [Atom]
  | Case Expr ValueId ResultContract CaseKind [Alternative]
  | Let (Group HeapBinding) Expr | LetJoins (Group JoinBinding) Expr
  | Jump JoinId [Atom]
  deriving stock (Eq, Show, Generic)
data TopBinding = TopBinding SymbolIdentity HeapBinding deriving stock (Eq, Show, Generic)
data CtorRow = CtorRow
  { rowConstructor :: ConstructorId
  , rowFields :: [TypeNodeId]
  } deriving stock (Eq, Show, Generic)
data TypeNode
  = TypeData SymbolIdentity [TypeNodeId] [CtorRow]
  | TypeText
  | TypeInteger
  | TypeNatural
  | TypeScalar RuntimeRep
  | TypeUnconstructible Text Text
  deriving stock (Eq, Show, Generic)
data SiteDelivery = HostAnswer | LiveReentry | ExitCellFill | TerminalCapture
  deriving stock (Eq, Ord, Show, Generic)
data SiteRow = SiteRow
  { siteId :: Word64
  , siteOrigin :: Text
  , siteOrdinal :: Word64
  , siteDelivery :: SiteDelivery
  , siteWire :: TypeNodeId
  , siteInputs :: [TypeNodeId]
  } deriving stock (Eq, Show, Generic)
data WireProgram = WireProgram
  { programEnvelope :: ProgramEnvelope, programSignatures :: [Signature]
  , programGlobals :: [GlobalDecl], programConstructors :: [ConstructorDecl]
  , programOperations :: [OperationDecl], programBindings :: [Group TopBinding]
  , programEntry :: ValueId, programTypes :: [TypeNode], programSites :: [SiteRow]
  -- | Request constructors whose reply is answered by a synthetic row in
  -- 'programSites': an ordinary effect request carries no dynamic site, so
  -- the host classifies it by its outer constructor.
  , programVerbSites :: [(ConstructorId, Word64)]
  -- | Compiler-authenticated JSON constructor roles. Kept independently of
  -- intrinsic operations because typed host mounts and answers also need it.
  , programJsonLayout :: Maybe (JsonLayout ConstructorId)
  } deriving stock (Eq, Show, Generic)
