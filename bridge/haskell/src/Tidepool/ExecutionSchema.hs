{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DeriveTraversable #-}

-- | The prepared-STG execution schema. GHC values are projected into these
-- finite semantic types before bytes cross into Rust; this is not an
-- introspection API. Diagnostic rendering is separate from graph semantics.
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
  , ProjectedGroup(..), ProjectedGroupBody(..)
  , TypeNodeId(..), TypeGraph, TypeNode, TypeGraphF(..), TypeNodeF(..), TypeEdgeF(..)
  , RootDomain(..), SourceBinderFlag(..), ParameterFlag(..), ForAllFlag(..)
  , FunctionFlag(..), NominalHeadKind(..), DeclarationFormF(..), TypeLiteral(..), SyntaxRestriction(..)
  , TypeEdgeRoleF(..), SiteDelivery(..), SiteRow(..), ConstructorReply(..)
  ) where

import Data.ByteString (ByteString)
import Data.IntMap.Strict (IntMap)
import Data.Text (Text)
import Data.Word (Word32, Word64, Word8)
import GHC.Generics (Generic)

schemaVersion, executionAbiVersion :: Word64
schemaVersion = 17
executionAbiVersion = 9

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
  deriving stock (Eq, Ord, Show, Generic)
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
  | JsonDecodeIdentity ConstructorId ConstructorId
  | JsonEncodeIdentity
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
-- Preparation retains original compiler objects in these payloads; projection
-- assigns physical identities once. Child references live only in edges.
data TypeGraphF constructor identity rep = TypeGraph
  { typeGraphNodes :: IntMap (TypeNodeF constructor identity rep)
  , typeGraphEdges :: IntMap [(TypeEdgeRoleF rep, TypeNodeId)]
  } deriving stock (Eq, Ord, Show, Generic)
type TypeGraph = TypeGraphF ConstructorId SymbolIdentity RuntimeRep
type TypeNode = TypeNodeF ConstructorId SymbolIdentity RuntimeRep

data RootDomain = ClosedRoot | ConstructorSchemeRoot
  deriving stock (Eq, Ord, Show, Generic)
data SourceBinderFlag = SourceSpecified | SourceInferred
  deriving stock (Eq, Ord, Show, Generic)
data ParameterFlag = NamedRequired | NamedSpecified | NamedInferred
  | AnonymousVisible
  deriving stock (Eq, Ord, Show, Generic)
data ForAllFlag = ForAllRequired | ForAllSpecified | ForAllInferred
  deriving stock (Eq, Ord, Show, Generic)
data FunctionFlag = TypeToType | TypeToConstraint | ConstraintToType | ConstraintToConstraint
  deriving stock (Eq, Ord, Show, Generic)
data NominalHeadKind = NominalConstructor | NominalFamily
  deriving stock (Eq, Ord, Show, Generic)
data DeclarationFormF rep = DataDeclaration | NewtypeDeclaration Word32
  | TextDeclaration | IntegerDeclaration | NaturalDeclaration | ScalarDeclaration rep
  | OpaqueDeclaration NominalHeadKind Text
  deriving stock (Eq, Ord, Show, Generic)
data TypeLiteral = NaturalTypeLiteral Text | SymbolTypeLiteral Text | CharacterTypeLiteral Char
  deriving stock (Eq, Ord, Show, Generic)
data SyntaxRestriction = UnrestrictedSyntax | EffectHead
  deriving stock (Eq, Ord, Show, Generic)
data TypeNodeF constructor identity rep
  = TypeRoot RootDomain [SourceBinderFlag] Text
  | TypeDeclaration identity [ParameterFlag] (DeclarationFormF rep) SyntaxRestriction
  | TypeConstructorTemplate constructor
  | TypeBound Word32
  | TypeNominalApplication
  | TypeApplication
  | TypeFunction FunctionFlag
  | TypeForAll ForAllFlag
  | TypeLiteral TypeLiteral
  deriving stock (Eq, Ord, Show, Generic)
data TypeEdgeRoleF rep
  = TypeBinderKind Word32 | TypeBody | TypeHead | TypeArgument Word32
  | TypeFunctionEdge | TypeApplyArgument | TypeMultiplicity | TypeDomain | TypeCodomain
  | TypeKind | TypeConstructor Word32 | TypeField Word32 rep | TypeAliasRhs
  deriving stock (Eq, Ord, Show, Generic)
data TypeEdgeF rep = TypeEdge
  { typeEdgeSource :: TypeNodeId, typeEdgeTarget :: TypeNodeId
  , typeEdgeRole :: TypeEdgeRoleF rep
  } deriving stock (Eq, Show, Generic)
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
  , programEntry :: ValueId, programTypes :: TypeGraph, programSites :: [SiteRow]
  -- | An unsited request retains its original constructor's scoped reply;
  -- only the exact compiler carrier selects a dynamic site.
  , programConstructorReplies :: [(ConstructorId, ConstructorReply)]
  -- | Compiler-authenticated JSON constructor roles. Kept independently of
  -- intrinsic operations because typed host mounts and answers also need it.
  , programJsonLayout :: Maybe (JsonLayout ConstructorId)
  } deriving stock (Eq, Show, Generic)

-- One original STG group projected with its own complete expression and
-- declaration arena. Product identity and versioned imports are attached by
-- the module-product owner before this becomes a durable artifact.
data ProjectedGroupBody = ProjectedGroupBody
  { projectedEnvelope :: ProgramEnvelope
  , projectedSignatures :: [Signature]
  , projectedGlobals :: [GlobalDecl]
  , projectedConstructors :: [ConstructorDecl]
  , projectedOperations :: [OperationDecl]
  , projectedBindings :: [Group TopBinding]
  , projectedTypes :: TypeGraph
  , projectedSites :: [SiteRow]
  , projectedConstructorReplies :: [(ConstructorId, ConstructorReply)]
  , projectedJsonLayout :: Maybe (JsonLayout ConstructorId)
  } deriving stock (Eq, Show, Generic)

data ProjectedGroup = ProjectedGroup
  { projectedOriginalOrdinal :: Word32
  , projectedBinders :: [SymbolIdentity]
  , projectedBody :: ProjectedGroupBody
  } deriving stock (Eq, Show, Generic)

-- | Reply evidence owned by the exact request constructor. AtSite reads only
-- its authenticated first runtime field; StaticReply never inspects payloads.
data ConstructorReply
  = StaticReply TypeNodeId
  | StaticReplyWithSite TypeNodeId Word32 Word32 (Maybe Word32)
  | ReplyAtSite
  deriving stock (Eq, Ord, Show, Generic)
