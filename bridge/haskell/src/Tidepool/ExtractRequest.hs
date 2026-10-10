-- | Versioned request protocol between the Rust launcher and the Haskell
-- compiler worker. The worker receives domain fields, not command-line
-- options; CLI compatibility is owned by the launcher layer.
module Tidepool.ExtractRequest
  ( CompileWorkload(..)
  , RequestField(..)
  , InspectionRequest(..)
  , StructuredInspection(..)
  , StructuredNameScope(..)
  , StructuredNameNamespace(..)
  , InspectionProvenance(..)
  , WorkerRequest(..)
  , RequestShapeError(..)
  , validateRequestShape
  , workerRequestFromArgv
  , workerArgv
  , workerRequestFlag
  ) where

import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import Data.Map.Strict (Map)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Data.Bits ((.|.), shiftL, shiftR)
import Data.Char (digitToInt, isHexDigit)
import Data.Word (Word32, Word64, Word8)
import Tidepool.ExecutionSchema (SymbolIdentity(..))

data CompileWorkload = Foreground | Preparation deriving (Eq, Show)

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
  | InspectSearch String
  | InspectStructuredInfo StructuredInspection
  | InspectStructuredType StructuredInspection
  | InspectOut FilePath
  | InspectTypeBatch FilePath
  | RetainedGeneration SymbolIdentity Word64
  | ActivationPreview
  | InspectionStrict
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
  , requestInspectionStrict :: Bool
  }
  deriving (Eq, Show)

data RequestShapeError = InvalidSourceCheckShape | InvalidCellPlanShape
  deriving (Eq, Show)

-- Keep mode shape policy pure. Manifest contents and exact-scope authority are
-- admitted by Main at the point where their I/O is performed.
validateRequestShape :: WorkerRequest -> Either RequestShapeError ()
validateRequestShape args
  | requestCheckSource args && not validSourceCheck = Left InvalidSourceCheckShape
  | requestCellPlan args && not validCellPlan = Left InvalidCellPlanShape
  | otherwise = Right ()
  where
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
  , requestInspectionStrict = False
  }

data InspectionRequest
  = InspectTypeOf String
  | InspectNameInfo String
  | InspectModule String Bool
  | InspectScope
  | InspectTypeSearch String
  | InspectStructuredInfoOf StructuredInspection
  | InspectStructuredTypeOf StructuredInspection
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
  deriving (Eq, Show)

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
      InspectSearch query -> request
        { requestInspections = InspectTypeSearch query : requestInspections request }
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

workerRequestFlag :: String
workerRequestFlag = "--worker-request-v18"

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
      validateCertificationFields fields
      pure (Just ((requestFromFields fields)
        { requestWorkload = workload, requestCompilerJobs = jobs
        , requestCompilerCapabilities = capabilities }))
workerRequestFromArgv _ = Left "worker requires exactly one versioned request"

-- Certification produces originals for every fresh home module. Authenticated
-- candidates may supply unchanged originals through the same compiler pipeline;
-- another dispatch mode cannot replace certification of the fresh source graph.
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
  if magic /= "TPREQ018"
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
encodeRequest fields = "TPREQ018" <> BS.singleton 0 <> putU32 1 <> putU32 1 <> putU32 (length fields) <> BS.concat (map encodeField fields)

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
  InspectScopeBrowse -> BS.singleton 43

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

encodeStructuredInspection :: StructuredInspection -> BS.ByteString
encodeStructuredInspection query =
  encodeScope (structuredScope query)
    <> BS.singleton (case structuredNamespace query of
      StructuredAnyName -> 0
      StructuredValueName -> 1
      StructuredTypeName -> 2
      StructuredConstructorName -> 3)
    <> textFrame (structuredName query)
    <> putU64 (inspectionGeneration (structuredProvenance query))
    <> textFrame (inspectionFingerprint (structuredProvenance query))
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
    43 -> Right (InspectScopeBrowse, rest)
    45 -> retired tag
    _  -> Left ("worker request: unknown field tag " ++ show tag)
  where
    retired tag = Left ("worker request: retired field tag " ++ show tag)

mapParser :: (a -> b) -> Parser a -> Parser b
mapParser f parser bytes = do
  (value, rest) <- parser bytes
  Right (f value, rest)

pStructuredInspection :: Parser StructuredInspection
pStructuredInspection bytes = do
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
    _ -> Left ("worker request: unknown structured inspection namespace " ++ show namespaceTag)
  (name, rest''') <- pText rest''
  (generation, rest'''') <- pWord64 rest'''
  (fingerprint, trailing) <- pText rest''''
  Right (StructuredInspection scope namespace name (InspectionProvenance generation fingerprint), trailing)

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
