-- | Versioned request protocol between the Rust launcher and the Haskell
-- compiler worker. The worker receives domain fields, not command-line
-- options; CLI compatibility is owned by the launcher layer.
module Tidepool.ExtractRequest
  ( CompileWorkload(..)
  , ExactInputsRequest(..), ExactInputsAcquisition(..), validExactInputsRequest
  , WorldQueryRequest(..), WorldQueryQuestion(..), WorldPublicQuestion(..), WorldObservationQuery(..), publicQuestionInspection, publicQuestionIsType
  , WorldCellRecipe(..), WorldCellOperandRequest(..), WorldCellPlanRequest(..), WorldCellPrepareRequest(..), PlannedCellSlot(..)
  , EnsureExactInputsOutcome(..), ExactInputsRejection(..), WorldResidency(..), WorldNonRetentionReason(..), WorldAcquisitionMiss(..), encodeEnsureExactInputsOutcomeWithin
  , RequestField(..)
  , InspectionRequest(..)
  , StructuredInspection(..)
  , ExactNameInspection(..)
  , StructuredNameScope(..)
  , StructuredNameNamespace(..)
  , InspectionProvenance(..)
  , WorkerRequest(..)
  , RequestOperation(..)
  , RequestShapeError(..)
  , admitRequestOperation
  , validateRequestShape
  , workerRequestFromArgv
  , workerArgv
  , workerRequestFlag
  ) where

import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as LBS
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import Data.Map.Strict (Map)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.Text.Lazy as LT
import qualified Data.Text.Lazy.Encoding as LTE
import Data.Bits ((.|.), shiftL, shiftR)
import Data.Char (digitToInt, isHexDigit)
import Data.Word (Word32, Word64, Word8)
import Tidepool.ExecutionSchema (SymbolIdentity(..))

data CompileWorkload = Foreground | Preparation deriving (Eq, Show)

-- References remain transport data until their issuing owners admit them.
data ExactInputsRequest = ExactInputsRequest String ExactInputsAcquisition
  deriving (Eq, Show)

data ExactInputsAcquisition
  = ResidentExactInputs String
  | CompleteExactInputs FilePath
  | SuccessorExactInputs String String FilePath
  deriving (Eq, Show)

validExactInputsRequest :: ExactInputsRequest -> Bool
validExactInputsRequest (ExactInputsRequest root acquisition) = valid root && case acquisition of
  ResidentExactInputs reference -> validWorldRealizationReference reference
  CompleteExactInputs manifest -> validReference manifest
  SuccessorExactInputs predecessor reference manifest ->
    valid predecessor && validWorldRealizationReference reference && validReference manifest
  where valid = validWorldIdentity

-- Inspect only the fixed nonce plus at most twenty decimal digits. This
-- grammar check stays bounded even for an unfinished transport reference.
validWorldRealizationReference :: String -> Bool
validWorldRealizationReference value = case splitAt 64 value of
  (nonce,'.':suffix) | validWorldIdentity nonce -> case splitAt 21 suffix of
    (digits,_) | length digits > 20 -> False
    (digits,[]) -> case reads digits :: [(Word64,String)] of
      [(unique,"")] -> show unique == digits
      _ -> False
    _ -> False
  _ -> False

validWorldIdentity :: String -> Bool
validWorldIdentity value = length value == 64 && all (\c -> c >= '0' && c <= '9' || c >= 'a' && c <= 'f') value

validReference :: String -> Bool
validReference value = not (null value) && '\0' `notElem` value

data WorldPublicQuestion
  = WorldPublicInfo StructuredInspection
  | WorldPublicType StructuredInspection
  deriving (Eq, Show)

data WorldQueryQuestion
  = WorldPublicModuleQuery WorldPublicQuestion
  | WorldScopedQuery WorldObservationQuery
  deriving (Eq, Show)

data WorldQueryRequest = WorldQueryRequest
  { worldQueryIdentity :: String
  , worldQueryRealization :: String
  , worldQueryQuestion :: WorldQueryQuestion
  , worldQueryOutput :: FilePath
  } deriving (Eq, Show)

publicQuestionInspection :: WorldPublicQuestion -> StructuredInspection
publicQuestionInspection (WorldPublicInfo query) = query
publicQuestionInspection (WorldPublicType query) = query

publicQuestionIsType :: WorldPublicQuestion -> Bool
publicQuestionIsType WorldPublicInfo{} = False
publicQuestionIsType WorldPublicType{} = True

-- Operands only: source, imports, effect row and native type witnesses belong
-- to the held World's protected observation scaffold.
data WorldObservationQuery
  = WorldObservationInfo StructuredInspection
  | WorldObservationType StructuredInspection
  | WorldObservationRowFit StructuredInspection String
  | WorldObservationExpression InspectionProvenance String
  | WorldObservationTypeSearch InspectionProvenance String Int
  deriving (Eq, Show)

-- Recipe bytes and reservations are request data. Runtime admission and the
-- held-world owner, rather than a digest or a slot constructor, grant use.
data WorldCellRecipe = WorldCellRecipe
  { worldCellSource :: String
  , worldCellTemplate :: String
  , worldCellTurnTemplates :: [(String,String)]
  } deriving (Eq, Show)

-- Correlated input data; only the owning row receiver can admit its contents.
data WorldCellOperandRequest = WorldCellOperandRequest FilePath BS.ByteString
  deriving (Eq, Show)

data WorldCellPlanRequest = WorldCellPlanRequest
  { worldCellPlanIdentity :: String
  , worldCellPlanRealization :: String
  , worldCellPlanRecipe :: WorldCellRecipe
  , worldCellPlanOperand :: Maybe WorldCellOperandRequest
  , worldCellPlanOutput :: FilePath
  } deriving (Eq, Show)

data PlannedCellSlot = PlannedPrologue Word64 | PlannedDeclaration Word64
  | PlannedBind Word64 | PlannedExpression Word64 String
  deriving (Eq, Show)

data WorldCellPrepareRequest = WorldCellPrepareRequest
  { worldCellPrepareIdentity :: String
  , worldCellPrepareRealization :: String
  , worldCellPrepareRecipe :: WorldCellRecipe
  , worldCellPrepareOperand :: Maybe WorldCellOperandRequest
  , worldCellPreparePlan :: BS.ByteString
  , worldCellPrepareAdmission :: BS.ByteString
  , worldCellPrepareDefiningScope :: Word64
  , worldCellPrepareSlots :: [(Word64,PlannedCellSlot)]
  , worldCellPrepareDirectory :: FilePath
  , worldCellPrepareOutput :: FilePath
  } deriving (Eq, Show)

validWorldCellRecipe :: WorldCellRecipe -> Bool
validWorldCellRecipe recipe = all (utf8Within (2 * 1024 * 1024))
    (worldCellSource recipe : worldCellTemplate recipe : map snd templates)
  && length templates <= 256
  && all (\(kind,_) -> validReference kind && utf8Within 65536 kind) templates
  where templates = worldCellTurnTemplates recipe

validWorldCellPath :: FilePath -> Bool
validWorldCellPath path = validReference path && utf8Within 65536 path

utf8Within :: Integer -> String -> Bool
utf8Within limit = (<= limit) . toInteger . LBS.length
  . LBS.take (fromInteger (limit + 1)) . LTE.encodeUtf8 . LT.pack

validWorldCellPlanRequest :: WorldCellPlanRequest -> Bool
validWorldCellPlanRequest request = validWorldIdentity (worldCellPlanIdentity request)
  && validWorldRealizationReference (worldCellPlanRealization request)
  && validWorldCellRecipe (worldCellPlanRecipe request)
  && maybe True validWorldCellOperand (worldCellPlanOperand request)
  && validWorldCellPath (worldCellPlanOutput request)

validWorldCellOperand :: WorldCellOperandRequest -> Bool
validWorldCellOperand (WorldCellOperandRequest path sha) = validWorldCellPath path && BS.length sha == 32

data PlannedCellModuleKind = PlannedLibrary | PlannedValue deriving (Eq,Ord)

validWorldCellPrepareRequest :: WorldCellPrepareRequest -> Bool
validWorldCellPrepareRequest request = validWorldIdentity (worldCellPrepareIdentity request)
  && validWorldRealizationReference (worldCellPrepareRealization request)
  && validWorldCellRecipe (worldCellPrepareRecipe request)
  && not (BS.null (worldCellPreparePlan request)) && BS.length (worldCellPreparePlan request) <= 8 * 1024 * 1024
  && BS.length (worldCellPrepareAdmission request) == 32
  && maybe True validWorldCellOperand (worldCellPrepareOperand request)
  && length slots <= 10000
  && map fst slots == take (length slots) [0..]
  && all validSlot (map snd slots)
  && Set.size (Set.fromList (map (owner . snd) slots)) == length slots
  && all validWorldCellPath [worldCellPrepareDirectory request,worldCellPrepareOutput request]
  where
    slots = worldCellPrepareSlots request
    generation slot = case slot of
      PlannedPrologue value -> value; PlannedDeclaration value -> value
      PlannedBind value -> value; PlannedExpression value _ -> value
    owner slot = (case slot of
      PlannedPrologue _ -> PlannedLibrary
      PlannedDeclaration _ -> PlannedLibrary
      PlannedBind _ -> PlannedValue
      PlannedExpression _ _ -> PlannedValue, generation slot)
    validSlot slot = generation slot > 0 && case slot of
      PlannedExpression _ observation -> validReference observation && utf8Within 65536 observation
      _ -> True

validWorldQueryRequest :: WorldQueryRequest -> Bool
validWorldQueryRequest request = validWorldIdentity (worldQueryIdentity request)
  && validWorldRealizationReference (worldQueryRealization request) && validReference (worldQueryOutput request)
  && case worldQueryQuestion request of
    WorldPublicModuleQuery question -> case structuredScope (publicQuestionInspection question) of
      StructuredPublicModule name -> validReference name
      StructuredCurrentScope -> False
    WorldScopedQuery query -> case query of
      WorldObservationInfo question -> current question
      WorldObservationType question -> current question
      WorldObservationRowFit question owner -> current question && structuredNamespace question /= StructuredAnyName && validReference owner
      WorldObservationExpression _ expression -> validReference expression
      WorldObservationTypeSearch _ expression limit -> validReference expression && limit > 0
  where current question = structuredScope question == StructuredCurrentScope && validReference (structuredName question)

data EnsureExactInputsOutcome
  = ExactInputsReady String String WorldResidency
  | ExactInputsNeedsAcquisition String WorldAcquisitionMiss
  | ExactInputsRejected ExactInputsRejection
  deriving (Eq, Show)

-- Known lookup facts about the requested issued realization, not eviction events.
data WorldAcquisitionMiss = WorldNotResidentOnThisWorker | WorldDifferentWorker
  deriving (Eq,Show)

-- Storage observations from the actual admission; neither constructor pins a worker.
data WorldResidency = WorldRetainedAtAdmission | WorldActiveOnly WorldNonRetentionReason
  deriving (Eq, Show)
data WorldNonRetentionReason = WorldStorageAllowanceExceeded Integer Integer
  deriving (Eq, Show)

data ExactInputsRejection
  = UnsupportedOperation | InvalidRealization | InvalidRoot | AcquisitionRejected
  deriving (Eq, Show)

-- Bound UTF-8 expansion before materializing the reference. The envelope and
-- frame length are charged to the existing aggregate response payload budget.
encodeEnsureExactInputsOutcomeWithin :: Integer -> EnsureExactInputsOutcome -> Maybe BS.ByteString
encodeEnsureExactInputsOutcomeWithin budget outcome = case outcome of
  ExactInputsReady root reference residency | validWorldIdentity root && validWorldRealizationReference reference -> do
    storage <- residencyFrame residency
    encodedRoot <- referenceFrame (budget - 13) root
    encodedReference <- referenceFrame (budget - 9 - toInteger (BS.length encodedRoot) - toInteger (BS.length storage)) reference
    pure ("TPEXO004" <> BS.singleton 0 <> encodedRoot <> encodedReference <> storage)
  ExactInputsReady _ _ _ -> Nothing
  ExactInputsNeedsAcquisition root reason | validWorldIdentity root -> do
    encoded <- referenceFrame (budget - 10) root
    pure ("TPEXO004" <> BS.singleton 1 <> encoded <> BS.singleton (case reason of
      WorldNotResidentOnThisWorker -> 0
      WorldDifferentWorker -> 1))
  ExactInputsNeedsAcquisition _ _ -> Nothing
  ExactInputsRejected cause
    | budget < 10 -> Nothing
    | otherwise -> Just ("TPEXO004" <> BS.pack [2, rejectionTag cause])
  where
    residencyFrame WorldRetainedAtAdmission = Just (BS.singleton 0)
    residencyFrame (WorldActiveOnly (WorldStorageAllowanceExceeded required limit))
      | required > limit && all (\n -> n >= 0 && n <= toInteger (maxBound :: Word64)) [required,limit] =
          Just (BS.singleton 1 <> putU64 (fromInteger required) <> putU64 (fromInteger limit))
      | otherwise = Nothing
    referenceFrame remaining reference
      | remaining < 4 = Nothing
      | otherwise =
          let limit = min (remaining - 4) (toInteger (maxBound :: Word32))
              prefix = LBS.take (fromInteger (limit + 1)) (LTE.encodeUtf8 (LT.pack reference))
          in if toInteger (LBS.length prefix) > limit then Nothing else
            let bytes = LBS.toStrict prefix
            in if BS.null bytes || BS.elem 0 bytes then Nothing else
              Just (putU32 (BS.length bytes) <> bytes)
    rejectionTag UnsupportedOperation = 0
    rejectionTag InvalidRealization = 1
    rejectionTag InvalidRoot = 2
    rejectionTag AcquisitionRejected = 3

data RequestField
  = Input FilePath
  | OutputDir FilePath
  | Target String
  | Targets [String]
  | TargetModuleOnly
  | Include FilePath
  | BindGen Word64
  | SessionRoot FilePath
  | SessionIncarnation String
  | InjectVal String
  | Turn
  | TurnTemplate String FilePath
  | TurnOut FilePath
  | TurnVerdict String
  | Classify
  | ClassifyOut FilePath
  | Cell
  | CellPlan
  | CheckSource
  | CellTemplate FilePath
  | CellOut FilePath
  | HarnessProfile
  | BuildProductsDir FilePath
  | ModuleCandidates FilePath
  | CertifyHomeProducts
  | SessionArtifacts FilePath
  | DeclarationJoin FilePath
  | DeclarationJoinOut FilePath
  | InspectType String
  | InspectInfo String
  | InspectBrowse String
  | InspectBrowseExpanded String
  | InspectScopeBrowse
  | InspectBrowseBounded String Bool Int
  | InspectSearch String
  | InspectSearchBounded String Int
  | InspectStructuredInfo StructuredInspection
  | InspectStructuredType StructuredInspection
  | InspectOut FilePath
  | InspectTypeBatch FilePath
  | RetainedGeneration SymbolIdentity Word64
  | ActivationPreview
  | InspectionStrict
  | PortableOutput
  | EnsureExactInputs ExactInputsRequest
  | WorldQuery WorldQueryRequest
  | WorldCellPlan WorldCellPlanRequest
  | WorldCellPrepare WorldCellPrepareRequest
  deriving (Eq, Show)

-- | A decoded compiler-worker invocation. This is the Haskell boundary's
-- domain request; the executable never reconstructs or parses CLI syntax.
data WorkerRequest = WorkerRequest
  { requestWorkload :: CompileWorkload
  , requestCompilerJobs :: Int
  , requestCompilerCapabilities :: Int
  , requestOutDir :: Maybe FilePath
  , requestTarget :: Maybe String
  , requestTargets :: [String]
  , requestTargetModuleOnly :: Bool
  , requestIncludes :: [FilePath]
  , requestFiles :: [FilePath]
  , requestBindGen :: Maybe Word64
  , requestSessionRoot :: Maybe FilePath
  -- | This session's incarnation identity (decimal text, the Rust
  -- @SessionId@), when the caller has one. Lets the resident worker's
  -- 'Tidepool.GhcPipeline.GutsMemo' retain a @Tidepool.Session.*@ entry
  -- across transactions within one incarnation instead of evicting it
  -- unconditionally at every transaction boundary; absent for a one-shot
  -- compile or an older caller, which keeps today's full eviction.
  , requestSessionIncarnation :: Maybe String
  , requestInjectVals :: [String]
  , requestTurn :: Bool
  , requestTurnTemplates :: [(String, FilePath)]
  , requestTurnOut :: Maybe FilePath
  , requestTurnVerdict :: Maybe String
  , requestClassify :: Bool
  , requestClassifyOut :: Maybe FilePath
  , requestCell :: Bool
  , requestCheckSource :: Bool
  , requestCellPlan :: Bool
  , requestCellTemplate :: Maybe FilePath
  , requestCellOut :: Maybe FilePath
  , requestHarnessProfile :: Bool
  , requestBuildProductsDir :: Maybe FilePath
  , requestModuleCandidates :: Maybe FilePath
  , requestCertifyHomeProducts :: Bool
  , requestSessionArtifacts :: Maybe FilePath
  , requestDeclarationJoin :: Maybe FilePath
  , requestDeclarationJoinOut :: Maybe FilePath
  , requestInspections :: [InspectionRequest]
  , requestInspectOut :: Maybe FilePath
  , requestInspectTypeBatch :: Maybe FilePath
  -- | Executable imports: symbols the caller has already retained at a prior
  -- generation. The projection excludes each one from recovery and declares
  -- it as a global carrying that generation, even when its defining module
  -- is compiled alongside this request as a home module.
  , requestRetainedGenerations :: Map SymbolIdentity Word64
  -- | A turn that also writes its target's prepared-STG program from the
  -- same compile, linked against 'requestRetainedGenerations'.
  , requestActivationPreview :: Bool
  , requestPortableOutput :: Bool
  , requestEnsureExactInputs :: Maybe ExactInputsRequest
  , requestWorldQuery :: Maybe WorldQueryRequest
  , requestWorldCellPlan :: Maybe WorldCellPlanRequest
  , requestWorldCellPrepare :: Maybe WorldCellPrepareRequest
  , requestInspectionStrict :: Bool
  }
  deriving (Eq, Show)

-- | One operation admitted from the externally encoded request fields. Preview
-- refines a turn; declaration operations own both their manifest and output.
data RequestOperation
  = EnsureExactInputsOperation ExactInputsRequest
  | WorldQueryOperation WorldQueryRequest
  | WorldCellPlanOperation WorldCellPlanRequest
  | WorldCellPrepareOperation WorldCellPrepareRequest
  | SourceOperation
  | TurnOperation
  | ActivationPreviewOperation
  | ClassificationOperation
  | InspectionOperation
  | CellOperation
  | CellPlanOperation
  | SourceCheckOperation
  | DeclarationOperation FilePath FilePath
  deriving (Eq, Show)

data RequestShapeError
  = InvalidExactInputsShape
  | InvalidWorldQueryShape
  | InvalidWorldCellShape
  | InvalidSourceCheckShape
  | InvalidPortableOutputShape
  | InvalidCellPlanShape
  | IncompleteDeclarationOperation
  | ConflictingRequestOperations
  | PreviewRequiresTurn
  deriving (Eq, Show)

-- Keep mode shape policy pure. Manifest contents and exact-scope authority are
-- admitted by Main at the point where their I/O is performed.
validateRequestShape :: WorkerRequest -> Either RequestShapeError ()
validateRequestShape args = () <$ admitRequestOperation args

admitRequestOperation :: WorkerRequest -> Either RequestShapeError RequestOperation
admitRequestOperation args
  | Just request <- requestEnsureExactInputs args =
      if validExactInputsRequest request && args == emptyWorkerRequest
        { requestWorkload = requestWorkload args
        , requestCompilerJobs = requestCompilerJobs args
        , requestCompilerCapabilities = requestCompilerCapabilities args
        , requestEnsureExactInputs = Just request }
      then Right (EnsureExactInputsOperation request)
      else Left InvalidExactInputsShape
  | Just request <- requestWorldQuery args =
      if validWorldQueryRequest request && args == emptyWorkerRequest
        { requestWorkload = requestWorkload args
        , requestCompilerJobs = requestCompilerJobs args
        , requestCompilerCapabilities = requestCompilerCapabilities args
        , requestWorldQuery = Just request }
      then Right (WorldQueryOperation request)
      else Left InvalidWorldQueryShape
  | Just request <- requestWorldCellPlan args =
      if validWorldCellPlanRequest request && args == emptyWorkerRequest
        { requestWorkload = requestWorkload args
        , requestCompilerJobs = requestCompilerJobs args
        , requestCompilerCapabilities = requestCompilerCapabilities args
        , requestWorldCellPlan = Just request }
      then Right (WorldCellPlanOperation request)
      else Left InvalidWorldCellShape
  | Just request <- requestWorldCellPrepare args =
      if validWorldCellPrepareRequest request && args == emptyWorkerRequest
        { requestWorkload = requestWorkload args
        , requestCompilerJobs = requestCompilerJobs args
        , requestCompilerCapabilities = requestCompilerCapabilities args
        , requestWorldCellPrepare = Just request }
      then Right (WorldCellPrepareOperation request)
      else Left InvalidWorldCellShape
  | requestCheckSource args && not validSourceCheck = Left InvalidSourceCheckShape
  | requestCellPlan args && not validCellPlan = Left InvalidCellPlanShape
  | requestActivationPreview args && not (requestTurn args) = Left PreviewRequiresTurn
  | requestPortableOutput args && not validPortableOutput = Left InvalidPortableOutputShape
  | otherwise = do
      declaration <- case (requestDeclarationJoin args, requestDeclarationJoinOut args) of
        (Nothing, Nothing) -> Right []
        (Just manifest, Just output) -> Right [DeclarationOperation manifest output]
        _ -> Left IncompleteDeclarationOperation
      if requestCertifyHomeProducts args && not (null (declaration ++ operations))
        then Left ConflictingRequestOperations
        else pure ()
      case declaration ++ operations of
        [] -> Right SourceOperation
        [operation] -> Right operation
        _ -> Left ConflictingRequestOperations
  where
    operations = [operation | (selected, operation) <-
      [ (requestTurn args, if requestActivationPreview args then ActivationPreviewOperation else TurnOperation)
      , (requestClassify args, ClassificationOperation)
      , (not (null (requestInspections args)), InspectionOperation)
      , (requestCell args, CellOperation)
      , (requestCellPlan args, CellPlanOperation)
      , (requestCheckSource args, SourceCheckOperation)
      ] , selected]
    validPortableOutput = length (requestFiles args) == 1
      && present (requestOutDir args)
      && (maybe False (not . null) (requestTarget args) || not (null (requestTargets args)))
      && maybe True (not . null) (requestTarget args) && all (not . null) (requestTargets args)
      && null operations && not hasSessionScope
      && not (present (requestSessionIncarnation args)) && not (present (requestBindGen args))
      && not (present (requestModuleCandidates args)) && not (requestCertifyHomeProducts args)
      && not (present (requestDeclarationJoin args)) && not (present (requestDeclarationJoinOut args))
      && not (requestTargetModuleOnly args) && Map.null (requestRetainedGenerations args)
    validSourceCheck = length (requestFiles args) == 1 && not (requestCell args)
      && not (requestCellPlan args) && not (requestTurn args) && not (requestClassify args)
      && null (requestInspections args) && not hasSessionScope
      && not (present (requestDeclarationJoin args)) && not (present (requestDeclarationJoinOut args))
      && not (requestCertifyHomeProducts args) && not (requestActivationPreview args)
      && not (requestHarnessProfile args)
      && not (present (requestOutDir args)) && not (present (requestTarget args))
      && null (requestTargets args) && not (present (requestBindGen args))
      && null (requestTurnTemplates args) && not (present (requestTurnOut args))
      && not (present (requestTurnVerdict args))
      && not (present (requestCellOut args)) && not (present (requestCellTemplate args))
      && not (present (requestClassifyOut args)) && not (present (requestInspectOut args))
      && not (present (requestInspectTypeBatch args)) && not (present (requestSessionArtifacts args))
      && not (present (requestSessionIncarnation args))
      && not (requestTargetModuleOnly args) && not (requestInspectionStrict args)
      && Map.null (requestRetainedGenerations args)
    validCellPlan = length (requestFiles args) == 1 && not (requestCell args)
      && not (requestTurn args) && not (requestClassify args)
      && null (requestInspections args) && not (present (requestSessionArtifacts args))
      && not (present (requestDeclarationJoin args)) && not (present (requestModuleCandidates args))
      && not (requestCertifyHomeProducts args) && not (requestActivationPreview args)
      && not (present (requestBindGen args))
      && Map.null (requestRetainedGenerations args)
    hasSessionScope = not (null (requestInjectVals args))
      || present (requestSessionRoot args) || present (requestSessionArtifacts args)
    present = maybe False (const True)

emptyWorkerRequest :: WorkerRequest
emptyWorkerRequest = WorkerRequest
  { requestWorkload = Foreground
  , requestCompilerJobs = 1
  , requestCompilerCapabilities = 1
  , requestOutDir = Nothing
  , requestTarget = Nothing
  , requestTargets = []
  , requestTargetModuleOnly = False
  , requestIncludes = []
  , requestFiles = []
  , requestBindGen = Nothing
  , requestSessionRoot = Nothing
  , requestSessionIncarnation = Nothing
  , requestInjectVals = []
  , requestTurn = False
  , requestTurnTemplates = []
  , requestTurnOut = Nothing
  , requestTurnVerdict = Nothing
  , requestClassify = False
  , requestClassifyOut = Nothing
  , requestCell = False
  , requestCheckSource = False
  , requestCellPlan = False
  , requestCellTemplate = Nothing
  , requestCellOut = Nothing
  , requestHarnessProfile = False
  , requestBuildProductsDir = Nothing
  , requestModuleCandidates = Nothing
  , requestCertifyHomeProducts = False
  , requestSessionArtifacts = Nothing
  , requestDeclarationJoin = Nothing
  , requestDeclarationJoinOut = Nothing
  , requestInspections = []
  , requestInspectOut = Nothing
  , requestInspectTypeBatch = Nothing
  , requestRetainedGenerations = Map.empty
  , requestActivationPreview = False
  , requestPortableOutput = False
  , requestEnsureExactInputs = Nothing
  , requestWorldQuery = Nothing
  , requestWorldCellPlan = Nothing
  , requestWorldCellPrepare = Nothing
  , requestInspectionStrict = False
  }

data InspectionRequest
  = InspectTypeOf String
  | InspectNameInfo String
  | InspectModule String Bool
  | InspectModuleBounded String Bool Int
  | InspectScope
  | InspectTypeSearch String
  | InspectTypeSearchBounded String Int
  | InspectStructuredInfoOf StructuredInspection
  | InspectStructuredTypeOf StructuredInspection
  | InspectKeyedAvailability StructuredInspection String
  deriving (Eq, Show)

data StructuredNameScope
  = StructuredCurrentScope
  | StructuredPublicModule String
  deriving (Eq, Show)

data StructuredNameNamespace
  = StructuredAnyName
  | StructuredValueName
  | StructuredTypeName
  | StructuredConstructorName
  | StructuredFieldName
  deriving (Eq, Show)

data ExactNameInspection = ExactNameInspection
  { exactNameScope :: StructuredNameScope, exactNameNamespace :: StructuredNameNamespace, exactNameQuery :: String
  } deriving (Eq, Show)

data InspectionProvenance = InspectionProvenance
  { inspectionGeneration :: Word64,
    inspectionFingerprint :: String
  }
  deriving (Eq, Show)

data StructuredInspection = StructuredInspection
  { structuredScope :: StructuredNameScope,
    structuredNamespace :: StructuredNameNamespace,
    structuredName :: String,
    structuredProvenance :: InspectionProvenance
  }
  deriving (Eq, Show)

requestFromFields :: [RequestField] -> WorkerRequest
requestFromFields = finish . foldl' apply emptyWorkerRequest
  where
    finish request = request
      { requestTargets = reverse (requestTargets request)
      , requestIncludes = reverse (requestIncludes request)
      , requestFiles = reverse (requestFiles request)
      , requestInjectVals = reverse (requestInjectVals request)
      , requestTurnTemplates = reverse (requestTurnTemplates request)
      , requestInspections = reverse (requestInspections request)
      }
    apply request field = case field of
      Input path -> request { requestFiles = path : requestFiles request }
      OutputDir path -> request { requestOutDir = Just path }
      Target name -> request { requestTarget = Just name }
      Targets names -> request { requestTargets = foldl' (flip (:)) (requestTargets request) names }
      TargetModuleOnly -> request { requestTargetModuleOnly = True }
      Include path -> request { requestIncludes = path : requestIncludes request }
      BindGen generation -> request { requestBindGen = Just generation }
      SessionRoot path -> request { requestSessionRoot = Just path }
      SessionIncarnation incarnation -> request { requestSessionIncarnation = Just incarnation }
      InjectVal name -> request { requestInjectVals = name : requestInjectVals request }
      Turn -> request { requestTurn = True }
      TurnTemplate kind path -> request
        { requestTurnTemplates = (kind, path) : requestTurnTemplates request }
      TurnOut path -> request { requestTurnOut = Just path }
      TurnVerdict verdict -> request { requestTurnVerdict = Just verdict }
      Classify -> request { requestClassify = True }
      ClassifyOut path -> request { requestClassifyOut = Just path }
      Cell -> request { requestCell = True }
      CellPlan -> request { requestCellPlan = True }
      CheckSource -> request { requestCheckSource = True }
      CellTemplate path -> request { requestCellTemplate = Just path }
      CellOut path -> request { requestCellOut = Just path }
      HarnessProfile -> request { requestHarnessProfile = True }
      BuildProductsDir path -> request { requestBuildProductsDir = Just path }
      ModuleCandidates path -> request { requestModuleCandidates = Just path }
      CertifyHomeProducts -> request { requestCertifyHomeProducts = True }
      SessionArtifacts path -> request { requestSessionArtifacts = Just path }
      DeclarationJoin path -> request { requestDeclarationJoin = Just path }
      DeclarationJoinOut path -> request { requestDeclarationJoinOut = Just path }
      InspectType expression -> request
        { requestInspections = InspectTypeOf expression : requestInspections request }
      InspectInfo name -> request
        { requestInspections = InspectNameInfo name : requestInspections request }
      InspectBrowse name -> request
        { requestInspections = InspectModule name False : requestInspections request }
      InspectBrowseExpanded name -> request
        { requestInspections = InspectModule name True : requestInspections request }
      InspectScopeBrowse -> request
        { requestInspections = InspectScope : requestInspections request }
      InspectBrowseBounded name expanded budget -> request
        { requestInspections = InspectModuleBounded name expanded budget : requestInspections request }
      InspectSearch query -> request
        { requestInspections = InspectTypeSearch query : requestInspections request }
      InspectSearchBounded query budget -> request
        { requestInspections = InspectTypeSearchBounded query budget : requestInspections request }
      InspectStructuredInfo query -> request
        { requestInspections = InspectStructuredInfoOf query : requestInspections request }
      InspectStructuredType query -> request
        { requestInspections = InspectStructuredTypeOf query : requestInspections request }
      InspectOut path -> request { requestInspectOut = Just path }
      InspectTypeBatch path -> request { requestInspectTypeBatch = Just path }
      RetainedGeneration identity generation -> request
        { requestRetainedGenerations =
            Map.insert identity generation (requestRetainedGenerations request) }
      ActivationPreview -> request { requestActivationPreview = True }
      InspectionStrict -> request { requestInspectionStrict = True }
      PortableOutput -> request { requestPortableOutput = True }
      EnsureExactInputs inputs -> request { requestEnsureExactInputs = Just inputs }
      WorldQuery query -> request { requestWorldQuery = Just query }
      WorldCellPlan query -> request { requestWorldCellPlan = Just query }
      WorldCellPrepare query -> request { requestWorldCellPrepare = Just query }

workerRequestFlag :: String
workerRequestFlag = "--worker-request-v" ++ show workerRequestVersion

-- The launcher flag and byte framing describe the same grammar version.
workerRequestVersion :: Int
workerRequestVersion = 28

workerRequestMagic :: BS.ByteString
workerRequestMagic = TE.encodeUtf8 (T.pack ("TPREQ" ++ replicate (3 - length version) '0' ++ version))
  where version = show workerRequestVersion

workerArgv :: [RequestField] -> [String]
workerArgv fields = [workerRequestFlag, encodeHex (encodeRequest fields)]

-- | Decode the worker's complete argv. The compiler boundary deliberately has
-- no command-line grammar: exactly one versioned payload is accepted.
workerRequestFromArgv :: [String] -> Either String (Maybe WorkerRequest)
workerRequestFromArgv [] = Right Nothing
workerRequestFromArgv [flag] | flag == workerRequestFlag = Left (workerRequestFlag ++ ": payload is required")
workerRequestFromArgv [flag, payload]
  | flag == workerRequestFlag = do
      (workload, jobs, capabilities, fields) <- decodeHex payload >>= decodeRequest
      validateExactInputsFields fields
      validateCertificationFields fields
      pure (Just ((requestFromFields fields)
        { requestWorkload = workload, requestCompilerJobs = jobs
        , requestCompilerCapabilities = capabilities }))
workerRequestFromArgv _ = Left "worker requires exactly one versioned request"

-- Inspect the original fields: projection into WorkerRequest must not hide
-- duplicate operations or source/output authority beside a source-free exchange.
validateExactInputsFields :: [RequestField] -> Either String ()
validateExactInputsFields fields
  | null [() | EnsureExactInputs _ <- fields] && null [() | WorldQuery _ <- fields]
      && null [() | WorldCellPlan _ <- fields] && null [() | WorldCellPrepare _ <- fields] = Right ()
  | [EnsureExactInputs request] <- fields, validExactInputsRequest request = Right ()
  | [WorldQuery request] <- fields, validWorldQueryRequest request = Right ()
  | [WorldCellPlan request] <- fields, validWorldCellPlanRequest request = Right ()
  | [WorldCellPrepare request] <- fields, validWorldCellPrepareRequest request = Right ()
  | otherwise = Left "worker request: exact input acquisition must be source-free and exclusive"

validateCertificationFields :: [RequestField] -> Either String ()
validateCertificationFields fields
  | CertifyHomeProducts `notElem` fields = Right ()
  | length [() | Input _ <- fields] /= 1 =
      Left "home-product certification requires exactly one input"
  | all compatible fields = Right ()
  | otherwise = Left "home-product certification cannot be combined with another mode or injected products"
  where
    compatible field = case field of
      Input _ -> True
      OutputDir _ -> True
      Target _ -> True
      Targets _ -> True
      Include _ -> True
      SessionRoot _ -> True
      SessionArtifacts _ -> True
      HarnessProfile -> True
      BuildProductsDir _ -> True
      ModuleCandidates _ -> True
      CertifyHomeProducts -> True
      _ -> False

type Parser a = BS.ByteString -> Either String (a, BS.ByteString)

decodeRequest :: BS.ByteString -> Either String (CompileWorkload, Int, Int, [RequestField])
decodeRequest bytes = do
  let (magic, body) = BS.splitAt 8 bytes
  if magic /= workerRequestMagic
    then Left "worker request: unsupported magic or version"
    else do
      (workloadTag, afterWorkload) <- pWord8 body
      workload <- case workloadTag of
        0 -> Right Foreground
        1 -> Right Preparation
        _ -> Left "worker request: invalid workload"
      (jobs, afterJobs) <- pWord32 afterWorkload
      (capabilities, afterCapabilities) <- pWord32 afterJobs
      if jobs == 0 || capabilities == 0
        then Left "worker request: execution grant must be positive"
        else pure ()
      (count, rest) <- pWord32 afterCapabilities
      (fields, trailing) <- pN (fromIntegral count) pField rest
      if BS.null trailing
        then Right (workload, fromIntegral jobs, fromIntegral capabilities, fields)
        else Left "worker request: trailing bytes"

encodeRequest :: [RequestField] -> BS.ByteString
encodeRequest fields = workerRequestMagic <> BS.singleton 0 <> putU32 1 <> putU32 1 <> putU32 (length fields) <> BS.concat (map encodeField fields)

encodeField :: RequestField -> BS.ByteString
encodeField field = case field of
  Input value -> taggedText 1 value
  OutputDir value -> taggedText 2 value
  Target value -> taggedText 3 value
  Targets values -> BS.singleton 4 <> putU32 (length values) <> BS.concat (map textFrame values)
  TargetModuleOnly -> BS.singleton 7
  Include value -> taggedText 8 value
  BindGen value -> BS.singleton 11 <> putU64 value
  SessionRoot value -> taggedText 12 value
  SessionIncarnation value -> taggedText 44 value
  InjectVal value -> taggedText 13 value
  Turn -> BS.singleton 16
  TurnTemplate kind path -> BS.singleton 17 <> textFrame kind <> textFrame path
  TurnOut value -> taggedText 18 value
  TurnVerdict value -> taggedText 19 value
  Classify -> BS.singleton 20
  ClassifyOut value -> taggedText 21 value
  HarnessProfile -> BS.singleton 24
  BuildProductsDir value -> taggedText 25 value
  ModuleCandidates value -> taggedText 46 value
  CertifyHomeProducts -> BS.singleton 50
  SessionArtifacts value -> taggedText 49 value
  DeclarationJoin value -> taggedText 47 value
  DeclarationJoinOut value -> taggedText 48 value
  InspectType value -> taggedText 26 value
  InspectInfo value -> taggedText 27 value
  InspectOut value -> taggedText 28 value
  InspectBrowse value -> taggedText 29 value
  InspectBrowseExpanded value -> taggedText 30 value
  Cell -> BS.singleton 31
  CellPlan -> BS.singleton 51
  CheckSource -> BS.singleton 52
  CellTemplate value -> taggedText 32 value
  CellOut value -> taggedText 33 value
  InspectSearch value -> taggedText 35 value
  InspectStructuredInfo query -> BS.singleton 36 <> encodeStructuredInspection query
  InspectStructuredType query -> BS.singleton 37 <> encodeStructuredInspection query
  RetainedGeneration identity generation ->
    BS.singleton 38 <> encodeSymbolIdentity identity <> putU64 generation
  InspectTypeBatch value -> taggedText 40 value
  ActivationPreview -> BS.singleton 41
  InspectionStrict -> BS.singleton 42
  PortableOutput -> BS.singleton 53
  EnsureExactInputs (ExactInputsRequest root acquisition) ->
    BS.singleton 57 <> textFrame root <> case acquisition of
      ResidentExactInputs reference -> BS.singleton 0 <> textFrame reference
      CompleteExactInputs manifest -> BS.singleton 1 <> textFrame manifest
      SuccessorExactInputs predecessor reference manifest ->
        BS.singleton 2 <> textFrame predecessor <> textFrame reference <> textFrame manifest
  WorldCellPlan request -> BS.singleton 59
    <> textFrame (worldCellPlanIdentity request) <> textFrame (worldCellPlanRealization request)
    <> encodeWorldCellRecipe (worldCellPlanRecipe request)
    <> encodeWorldCellOperand (worldCellPlanOperand request) <> textFrame (worldCellPlanOutput request)
  WorldCellPrepare request -> BS.singleton 60
    <> textFrame (worldCellPrepareIdentity request) <> textFrame (worldCellPrepareRealization request)
    <> encodeWorldCellRecipe (worldCellPrepareRecipe request)
    <> encodeWorldCellOperand (worldCellPrepareOperand request)
    <> putU32 (BS.length (worldCellPreparePlan request)) <> worldCellPreparePlan request
    <> worldCellPrepareAdmission request <> putU64 (worldCellPrepareDefiningScope request)
    <> putU32 (length (worldCellPrepareSlots request)) <> foldMap encodeWorldCellSlot (worldCellPrepareSlots request)
    <> textFrame (worldCellPrepareDirectory request) <> textFrame (worldCellPrepareOutput request)
  WorldQuery request -> BS.singleton 58
    <> textFrame (worldQueryIdentity request) <> textFrame (worldQueryRealization request)
    <> encodeWorldQueryQuestion (worldQueryQuestion request) <> textFrame (worldQueryOutput request)
  InspectScopeBrowse -> BS.singleton 43
  InspectBrowseBounded value expanded budget ->
    BS.singleton 62 <> textFrame value <> BS.singleton (if expanded then 1 else 0) <> putU32 budget
  InspectSearchBounded value budget -> BS.singleton 63 <> textFrame value <> putU32 budget

encodeWorldCellOperand :: Maybe WorldCellOperandRequest -> BS.ByteString
encodeWorldCellOperand Nothing = BS.singleton 0
encodeWorldCellOperand (Just (WorldCellOperandRequest path sha)) = BS.singleton 1 <> textFrame path <> sha

pWorldCellOperand :: BS.ByteString -> Either String (Maybe WorldCellOperandRequest,BS.ByteString)
pWorldCellOperand bytes = case BS.uncons bytes of
  Just (0,rest) -> Right (Nothing,rest)
  Just (1,rest) -> do
    (path,afterPath) <- pCellText 65536 rest
    if BS.length afterPath < 32 then Left "worker request: truncated cell operand seal" else pure ()
    let (sha,afterSha) = BS.splitAt 32 afterPath
    Right (Just (WorldCellOperandRequest path sha),afterSha)
  _ -> Left "worker request: invalid cell operand tag"

encodeSymbolIdentity :: SymbolIdentity -> BS.ByteString
encodeSymbolIdentity identity =
  textFrame (T.unpack (symbolUnit identity))
    <> textFrame (T.unpack (symbolModule identity))
    <> textFrame (T.unpack (symbolNamespace identity))
    <> textFrame (T.unpack (symbolOccurrence identity))
    <> encodeMaybeText (symbolRecordParent identity)

encodeMaybeText :: Maybe T.Text -> BS.ByteString
encodeMaybeText Nothing = BS.singleton 0
encodeMaybeText (Just value) = BS.singleton 1 <> textFrame (T.unpack value)

encodeWorldCellRecipe :: WorldCellRecipe -> BS.ByteString
encodeWorldCellRecipe recipe = textFrame (worldCellSource recipe) <> textFrame (worldCellTemplate recipe)
  <> putU32 (length (worldCellTurnTemplates recipe))
  <> foldMap (\(kind,source) -> textFrame kind <> textFrame source) (worldCellTurnTemplates recipe)

encodeWorldCellSlot :: (Word64,PlannedCellSlot) -> BS.ByteString
encodeWorldCellSlot (ordinal,slot) = putU64 ordinal <> case slot of
  PlannedPrologue generation -> BS.singleton 0 <> putU64 generation
  PlannedDeclaration generation -> BS.singleton 1 <> putU64 generation
  PlannedBind generation -> BS.singleton 2 <> putU64 generation
  PlannedExpression generation observation -> BS.singleton 3 <> putU64 generation <> textFrame observation

encodeWorldQueryQuestion :: WorldQueryQuestion -> BS.ByteString
encodeWorldQueryQuestion question = case question of
  WorldPublicModuleQuery (WorldPublicInfo query) -> BS.singleton 0 <> encodeStructuredInspection query
  WorldPublicModuleQuery (WorldPublicType query) -> BS.singleton 1 <> encodeStructuredInspection query
  WorldScopedQuery query -> case query of
    WorldObservationInfo name -> BS.singleton 2 <> encodeStructuredInspection name
    WorldObservationType name -> BS.singleton 3 <> encodeStructuredInspection name
    WorldObservationRowFit name owner -> BS.singleton 4 <> encodeStructuredInspection name <> textFrame owner
    WorldObservationExpression provenance expression -> BS.singleton 5 <> pin provenance <> textFrame expression
    WorldObservationTypeSearch provenance expression limit -> BS.singleton 6 <> pin provenance <> textFrame expression <> putU32 limit
  where pin provenance = putU64 (inspectionGeneration provenance) <> textFrame (inspectionFingerprint provenance)

pWorldQueryQuestion :: Parser WorldQueryQuestion
pWorldQueryQuestion input = do
  (tag,afterTag) <- pWord8 input
  case tag of
    0 -> public WorldPublicInfo afterTag
    1 -> public WorldPublicType afterTag
    2 -> scoped WorldObservationInfo afterTag
    3 -> scoped WorldObservationType afterTag
    4 -> do
      (name,afterName) <- pStructuredInspection afterTag
      (owner,afterOwner) <- pText afterName
      pure (WorldScopedQuery (WorldObservationRowFit name owner),afterOwner)
    5 -> do
      (provenance,afterPin) <- pin afterTag
      (expression,afterExpression) <- pText afterPin
      pure (WorldScopedQuery (WorldObservationExpression provenance expression),afterExpression)
    6 -> do
      (provenance,afterPin) <- pin afterTag
      (expression,afterExpression) <- pText afterPin
      (limit,afterLimit) <- pWord32 afterExpression
      pure (WorldScopedQuery (WorldObservationTypeSearch provenance expression (fromIntegral limit)),afterLimit)
    _ -> Left "worker request: invalid world query question"
  where
    public constructor bytes = do
      (name,rest) <- pStructuredInspection bytes
      pure (WorldPublicModuleQuery (constructor name),rest)
    scoped constructor bytes = do
      (name,rest) <- pStructuredInspection bytes
      pure (WorldScopedQuery (constructor name),rest)
    pin bytes = do
      (generation,afterGeneration) <- pWord64 bytes
      (fingerprint,rest) <- pText afterGeneration
      pure (InspectionProvenance generation fingerprint,rest)

encodeStructuredInspection :: StructuredInspection -> BS.ByteString
encodeStructuredInspection query =
  encodeExactNameInspection (ExactNameInspection (structuredScope query) (structuredNamespace query) (structuredName query))
    <> putU64 (inspectionGeneration (structuredProvenance query))
    <> textFrame (inspectionFingerprint (structuredProvenance query))

encodeExactNameInspection :: ExactNameInspection -> BS.ByteString
encodeExactNameInspection query =
  encodeScope (exactNameScope query)
    <> BS.singleton (case exactNameNamespace query of
      StructuredAnyName -> 0; StructuredValueName -> 1; StructuredTypeName -> 2
      StructuredConstructorName -> 3; StructuredFieldName -> 4)
    <> textFrame (exactNameQuery query)
  where
    encodeScope scope = case scope of
      StructuredCurrentScope -> BS.singleton 0
      StructuredPublicModule moduleName -> BS.singleton 1 <> textFrame moduleName

taggedText :: Word8 -> String -> BS.ByteString
taggedText tag value = BS.singleton tag <> textFrame value

textFrame :: String -> BS.ByteString
textFrame value = let bytes = TE.encodeUtf8 (T.pack value) in putU32 (BS.length bytes) <> bytes

putU32 :: Int -> BS.ByteString
putU32 value = BS.pack
  [ fromIntegral (value `shiftR` 0)
  , fromIntegral (value `shiftR` 8)
  , fromIntegral (value `shiftR` 16)
  , fromIntegral (value `shiftR` 24)
  ]

putU64 :: Word64 -> BS.ByteString
putU64 value = BS.pack [fromIntegral (value `shiftR` shift) | shift <- [0, 8 .. 56]]

encodeHex :: BS.ByteString -> String
encodeHex = concatMap byteHex . BS.unpack
  where
    digits = "0123456789abcdef"
    byteHex byte = [digits !! fromIntegral (byte `div` 16), digits !! fromIntegral (byte `mod` 16)]

pField :: Parser RequestField
pField bytes = do
  (tag, rest) <- pWord8 bytes
  case tag of
    1  -> mapParser Input pText rest
    2  -> mapParser OutputDir pText rest
    3  -> mapParser Target pText rest
    4  -> do
      (count, rest') <- pWord32 rest
      mapParser Targets (pN (fromIntegral count) pText) rest'
    5  -> retired tag
    6  -> Left "retired worker request field tag: 6"
    7  -> Right (TargetModuleOnly, rest)
    8  -> mapParser Include pText rest
    9  -> retired tag
    10 -> retired tag
    11 -> mapParser BindGen pWord64 rest
    12 -> mapParser SessionRoot pText rest
    13 -> mapParser InjectVal pText rest
    44 -> mapParser SessionIncarnation pText rest
    14 -> retired tag
    16 -> Right (Turn, rest)
    17 -> do
      (kind, rest') <- pText rest
      (path, rest'') <- pText rest'
      Right (TurnTemplate kind path, rest'')
    18 -> mapParser TurnOut pText rest
    19 -> mapParser TurnVerdict pText rest
    20 -> Right (Classify, rest)
    21 -> mapParser ClassifyOut pText rest
    24 -> Right (HarnessProfile, rest)
    25 -> mapParser BuildProductsDir pText rest
    46 -> mapParser ModuleCandidates pText rest
    50 -> Right (CertifyHomeProducts, rest)
    49 -> mapParser SessionArtifacts pText rest
    47 -> mapParser DeclarationJoin pText rest
    48 -> mapParser DeclarationJoinOut pText rest
    26 -> mapParser InspectType pText rest
    27 -> mapParser InspectInfo pText rest
    28 -> mapParser InspectOut pText rest
    29 -> mapParser InspectBrowse pText rest
    30 -> mapParser InspectBrowseExpanded pText rest
    31 -> Right (Cell, rest)
    51 -> Right (CellPlan, rest)
    52 -> Right (CheckSource, rest)
    32 -> mapParser CellTemplate pText rest
    33 -> mapParser CellOut pText rest
    34 -> retired tag
    35 -> mapParser InspectSearch pText rest
    36 -> mapParser InspectStructuredInfo pStructuredInspection rest
    37 -> mapParser InspectStructuredType pStructuredInspection rest
    38 -> do
      (identity, rest') <- pSymbolIdentity rest
      (generation, rest'') <- pWord64 rest'
      Right (RetainedGeneration identity generation, rest'')
    39 -> Left "retired worker request field tag: 39"
    40 -> mapParser InspectTypeBatch pText rest
    41 -> Right (ActivationPreview, rest)
    42 -> Right (InspectionStrict, rest)
    53 -> Right (PortableOutput, rest)
    57 -> do
      (root, afterRoot) <- pText rest
      (kind, afterKind) <- pWord8 afterRoot
      (acquisition, afterValue) <- case kind of
        0 -> mapParser ResidentExactInputs pText afterKind
        1 -> mapParser CompleteExactInputs pText afterKind
        2 -> do
          (predecessor, afterPredecessor) <- pText afterKind
          (reference, afterReference) <- pText afterPredecessor
          (manifest, remaining) <- pText afterReference
          Right (SuccessorExactInputs predecessor reference manifest, remaining)
        _ -> Left "worker request: invalid exact input acquisition tag"
      let request = ExactInputsRequest root acquisition
      if validExactInputsRequest request then Right (EnsureExactInputs request, afterValue)
        else Left "worker request: invalid exact input reference"
    43 -> Right (InspectScopeBrowse, rest)
    62 -> do
      (name, afterName) <- pText rest
      (expandedTag, afterExpanded) <- pWord8 afterName
      expanded <- case expandedTag of
        0 -> Right False
        1 -> Right True
        _ -> Left "worker request: invalid bounded-browse expansion flag"
      (budget, afterBudget) <- pWord32 afterExpanded
      bounded "module browse" (InspectBrowseBounded name expanded . fromIntegral) budget afterBudget
    63 -> do
      (query, afterQuery) <- pText rest
      (budget, afterBudget) <- pWord32 afterQuery
      bounded "type search" (InspectSearchBounded query . fromIntegral) budget afterBudget
    58 -> do
      (root, afterRoot) <- pText rest
      (reference, afterReference) <- pText afterRoot
      (question,afterQuestion) <- pWorldQueryQuestion afterReference
      (output,afterOutput) <- pText afterQuestion
      let request = WorldQueryRequest root reference question output
      if validWorldQueryRequest request then Right (WorldQuery request,afterOutput)
        else Left "worker request: invalid world query"
    59 -> do
      (root,a) <- pCellText 64 rest
      (reference,b) <- pText a
      (recipe,c) <- pWorldCellRecipe b
      (operand,afterOperand) <- pWorldCellOperand c
      (output,d) <- pCellText 65536 afterOperand
      let request = WorldCellPlanRequest root reference recipe operand output
      if validWorldCellPlanRequest request then Right (WorldCellPlan request,d)
        else Left "worker request: invalid held world cell plan"
    60 -> do
      (root,a) <- pCellText 64 rest
      (reference,b) <- pText a
      (recipe,c) <- pWorldCellRecipe b
      (operand,afterOperand) <- pWorldCellOperand c
      (plan,d) <- pCellFrame (8 * 1024 * 1024) afterOperand
      if BS.length d < 32 then Left "worker request: truncated cell admission" else pure ()
      let (admission,e) = BS.splitAt 32 d
      (definingScope,afterScope) <- pWord64 e
      (slots,f) <- pCellList 10000 17 pWorldCellSlot afterScope
      (directory,g) <- pCellText 65536 f
      (output,h) <- pCellText 65536 g
      let request = WorldCellPrepareRequest root reference recipe operand plan admission definingScope slots directory output
      if validWorldCellPrepareRequest request then Right (WorldCellPrepare request,h)
        else Left "worker request: invalid held world cell preparation"
    45 -> retired tag
    _  -> Left ("worker request: unknown field tag " ++ show tag)
  where
    retired tag = Left ("worker request: retired field tag " ++ show tag)
    bounded label build budget afterBudget
      | budget == 0 = Left ("worker request: " ++ label ++ " budget must be positive")
      | otherwise = Right (build budget, afterBudget)

-- Check operational bounds before decoding UTF-8 or allocating collections.
pCellFrame :: Int -> Parser BS.ByteString
pCellFrame limit bytes = do
  (value,rest) <- pFrame bytes
  if BS.length value <= limit then Right (value,rest)
    else Left "worker request: cell field exceeds operational bound"

pCellText :: Int -> Parser String
pCellText limit bytes = do
  (value,rest) <- pCellFrame limit bytes
  case TE.decodeUtf8' value of
    Left _ -> Left "worker request: invalid cell UTF-8"
    Right text -> Right (T.unpack text,rest)

pCellList :: Int -> Int -> Parser a -> Parser [a]
pCellList limit minimumBytes item bytes = do
  (count,rest) <- pWord32 bytes
  if toInteger count > toInteger limit || toInteger count > toInteger (BS.length rest `div` minimumBytes)
    then Left "worker request: cell collection exceeds operational bound or remaining bytes"
    else pN (fromIntegral count) item rest

pWorldCellRecipe :: Parser WorldCellRecipe
pWorldCellRecipe bytes = do
  (source,a) <- pCellText (2 * 1024 * 1024) bytes
  (template,b) <- pCellText (2 * 1024 * 1024) a
  (templates,c) <- pCellList 256 8 (\input -> do
    (kind,next) <- pCellText 65536 input
    (body,rest) <- pCellText (2 * 1024 * 1024) next
    Right ((kind,body),rest)) b
  Right (WorldCellRecipe source template templates,c)

pWorldCellSlot :: Parser (Word64,PlannedCellSlot)
pWorldCellSlot bytes = do
  (ordinal,a) <- pWord64 bytes
  (tag,b) <- pWord8 a
  (generation,c) <- pWord64 b
  case tag of
    0 -> Right ((ordinal,PlannedPrologue generation),c)
    1 -> Right ((ordinal,PlannedDeclaration generation),c)
    2 -> Right ((ordinal,PlannedBind generation),c)
    3 -> do
      (observation,d) <- pCellText 65536 c
      Right ((ordinal,PlannedExpression generation observation),d)
    _ -> Left "worker request: invalid cell reservation tag"

mapParser :: (a -> b) -> Parser a -> Parser b
mapParser f parser bytes = do
  (value, rest) <- parser bytes
  Right (f value, rest)

pStructuredInspection :: Parser StructuredInspection
pStructuredInspection bytes = do
  (ExactNameInspection scope namespace name, rest) <- pExactNameInspection bytes
  (generation, rest') <- pWord64 rest
  (fingerprint, trailing) <- pText rest'
  Right (StructuredInspection scope namespace name (InspectionProvenance generation fingerprint), trailing)

pExactNameInspection :: Parser ExactNameInspection
pExactNameInspection bytes = do
  (scopeTag, rest) <- pWord8 bytes
  (scope, rest') <- case scopeTag of
    0 -> Right (StructuredCurrentScope, rest)
    1 -> mapParser StructuredPublicModule pText rest
    _ -> Left ("worker request: unknown structured inspection scope " ++ show scopeTag)
  (namespaceTag, rest'') <- pWord8 rest'
  namespace <- case namespaceTag of
    0 -> Right StructuredAnyName
    1 -> Right StructuredValueName
    2 -> Right StructuredTypeName
    3 -> Right StructuredConstructorName
    4 -> Right StructuredFieldName
    _ -> Left ("worker request: unknown structured inspection namespace " ++ show namespaceTag)
  (name, rest''') <- pText rest''
  Right (ExactNameInspection scope namespace name, rest''')

pSymbolIdentity :: Parser SymbolIdentity
pSymbolIdentity bytes = do
  (unit, r1) <- pText bytes
  (modul, r2) <- pText r1
  (namespace, r3) <- pText r2
  (occurrence, r4) <- pText r3
  (recordParent, r5) <- pMaybeText r4
  Right
    ( SymbolIdentity (T.pack unit) (T.pack modul) (T.pack namespace) (T.pack occurrence)
        (T.pack <$> recordParent)
    , r5
    )

pMaybeText :: Parser (Maybe String)
pMaybeText bytes = do
  (tag, rest) <- pWord8 bytes
  case tag of
    0 -> Right (Nothing, rest)
    1 -> mapParser Just pText rest
    _ -> Left ("worker request: unknown optional-text tag " ++ show tag)

pN :: Int -> Parser a -> Parser [a]
pN 0 _ bytes = Right ([], bytes)
pN count parser bytes = do
  (value, rest) <- parser bytes
  (values, trailing) <- pN (count - 1) parser rest
  Right (value : values, trailing)

pWord8 :: Parser Word8
pWord8 bytes = case BS.uncons bytes of
  Nothing -> Left "worker request: truncated field tag"
  Just pair -> Right pair

pWord32 :: Parser Word32
pWord32 bytes
  | BS.length bytes < 4 = Left "worker request: truncated u32"
  | otherwise =
      let b0 = BS.index bytes 0
          b1 = BS.index bytes 1
          b2 = BS.index bytes 2
          b3 = BS.index bytes 3
          value = fromIntegral b0
            .|. (fromIntegral b1 `shiftL` 8)
            .|. (fromIntegral b2 `shiftL` 16)
            .|. (fromIntegral b3 `shiftL` 24)
      in Right (value, BS.drop 4 bytes)

pWord64 :: Parser Word64
pWord64 bytes
  | BS.length bytes < 8 = Left "worker request: truncated u64"
  | otherwise =
      let octets = zip [0, 8 .. 56] (BS.unpack (BS.take 8 bytes))
          value = foldr (\(shift, byte) acc -> acc .|. (fromIntegral byte `shiftL` shift)) 0 octets
      in Right (value, BS.drop 8 bytes)

pFrame :: Parser BS.ByteString
pFrame bytes = do
  (len, rest) <- pWord32 bytes
  let count = fromIntegral len
  if BS.length rest < count
    then Left "worker request: truncated frame"
    else Right (BS.take count rest, BS.drop count rest)

pText :: Parser String
pText bytes = do
  (frame, rest) <- pFrame bytes
  case TE.decodeUtf8' frame of
    Left err -> Left ("worker request: invalid UTF-8: " ++ show err)
    Right value -> Right (T.unpack value, rest)

decodeHex :: String -> Either String BS.ByteString
decodeHex input
  | odd (length input) = Left "worker request: hex payload has odd length"
  | otherwise = BS.pack <$> pairs input
  where
    pairs [] = Right []
    pairs (hi : lo : rest)
      | isHexDigit hi && isHexDigit lo =
          let byte = fromIntegral (digitToInt hi * 16 + digitToInt lo)
          in (byte :) <$> pairs rest
      | otherwise = Left "worker request: hex payload contains a non-hex digit"
    pairs _ = Left "worker request: hex payload has odd length"
