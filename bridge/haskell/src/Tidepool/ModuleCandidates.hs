-- | Optional, bounded cache suggestions. The compiler checks each candidate
-- against its current downsweep before a source module may be skipped.
module Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateImport(..), CandidateQualifier(..)
  , CandidateGroup(..), CandidateGlobal(..)
  , CandidateModuleInterface, candidateCertificatePath, candidateCertificateSha256
  , candidateCoreDescriptor
  , CandidateExecutionSource, candidateExecutionSources, candidateOriginalIdentity
  , CapturedCandidateManifest, captureCandidateManifest, candidateManifestSha256
  , CandidateContextIdentity, candidateContextIdentity
  , readCapturedModuleCandidatesWithGraphs
  , readModuleCandidates, readModuleCandidatesWithGraphs ) where

import Codec.CBOR.Decoding
  ( Decoder, TokenType(..), decodeBool, decodeListLen, decodeNull
  , decodeString, decodeWord, peekTokenType )
import Codec.CBOR.Read (deserialiseFromBytes)
import Control.Exception (IOException, try)
import Control.Monad (forM_, replicateM, unless, when)
import Data.Char (isHexDigit)
import Data.List (stripPrefix)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BS8
import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString.Lazy as BL
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import qualified Data.IntMap.Strict as IntMap
import qualified Data.Text.Encoding as TE
import qualified Data.Text as T
import Tidepool.ArtifactBytes (ArtifactBytes, captureArtifactBytes, artifactBytes, artifactDigestBytes, checkArtifactSeal)
import Tidepool.RequestInputs (RequestOriginalInputs, captureRequestInputTokens, continueRequestInputs, emptyCapturedOriginalContent, OriginalInputReference(..))
import Tidepool.OwnedInputTransport (OriginalInputKind(..), OriginalInputImage(..), InputAcquisition(..), decodeInputAcquisition)
import Tidepool.BoundedRead (readFileAtMost)
import System.FilePath (isAbsolute)
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..), RuntimeRep(..), Signature(..), ResultContract(..) )
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), executionGraphBytes, executionGraphSha256, ExecutionSourceIdentity(..), ExecutionSourceOwner(..), ExecutionSourceRef(..)
  , decodeExecutionSourceDescriptors, decodeExecutionSourceReferences, readExecutionSourceGraphs, readExecutionSourceGraphsWithFacts, decodeExecutionSourceBody
  , executionSourceGraphsFit, executionIdentityKey, executionSourceOriginalClosure )

data ModuleCandidate = ModuleCandidate
  { candidateInputCustody :: Maybe RequestOriginalInputs
  , candidateUnit :: String
  , candidateModule :: String
  , candidateSource :: FilePath
  , candidateSourceSha256 :: String
  , candidateInterface :: FilePath
  , candidateInterfaceSha256 :: String
  , candidateModuleVersion :: String
  , candidateProductSha256 :: String
  , candidateEvidenceSha256 :: String
  , candidateImports :: [CandidateImport]
  , candidateGroups :: [CandidateGroup]
  , candidatePackageImports :: FilePath
  , candidatePackageImportsSha256 :: String
  , candidateProductPath :: FilePath
  , candidateExecutionSource :: Maybe CandidateExecutionSource
  , candidateProducerSha256 :: String
  , candidateInterfaceRequirements :: [(String,String)]
  , candidateModuleInterface :: CandidateModuleInterface
  } deriving (Eq, Show)

-- This decoded descriptor supplies provenance only. ExactScope owns its
-- promotion to a validated durable proof against the selected interface closure.
data CandidateModuleInterface = CandidateModuleInterface
  { candidateCertificatePath :: FilePath
  , candidateCertificateSha256 :: String
  , candidateCoreDescriptor :: (FilePath,String)
  } deriving (Eq, Show)

-- The offer supplies provenance only. Current GHC admission must succeed
-- before these original recipes may accompany promoted native products.
data CandidateExecutionSource = CandidateExecutionSource
  [ExecutionSourceGraph] ExecutionSourceRef deriving (Eq, Show)

candidateExecutionSources :: ModuleCandidate -> Maybe ([ExecutionSourceGraph], ExecutionSourceRef)
candidateExecutionSources candidate = case candidateExecutionSource candidate of
  Nothing -> Nothing
  Just (CandidateExecutionSource graphs reference) -> Just (graphs, reference)

candidateOriginalIdentity :: ModuleCandidate -> ExecutionSourceIdentity
candidateOriginalIdentity candidate = ExecutionSourceIdentity
  (candidateUnit candidate) (candidateModule candidate) (candidateModuleVersion candidate)
  (candidateInterfaceSha256 candidate) (candidateProductSha256 candidate)

data CandidateGroup = CandidateGroup
  { candidateGroupOrdinal :: Word
  , candidateGroupBinders :: [SymbolIdentity]
  , candidateGroupGlobals :: [CandidateGlobal]
  } deriving (Eq, Show)

data CandidateGlobal = CandidateGlobal
  { candidateGlobalIdentity :: SymbolIdentity
  , candidateGlobalRep :: RuntimeRep
  , candidateGlobalSignature :: Maybe Signature
  , candidateGlobalEvaluated :: Bool
  , candidateGlobalGeneration :: Maybe Word
  } deriving (Eq, Show)

data CandidateQualifier
  = CandidateUnqualified
  | CandidateThisUnit String
  | CandidateOtherUnit String
  deriving (Eq, Ord, Show)

data CandidateImport = CandidateImport
  { candidateImportQualifier :: CandidateQualifier
  , candidateImportModule :: String
  , candidateImportBoot :: Bool
  , candidateImportSelected :: Maybe FilePath
  } deriving (Eq, Ord, Show)

maxManifestBytes :: Integer
maxManifestBytes = 4 * 1024 * 1024

maxCandidates :: Int
maxCandidates = 128

readModuleCandidates :: FilePath -> IO (Either String [ModuleCandidate])
readModuleCandidates = readModuleCandidatesWithGraphs []

-- The envelope is captured once. Its fingerprint and later candidate admission
-- consume these same bounded bytes, including when the source file changes.
data CapturedCandidateManifest = CapturedCandidateManifest FilePath ArtifactBytes

candidateManifestSha256 :: CapturedCandidateManifest -> BS.ByteString
candidateManifestSha256 (CapturedCandidateManifest _ bytes) = artifactDigestBytes bytes

-- This is a selection key, never candidate admission. Current source,
-- products and canonical evidence still pass the normal admission path.
newtype CandidateContextIdentity = CandidateContextIdentity BS.ByteString
  deriving (Eq)

candidateContextIdentity :: CapturedCandidateManifest -> Either String CandidateContextIdentity
candidateContextIdentity (CapturedCandidateManifest _ body) =
  case deserialiseFromBytes decodeManifest (BL.fromStrict (artifactBytes body)) of
    Left failure -> Left (show failure)
    Right (remaining,(candidates,_,_,producer,_))
      | BL.null remaining -> Right (CandidateContextIdentity (SHA256.hash (BS8.pack
          (show (producer,map identity candidates)))))
      | otherwise -> Left "candidate manifest has trailing bytes"
  where
    identity candidate =
      (candidateUnit candidate,candidateModule candidate,candidateSourceSha256 candidate,
       candidateInterfaceSha256 candidate,candidateModuleVersion candidate,
       candidateProductSha256 candidate,candidateProducerSha256 candidate,
       candidateInterfaceRequirements candidate,
       candidateCertificateSha256 (candidateModuleInterface candidate),
       snd (candidateCoreDescriptor (candidateModuleInterface candidate)))

captureCandidateManifest :: FilePath -> IO (Either String CapturedCandidateManifest)
captureCandidateManifest path = do
  captured <- try (readFileAtMost path (fromInteger maxManifestBytes + 1))
    :: IO (Either IOException BS.ByteString)
  pure $ case captured of
    Left failure -> Left (show failure)
    Right bytes | toInteger (BS.length bytes) > maxManifestBytes ->
      Left "candidate manifest exceeds four MiB"
    Right bytes -> Right (CapturedCandidateManifest path (captureArtifactBytes bytes))

-- Exact-scope graphs have already passed their digest and producer checks.
-- Their inventory closes provenance; it does not grant lexical admission.
readModuleCandidatesWithGraphs
  :: [ExecutionSourceGraph] -> FilePath -> IO (Either String [ModuleCandidate])
readModuleCandidatesWithGraphs exactGraphs path = do
  captured <- captureCandidateManifest path
  case captured of
    Left reason -> pure (Left reason)
    Right manifest -> readCapturedModuleCandidatesWithGraphs exactGraphs manifest

readCapturedModuleCandidatesWithGraphs
  :: [ExecutionSourceGraph] -> CapturedCandidateManifest -> IO (Either String [ModuleCandidate])
readCapturedModuleCandidatesWithGraphs exactGraphs (CapturedCandidateManifest path body) = do
  result <- try (do
    case deserialiseFromBytes decodeManifest (BL.fromStrict (artifactBytes body)) of
      Left failure -> pure (Left (show failure))
      Right (remaining, (candidates, descriptors, references, producer, acquisition))
        | BL.null remaining -> do
            case acquisition of
              FreshFiles -> do
                graphs <- readExecutionSourceGraphs path exactGraphs descriptors
                pure (attachExecutionSources exactGraphs graphs references producer candidates)
              ContinueOwnedOriginals images -> do
                either fail pure (validateCandidateImages producer candidates descriptors references images)
                base <- continueRequestInputs emptyCapturedOriginalContent
                  [reference | OriginalInputImage _ _ _ parts <- images, (_,reference) <- parts]
                (graphs,custody) <- captureRequestInputTokens (Just base) $ \readToken -> do
                  forM_ candidates $ \candidate -> forM_ (candidateParts candidate) $ \(_,artifact,sha,bound) -> do
                    token <- readToken artifact bound
                    either fail pure (checkArtifactSeal sha token)
                  readExecutionSourceGraphsWithFacts (\sha artifact -> do
                    token <- readToken artifact (64 * 1024 * 1024)
                    either fail pure (checkArtifactSeal sha token)
                    either fail pure (decodeExecutionSourceBody token)) path descriptors
                pure (map (\candidate -> candidate {candidateInputCustody=Just custody}) <$>
                  attachExecutionSources exactGraphs graphs references producer candidates)
        | otherwise -> pure (Left "candidate manifest has trailing bytes"))
    :: IO (Either IOException (Either String [ModuleCandidate]))
  pure $ case result of
    Left failure -> Left (show failure)
    Right decoded -> decoded

candidateParts :: ModuleCandidate -> [(OriginalInputKind,FilePath,String,Int)]
candidateParts candidate =
  [(InputInterface,candidateInterface candidate,candidateInterfaceSha256 candidate,32*1024*1024)
  ,(InputPackages,candidatePackageImports candidate,candidatePackageImportsSha256 candidate,4*1024*1024)
  ,(InputNative,candidateProductPath candidate,candidateProductSha256 candidate,64*1024*1024)
  ,(InputCertificate,candidateCertificatePath proof,candidateCertificateSha256 proof,4*1024*1024)
  ,(InputCore,fst core,snd core,32*1024*1024)]
  where proof = candidateModuleInterface candidate; core = candidateCoreDescriptor proof

validateCandidateImages :: String -> [ModuleCandidate] -> [(String,FilePath)] -> [ExecutionSourceRef]
  -> [OriginalInputImage] -> Either String ()
validateCandidateImages producer candidates graphs references images = do
  let graphPaths = Map.fromList graphs
      expected candidate = Set.fromList
        ([(kind,path,sha) | (kind,path,sha,_) <- candidateParts candidate] ++
          [(InputGraph,path,executionRefGraph reference) | reference <- references
          , executionIdentityKey (executionRefIdentity reference) == (candidateUnit candidate,candidateModule candidate)
          , Just path <- [Map.lookup (executionRefGraph reference) graphPaths]])
      owners = Map.fromList [((candidateUnit candidate,candidateModule candidate),expected candidate) | candidate <- candidates]
      supplied = Map.fromList [(key,Set.fromList [(kind,originalInputPath reference,originalInputSha256 reference)
        | (kind,reference) <- parts]) | OriginalInputImage _ key _ parts <- images]
  unless (owners == supplied && all (\(OriginalInputImage issuer _ _ _) -> issuer == producer) images)
    (Left "candidate owned inputs differ from receiving owners and roles")

decodeManifest :: Decoder s ([ModuleCandidate], [(String, FilePath)], [ExecutionSourceRef], String,InputAcquisition)
decodeManifest = do
  count <- decodeListLen
  magic <- decodeString
  unless (magic == "TPMCAN") (fail "candidate manifest has wrong magic")
  version <- decodeString
  unless (version == "11" && count == 8)
    (fail "unsupported candidate manifest version or framing")
  symbols <- decodeTable $ do
    identity <- decodeIdentity
    pure (identity, identitySize identity)
  globals <- decodeTable $ do
    global <- decodeGlobal symbols
    pure (global, globalSize global)
  total <- decodeListLen
  when (total > maxCandidates) (fail "too many module candidates")
  (candidates,_) <- decodeCandidates total symbols globals maxManifestBytes
  let owners = Set.fromList [(candidateUnit c, candidateModule c) | c <- candidates]
  unless (Set.size owners == length candidates) (fail "duplicate module candidate")
  parcelCount <- decodeListLen
  unless (parcelCount == 2) (fail "invalid candidate execution parcel")
  descriptors <- decodeExecutionSourceDescriptors
  references <- decodeExecutionSourceReferences
  producer <- T.unpack <$> decodeString
  unless (canonicalDigest producer) (fail "invalid candidate compiler producer")
  acquisition <- decodeInputAcquisition
  pure (candidates, descriptors, references, producer,acquisition)

attachExecutionSources :: [ExecutionSourceGraph] -> [ExecutionSourceGraph]
  -> [ExecutionSourceRef] -> String -> [ModuleCandidate] -> Either String [ModuleCandidate]
attachExecutionSources exactGraphs graphs references producer candidates = do
  let offered = Map.fromList [(candidateOriginalIdentity candidate,candidate) | candidate <- candidates]
      available = Map.fromList [(executionGraphSha256 graph,graph) | graph <- exactGraphs ++ graphs]
      byOwner = Map.fromList [(executionIdentityKey (executionRefIdentity reference),reference)
        | reference <- references]
  unless (executionSourceGraphsFit (Map.elems available))
    (Left "combined candidate execution graphs exceed bound")
  unless (all ((== producer) . executionGraphProducer) (Map.elems available))
    (Left "invalid or conflicting candidate compiler producer")
  forM_ references $ \reference -> do
    unless (Map.member (executionRefIdentity reference) offered)
      (Left "candidate execution reference differs from offered original")
    case Map.lookup (executionRefGraph reference) available of
      Just graph | any ((== executionRefIdentity reference) . executionOwnerIdentity)
          (executionGraphOwners graph) -> pure ()
      _ -> Left "candidate execution reference lacks its original graph owner"
    either (Left . show) (const (pure ()))
      (executionSourceOriginalClosure (Map.elems available) [reference])
  pure [candidate {candidateExecutionSource = CandidateExecutionSource graphs <$>
      Map.lookup (candidateUnit candidate,candidateModule candidate) byOwner
      , candidateProducerSha256 = producer}
    | candidate <- candidates]

type InventoryTable a = IntMap.IntMap (a, Integer)

decodeTable :: Decoder s (a, Integer) -> Decoder s (InventoryTable a)
decodeTable parse = do
  total <- decodeListLen
  when (total > 65536) (fail "candidate inventory table exceeds bound")
  let go index table
        | index == total = pure table
        | otherwise = do
            value <- parse
            go (index + 1) (IntMap.insert index value table)
  go 0 IntMap.empty

decodeCandidates :: Int -> InventoryTable SymbolIdentity -> InventoryTable CandidateGlobal
  -> Integer -> Decoder s ([ModuleCandidate], Integer)
decodeCandidates total symbols globals = go total []
  where
    go 0 values budget = pure (reverse values,budget)
    go remaining values budget = do
      (candidate,next) <- decodeCandidate symbols globals budget
      go (remaining - 1) (candidate:values) next

decodeCandidate :: InventoryTable SymbolIdentity -> InventoryTable CandidateGlobal
  -> Integer -> Decoder s (ModuleCandidate, Integer)
decodeCandidate symbols globals budget = do
  count <- decodeListLen
  unless (count == 16) (fail "module candidate must have sixteen fields")
  let text = T.unpack <$> decodeString
  unit <- text
  name <- text
  source <- text
  sourceSha <- text
  interface <- text
  interfaceSha <- text
  version <- text
  productSha <- text
  evidenceSha <- text
  imports <- decodeImports
  (groups,next) <- decodeGroups symbols globals budget
  packages <- text
  packageSha <- text
  productPath <- text
  requirements <- boundedList maxCandidates $ do
    fields <- decodeListLen
    unless (fields == 2) (fail "candidate interface requirement must have two fields")
    (,) <$> text <*> text
  unless (and (zipWith (<) requirements (drop 1 requirements))
      && all (\(requiredUnit,requiredModule) -> not (null requiredUnit) && not (null requiredModule)) requirements
      && (unit,name) `notElem` requirements)
    (fail "invalid candidate canonical requirements")
  descriptorFields <- decodeListLen
  role <- text
  unless (descriptorFields == 5 && role == "module")
    (fail "candidate requires canonical module evidence")
  certificatePath <- text
  certificateSha <- text
  corePath <- text
  coreSha <- text
  unless (isAbsolute certificatePath && canonicalDigest certificateSha
      && isAbsolute corePath && canonicalDigest coreSha)
    (fail "invalid candidate canonical descriptor")
  let descriptor = CandidateModuleInterface certificatePath certificateSha (corePath,coreSha)
      candidate = ModuleCandidate Nothing unit name source sourceSha interface interfaceSha
        version productSha evidenceSha imports groups packages packageSha productPath Nothing
        "" requirements descriptor
  unless (not (null (candidateUnit candidate))
      && not (null (candidateModule candidate))
      && isAbsolute (candidateSource candidate)
      && isAbsolute (candidateInterface candidate)
      && isDigest (candidateSourceSha256 candidate)
      && isDigest (candidateInterfaceSha256 candidate)
      && isDigest (candidateModuleVersion candidate)
      && isDigest (candidateProductSha256 candidate)
      && isDigest (candidateEvidenceSha256 candidate)
      && isAbsolute (candidatePackageImports candidate)
      && isDigest (candidatePackageImportsSha256 candidate)
      && isAbsolute (candidateProductPath candidate))
    (fail "invalid module candidate identity or digest")
  pure (candidate,next)

decodeGroups :: InventoryTable SymbolIdentity -> InventoryTable CandidateGlobal
  -> Integer -> Decoder s ([CandidateGroup], Integer)
decodeGroups symbols globals budget = do
  total <- decodeListLen
  when (total > 65536) (fail "too many original groups")
  start <- chargeExpanded budget (uintSize (toInteger total))
  let go 0 groups remaining = pure (reverse groups,remaining)
      go count groups remaining = do
        fields <- decodeListLen
        unless (fields == 3) (fail "original group must have three fields")
        ordinal <- decodeWord
        afterHeader <- chargeExpanded remaining (1 + uintSize (toInteger ordinal))
        (binders,afterBinders) <- decodeReferences symbols afterHeader
        (originalGlobals,afterGlobals) <- decodeReferences globals afterBinders
        go (count - 1) (CandidateGroup ordinal binders originalGlobals:groups) afterGlobals
  go total [] start

-- Charge full legacy values before allocating each resolved list cell. The
-- IntMap entries share immutable identities and signatures across candidates.
decodeReferences :: InventoryTable a -> Integer -> Decoder s ([a], Integer)
decodeReferences table budget = do
  total <- decodeListLen
  when (total > 65536) (fail "candidate inventory exceeds bound")
  start <- chargeExpanded budget (uintSize (toInteger total))
  let go 0 values remaining = pure (reverse values,remaining)
      go count values remaining = do
        (value,bytes) <- decodeReference table
        next <- chargeExpanded remaining bytes
        go (count - 1) (value:values) next
  go total [] start

decodeReference :: InventoryTable a -> Decoder s (a,Integer)
decodeReference table = do
  index <- decodeWord
  when (index >= 65536) (fail "candidate inventory index exceeds bound")
  maybe (fail "candidate inventory index is unavailable") pure
    (IntMap.lookup (fromIntegral index) table)

decodeGlobal :: InventoryTable SymbolIdentity -> Decoder s CandidateGlobal
decodeGlobal symbols = do
  count <- decodeListLen
  unless (count == 5) (fail "global inventory must have five fields")
  (identity,_) <- decodeReference symbols
  CandidateGlobal identity <$> decodeRep <*> nullable decodeSignature
    <*> decodeBool <*> nullable decodeWord

chargeExpanded :: Integer -> Integer -> Decoder s Integer
chargeExpanded remaining bytes
  | bytes < 0 || bytes > remaining = fail "expanded candidate inventory exceeds four MiB"
  | otherwise = pure (remaining - bytes)

-- Canonical CBOR size of the old, fully expanded group inventory. Counts are
-- Integer so repeated references cannot wrap the aggregate admission bound.
uintSize :: Integer -> Integer
uintSize value
  | value <= 23 = 1
  | value <= 255 = 2
  | value <= 65535 = 3
  | value <= 4294967295 = 5
  | otherwise = 9

textSize :: T.Text -> Integer
textSize value = let bytes = toInteger (BS.length (TE.encodeUtf8 value))
  in uintSize bytes + bytes

identitySize :: SymbolIdentity -> Integer
identitySize identity = 1 + sum (map textSize
  [symbolUnit identity,symbolModule identity,symbolNamespace identity,symbolOccurrence identity])
  + maybe 1 textSize (symbolRecordParent identity)

repSize :: RuntimeRep -> Integer
repSize rep =
  let (tag,bits) = case rep of
        VoidRep -> ("void",0)
        LiftedRefRep -> ("lifted",0)
        UnliftedRefRep -> ("unlifted",0)
        AddressRep -> ("address",0)
        IntRep width -> ("int",toInteger width)
        WordRep width -> ("word",toInteger width)
        FloatRep width -> ("float",toInteger width)
  in 1 + textSize (T.pack tag) + uintSize bits

signatureSize :: Signature -> Integer
signatureSize signature = let
    arguments = signatureArguments signature
    (tag,results) = case signatureResults signature of
      Returns values -> ("returns",values)
      NoSuccess -> ("no_success",[])
      CallerResult -> ("caller_result",[])
    repsSize values = uintSize (toInteger (length values)) + sum (map repSize values)
  in 1 + repsSize arguments + 1 + textSize (T.pack tag) + repsSize results

globalSize :: CandidateGlobal -> Integer
globalSize global = 1 + identitySize (candidateGlobalIdentity global)
  + repSize (candidateGlobalRep global)
  + maybe 1 signatureSize (candidateGlobalSignature global)
  + 1 + maybe 1 (uintSize . toInteger) (candidateGlobalGeneration global)

decodeIdentity :: Decoder s SymbolIdentity
decodeIdentity = do
  count <- decodeListLen
  unless (count == 5) (fail "symbol identity must have five fields")
  SymbolIdentity <$> decodeString <*> decodeString <*> decodeString
    <*> decodeString <*> nullable decodeString

decodeRep :: Decoder s RuntimeRep
decodeRep = do
  count <- decodeListLen
  unless (count == 2) (fail "representation must have two fields")
  tag <- decodeString
  bits <- decodeWord
  case (tag, bits) of
    ("void", 0) -> pure VoidRep
    ("lifted", 0) -> pure LiftedRefRep
    ("unlifted", 0) -> pure UnliftedRefRep
    ("address", 0) -> pure AddressRep
    ("int", width) | width <= 255 -> pure (IntRep (fromIntegral width))
    ("word", width) | width <= 255 -> pure (WordRep (fromIntegral width))
    ("float", width) | width <= 255 -> pure (FloatRep (fromIntegral width))
    _ -> fail "invalid global representation"

decodeSignature :: Decoder s Signature
decodeSignature = do
  count <- decodeListLen
  unless (count == 2) (fail "signature must have two fields")
  arguments <- boundedList 256 decodeRep
  resultCount <- decodeListLen
  unless (resultCount == 2) (fail "result contract must have two fields")
  tag <- decodeString
  returned <- boundedList 256 decodeRep
  result <- case tag of
    "returns" -> pure (Returns returned)
    "no_success" | null returned -> pure NoSuccess
    "caller_result" | null returned -> pure CallerResult
    _ -> fail "invalid result contract"
  pure (Signature arguments result)

nullable :: Decoder s a -> Decoder s (Maybe a)
nullable parse = do
  kind <- peekTokenType
  if kind == TypeNull then decodeNull *> pure Nothing else Just <$> parse

boundedList :: Int -> Decoder s a -> Decoder s [a]
boundedList limit parse = do
  total <- decodeListLen
  when (total > limit) (fail "candidate inventory exceeds bound")
  replicateM total parse

decodeImports :: Decoder s [CandidateImport]
decodeImports = do
  total <- decodeListLen
  when (total > 512) (fail "too many original imports")
  replicateM total $ do
    count <- decodeListLen
    unless (count == 4) (fail "original import must have four fields")
    qualifierText <- T.unpack <$> decodeString
    qualifier <- case qualifierText of
      "none" -> pure CandidateUnqualified
      _ | Just unit <- stripPrefix "this:" qualifierText, not (null unit) ->
            pure (CandidateThisUnit unit)
        | Just unit <- stripPrefix "other:" qualifierText, not (null unit) ->
            pure (CandidateOtherUnit unit)
        | otherwise -> fail "invalid original import qualifier"
    name <- T.unpack <$> decodeString
    boot <- decodeBool
    selectedText <- T.unpack <$> decodeString
    unless (not (null name) && (null selectedText || isAbsolute selectedText))
      (fail "invalid original import")
    pure CandidateImport
      { candidateImportQualifier = qualifier
      , candidateImportModule = name
      , candidateImportBoot = boot
      , candidateImportSelected = if null selectedText then Nothing else Just selectedText
      }

isDigest :: String -> Bool
isDigest bytes = length bytes == 64 && all isHexDigit bytes

canonicalDigest :: String -> Bool
canonicalDigest value = length value == 64 && value /= replicate 64 '0'
  && all (`elem` ("0123456789abcdef" :: String)) value
