-- Genuine compiler fixture packets keep GHC capture and Rust issuance separate.
-- The configured Rust libtest adapter consumes the production owners; this
-- module never writes durable certificates or candidate/scope descriptors.
module Tidepool.Test.GenuineCandidate
  ( CapturedCompilerFixture, FixtureCompilerInput(..)
  , captureCompilerFixture, capturedPreparedNames, capturedCertifiedProducts
  , writeGenuineCandidateManifestFor, writeGenuineMetadataScope
  , writeGenuineEmptyMetadataScope, writeGenuineCandidateNativeScope
  , writeGenuineCandidateLexicalScope
  , writeGenuineExecutionScope, writeGenuineExecutionScopeWithContext
  , writeGenuinePublishedScopeWithContext
  , writeGenuineAuthoredDeclarationScope ) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm)
import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BSL
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString)
import System.Environment (lookupEnv)
import System.FilePath ((</>))
import Tidepool.Test.FixturePacket
  ( PacketProducer(..), PacketCompletion, completedPacketOutput, newPacketDirectory, runPacketProducer )
import Tidepool.ExecutionProjection (projectOriginalHomeModuleProducts)
import Tidepool.ModuleCandidates (ModuleCandidate(..), CandidateGroup(..))
import Tidepool.GhcPipeline
  ( PreparedPipelineResult, pprPipelineResult, pprModules, pprProductInterfaces, pprAcceptedCandidates
  , prHscEnv, prTargetTcGblEnv )
import Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, prepareCompilerProjectionContext, newPreparedOriginalInterfaceArtifacts
  , writeCertifiedProductsKeeping )
import Tidepool.PreparedStg (pmModule)
import Tidepool.Session (Generation(..), SessionModule(..), SessionModuleKind(..))

-- Only the capture constructor consumes a compiler result. Later selections
-- retain these exact original bytes and cannot re-emit or restamp them.
data FixtureCompilerInput = FixtureCompilerInput
  { fixtureWorkRoot :: FilePath
  , fixtureSourcePath :: FilePath
  , fixtureIncludeRoots :: [FilePath]
  }

-- The emission result and all its paths belong to this immutable capture's
-- work root. Deliveries retain it and allocate independent request directories.
data CapturedCompilerFixture = CapturedCompilerFixture
  { capturedInput :: FixtureCompilerInput
  , capturedPacket :: FilePath
  , capturedProducer :: String
  , capturedModuleNames :: [String]
  , capturedCertifiedProducts :: CertifiedOriginalProducts
  }

data FixtureSelection = FixtureSelection
  { fixtureCandidates :: [String]
  , fixtureInterfaces :: [String]
  , fixtureNativeOwners :: [String]
  , fixtureLexicalRoots :: [String]
  }

data FixtureDelivery
  = CandidateDelivery { fixtureDeliveryRoot :: FilePath }
  | ScopeDelivery { fixtureDeliveryRoot :: FilePath }
  | WorldContextDelivery { fixtureDeliveryRoot :: FilePath }
  | PublishedContextDelivery
      { fixtureDeliveryRoot :: FilePath, fixtureCompletedTarget :: String, fixturePublicationRoots :: [String] }

data FixtureOutput
  = CandidateManifestOutput BS.ByteString
  | ScopeOutput FilePath
  | ScopeContextOutput FilePath BS.ByteString

captureCompilerFixture :: FixtureCompilerInput -> PreparedPipelineResult -> IO CapturedCompilerFixture
captureCompilerFixture input prepared = do
  producer <- requiredProducer
  packet <- newPacketDirectory (fixtureWorkRoot input) "compiler-original"
  BS.readFile (fixtureSourcePath input) >>= BS.writeFile (packet </> "original-source.hs")
  products <- capturePacket (fixtureIncludeRoots input) packet prepared
  pure CapturedCompilerFixture
    { capturedInput = input, capturedPacket = packet, capturedProducer = producer
    , capturedCertifiedProducts = products
    , capturedModuleNames = map (moduleNameString . moduleName . pmModule) (pprModules prepared) }

capturedPreparedNames :: CapturedCompilerFixture -> [String]
capturedPreparedNames = capturedModuleNames

deliverCapturedFixture :: CapturedCompilerFixture -> FixtureSelection -> FixtureDelivery -> IO FixtureOutput
deliverCapturedFixture capture selection delivery =
  writePacket delivery (Just capture) selection

writeGenuineCandidateManifestFor :: [String] -> FilePath -> CapturedCompilerFixture -> IO ()
writeGenuineCandidateManifestFor names work capture = do
  output <- deliverCapturedFixture capture (FixtureSelection names [] [] []) (CandidateDelivery work)
  case output of
    CandidateManifestOutput bytes -> BS.writeFile (work </> "module-candidates.cbor") bytes
    _ -> fail "fixture delivery omitted its requested candidate manifest"

writeGenuineMetadataScope :: FilePath -> [String] -> CapturedCompilerFixture -> IO FilePath
writeGenuineMetadataScope work names capture = scopeOutput $
  deliverCapturedFixture capture (FixtureSelection [] names [] []) (ScopeDelivery work)

writeGenuineEmptyMetadataScope :: FilePath -> IO FilePath
writeGenuineEmptyMetadataScope work = scopeOutput $
  writePacket (ScopeDelivery work) Nothing (FixtureSelection [] [] [] [])

-- Interface, native and lexical selections remain independent demands on the
-- same compiler original. The Rust production owners validate each delivery.
writeGenuineCandidateNativeScope
  :: [String] -> [String] -> FilePath -> CapturedCompilerFixture -> IO FilePath
writeGenuineCandidateNativeScope candidates nativeOwners work capture = scopeOutput $
  deliverCapturedFixture capture (FixtureSelection candidates candidates nativeOwners []) (ScopeDelivery work)

writeGenuineExecutionScope :: [String] -> [String] -> FilePath -> CapturedCompilerFixture -> IO FilePath
writeGenuineExecutionScope nativeOwners lexicalRoots work capture = scopeOutput $
  deliverCapturedFixture capture (FixtureSelection [] nativeOwners nativeOwners lexicalRoots)
    (ScopeDelivery work)

-- One completion delivers the original scope and its materialized context DATA.
-- The parcel gains no scope authority; both outputs keep their existing owners.
writeGenuineExecutionScopeWithContext :: [String] -> [String] -> FilePath -> CapturedCompilerFixture -> IO (FilePath,BS.ByteString)
writeGenuineExecutionScopeWithContext nativeOwners lexicalRoots work capture = do
  output <- deliverCapturedFixture capture
    (FixtureSelection [] nativeOwners nativeOwners lexicalRoots) (WorldContextDelivery work)
  case output of
    ScopeContextOutput path bytes -> pure (path,bytes)
    _ -> fail "fixture delivery omitted its requested scope and context parcel"

-- Explicit publication intent is consumed by the Rust completed-source issuer.
-- Ordinary fixture selection never silently endorses a source original.
writeGenuinePublishedScopeWithContext :: String -> [String] -> FilePath -> CapturedCompilerFixture -> IO (FilePath,BS.ByteString)
writeGenuinePublishedScopeWithContext target roots work capture = do
  unless (not (null target) && not (null roots) && all (not . null) roots
      && Set.size (Set.fromList roots) == length roots)
    (fail "published fixture requires one completed target and unique nonempty publication roots")
  output <- deliverCapturedFixture capture (FixtureSelection [] [] [] [])
    (PublishedContextDelivery work target roots)
  case output of
    ScopeContextOutput path bytes -> pure (path,bytes)
    _ -> fail "published fixture delivery omitted its scope and context parcel"

writeGenuineCandidateLexicalScope
  :: [String] -> [String] -> FilePath -> CapturedCompilerFixture -> IO FilePath
writeGenuineCandidateLexicalScope candidates lexicalOwners work capture = scopeOutput $
  deliverCapturedFixture capture (FixtureSelection candidates lexicalOwners [] lexicalOwners) (ScopeDelivery work)

scopeOutput :: IO FixtureOutput -> IO FilePath
scopeOutput action = action >>= \output -> case output of
  ScopeOutput path -> pure path
  _ -> fail "fixture delivery omitted its requested original scope"

-- The production request owns both graph descriptors and its manifest. Return
-- its actual path rather than relocating one part of the issued resource.
readScopeOutput :: FilePath -> PacketCompletion -> IO FixtureOutput
readScopeOutput packet completion = do
  bytes <- BSL.fromStrict <$> completedPacketOutput completion (packet </> "delivery.cbor")
  case deserialiseFromBytes decodeTerm bytes of
    Right (trailing, TList [TString "TPSOURCEBOOTDELIVERY1", TString path])
      | BSL.null trailing -> do
          _ <- completedPacketOutput completion (T.unpack path)
          pure (ScopeOutput (T.unpack path))
    _ -> fail "genuine fixture adapter did not return its original scope resource"

requiredProducer :: IO String
requiredProducer = lookupEnv "TIDEPOOL_COMPILER_PRODUCER" >>= maybe
  (fail "genuine fixture requires the matched TIDEPOOL_COMPILER_PRODUCER") pure

-- This packet invokes the existing protected authored producer once. It does
-- not relabel an ordinary finalized module as a native declaration.
writeGenuineAuthoredDeclarationScope
  :: SessionModule -> [FilePath] -> FilePath -> FilePath -> IO FilePath
writeGenuineAuthoredDeclarationScope owner includes source work = do
  let Generation generation = smGen owner
  unless (smKind owner == LibMod && generation > 0 && not (null includes))
    (fail "genuine authored fixture requires a positive reserved Lib generation and source roots")
  packet <- newPacketDirectory work "authored-origin-fixture"
  let text = encodeString . T.pack
      names values = encodeListLen (fromIntegral (length values)) <> foldMap text values
  BS.writeFile (packet </> "request.cbor") (toStrictByteString
    (encodeListLen 5 <> text "TPSOURCEBOOTAUTHORED2" <> encodeWord64 generation
      <> names includes <> text source <> text work))
  completion <- runPacketProducer AuthoredDeclarationProducer packet
  scopeOutput (readScopeOutput packet completion)

writePacket :: FixtureDelivery -> Maybe CapturedCompilerFixture -> FixtureSelection -> IO FixtureOutput
writePacket delivery input selection = do
  producer <- maybe requiredProducer (pure . capturedProducer) input
  packet <- newPacketDirectory (fixtureDeliveryRoot delivery) "candidate-delivery"
  let text = encodeString . T.pack
      names values = encodeListLen (fromIntegral (length values)) <> foldMap text values
      optional = maybe encodeNull text
      includes = maybe [] (fixtureIncludeRoots . capturedInput) input
      source = (fixtureSourcePath . capturedInput) <$> input
      capture = capturedPacket <$> input
      wantsScope = case delivery of CandidateDelivery _ -> False; _ -> True
      intent = case delivery of
        PublishedContextDelivery _ target roots ->
          encodeListLen 12 <> text "TPSOURCEBOOTPUBLISHED1"
            <> common <> text target <> names roots
        _ -> encodeListLen 10 <> text "TPSOURCEBOOTFIXTURE4" <> common
      common = optional source
        <> names includes <> names (fixtureCandidates selection) <> names (fixtureInterfaces selection)
        <> names (fixtureNativeOwners selection) <> encodeBool wantsScope
        <> text producer <> names (fixtureLexicalRoots selection) <> optional capture
  BS.writeFile (packet </> "request.cbor") (toStrictByteString
    intent)
  completion <- runPacketProducer OriginalProductsProducer packet
  case delivery of
    ScopeDelivery _ -> readScopeOutput packet completion
    CandidateDelivery _ -> CandidateManifestOutput <$> completedPacketOutput completion (packet </> "module-candidates.cbor")
    _ -> do
      scoped <- readScopeOutput packet completion
      case scoped of
        ScopeOutput path -> ScopeContextOutput path <$> completedPacketOutput completion (packet </> "world-context-blocks.cbor")
        _ -> fail "fixture delivery omitted its requested original scope"

-- Select the actual checked root; projection policy and all emission belong to
-- the same internal compiler owner used by the production worker.
capturePacket :: [FilePath] -> FilePath -> PreparedPipelineResult -> IO CertifiedOriginalProducts
capturePacket includes packet prepared = do
  let result = pprPipelineResult prepared
      environment = prHscEnv result
      owner = tcg_mod (prTargetTcGblEnv result)
  context <- prepareCompilerProjectionContext prepared Map.empty owner "__result" [] Nothing
  let products = projectOriginalHomeModuleProducts environment
        (pprProductInterfaces prepared) context
        (Set.fromList [binder | candidate <- pprAcceptedCandidates prepared
          , group <- candidateGroups candidate, binder <- candidateGroupBinders group])
        (pprModules prepared)
  originals <- newPreparedOriginalInterfaceArtifacts prepared packet
  writeCertifiedProductsKeeping includes originals packet prepared (Just products) []
