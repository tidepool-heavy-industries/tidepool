-- Genuine source-boot packets keep GHC capture and Rust issuance separate.
-- The configured Rust libtest adapter consumes the production owners; this
-- module never writes durable certificates or candidate/scope descriptors.
module GenuineCandidateFixture
  ( CapturedCompilerFixture, FixtureCompilerInput(..)
  , captureCompilerFixture, capturedPreparedNames
  , writeGenuineCandidateManifestFor, writeGenuineMetadataScope
  , writeGenuineEmptyMetadataScope, writeGenuineCandidateNativeScope
  , writeGenuineCandidateLexicalScope
  , writeGenuineExecutionScope
  , writeGenuineAuthoredDeclarationScope ) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm)
import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (SomeException, bracket, bracketOnError, finally, try)
import Control.Monad (unless, void)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BSL
import Data.Map.Strict qualified as Map
import Data.Text qualified as T
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString)
import System.Directory (createDirectory, removeFile, removePathForcibly)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (readProcessWithExitCode)
import Tidepool.ExactHydration (newOriginalInterfaceArtifacts)
import Tidepool.ExecutionProjection (projectOriginalHomeModuleProducts)
import Tidepool.GhcPipeline (PreparedPipelineResult(..), PipelineResult(..))
import Tidepool.CompilerProducts
  ( prepareCompilerProjectionContext, retainedOriginalInterfaces, writeCertifiedProductsKeeping )
import Tidepool.PreparedStg (PreparedModule(..))
import Tidepool.Session (Generation(..), SessionModule(..), SessionModuleKind(..))

-- Only the capture constructor consumes a compiler result. Later selections
-- retain these exact original bytes and cannot re-emit or restamp them.
data FixtureCompilerInput = FixtureCompilerInput
  { fixtureWorkRoot :: FilePath
  , fixtureSourcePath :: FilePath
  , fixtureIncludeRoots :: [FilePath]
  }

data CapturedCompilerFixture = CapturedCompilerFixture
  { capturedInput :: FixtureCompilerInput
  , capturedPacket :: FilePath
  , capturedProducer :: String
  , capturedModuleNames :: [String]
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

data FixtureOutput
  = CandidateManifestOutput FilePath
  | ScopeOutput FilePath

captureCompilerFixture :: FixtureCompilerInput -> PreparedPipelineResult -> IO CapturedCompilerFixture
captureCompilerFixture input prepared = do
  producer <- requiredProducer
  packet <- newPacketDirectory (fixtureWorkRoot input) "compiler-original"
  BS.readFile (fixtureSourcePath input) >>= BS.writeFile (packet </> "original-source.hs")
  capturePacket (fixtureIncludeRoots input) packet prepared
  pure CapturedCompilerFixture
    { capturedInput = input, capturedPacket = packet, capturedProducer = producer
    , capturedModuleNames = map (moduleNameString . moduleName . pmModule) (pprModules prepared) }

capturedPreparedNames :: CapturedCompilerFixture -> [String]
capturedPreparedNames = capturedModuleNames

deliverCapturedFixture :: CapturedCompilerFixture -> FixtureSelection -> FixtureDelivery -> IO FixtureOutput
deliverCapturedFixture capture selection delivery =
  writePacket delivery (Just capture) selection

writeGenuineCandidateManifestFor :: [String] -> FilePath -> CapturedCompilerFixture -> IO ()
writeGenuineCandidateManifestFor names work capture =
  void (deliverCapturedFixture capture (FixtureSelection names [] [] []) (CandidateDelivery work))

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

writeGenuineCandidateLexicalScope
  :: [String] -> [String] -> FilePath -> CapturedCompilerFixture -> IO FilePath
writeGenuineCandidateLexicalScope candidates lexicalOwners work capture = scopeOutput $
  deliverCapturedFixture capture (FixtureSelection candidates lexicalOwners [] lexicalOwners) (ScopeDelivery work)

scopeOutput :: IO FixtureOutput -> IO FilePath
scopeOutput action = action >>= \output -> case output of
  ScopeOutput path -> pure path
  CandidateManifestOutput _ -> fail "fixture delivery omitted its requested original scope"

-- The production request owns both graph descriptors and its manifest. Return
-- its actual path rather than relocating one part of the issued resource.
readScopeOutput :: FilePath -> IO FixtureOutput
readScopeOutput packet = do
  bytes <- BSL.readFile (packet </> "delivery.cbor")
  case deserialiseFromBytes decodeTerm bytes of
    Right (trailing, TList [TString "TPSOURCEBOOTDELIVERY1", TString path])
      | BSL.null trailing -> pure (ScopeOutput (T.unpack path))
    _ -> fail "genuine fixture adapter did not return its original scope resource"

requiredProducer :: IO String
requiredProducer = lookupEnv "TIDEPOOL_COMPILER_PRODUCER" >>= maybe
  (fail "genuine fixture requires the matched TIDEPOOL_COMPILER_PRODUCER") pure

newPacketDirectory :: FilePath -> String -> IO FilePath
newPacketDirectory work name = bracketOnError (openTempFile work name)
  (\(packet, handle) -> void (try (hClose handle `finally` removePathForcibly packet)
    :: IO (Either SomeException ()))) $ \(packet, handle) -> do
    hClose handle
    removeFile packet
    createDirectory packet
    pure packet

-- This packet invokes the existing protected authored producer once. It does
-- not relabel an ordinary finalized module as a native declaration.
writeGenuineAuthoredDeclarationScope
  :: SessionModule -> [FilePath] -> FilePath -> FilePath -> IO FilePath
writeGenuineAuthoredDeclarationScope owner includes source work = do
  let Generation generation = smGen owner
  unless (smKind owner == LibMod && generation > 0 && not (null includes))
    (fail "genuine authored fixture requires a positive reserved Lib generation and source roots")
  issuer <- lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER" >>= maybe
    (fail "genuine authored fixture requires the matched Rust libtest issuer") pure
  packet <- newPacketDirectory work "authored-origin-fixture"
  let text = encodeString . T.pack
      names values = encodeListLen (fromIntegral (length values)) <> foldMap text values
  BS.writeFile (packet </> "request.cbor") (toStrictByteString
    (encodeListLen 5 <> text "TPSOURCEBOOTAUTHORED2" <> encodeWord64 generation
      <> names includes <> text source <> text work))
  bracket (lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")
    (maybe (unsetEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET") (setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")) $ \_ -> do
      setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET" packet
      (status, output, errors) <- readProcessWithExitCode issuer
        ["--exact", "module_candidates::fixture_packets::source_boot_authored_declaration_packet_producer"
        , "--ignored", "--nocapture"] ""
      unless (status == ExitSuccess) $
        fail ("genuine authored Rust fixture issuance failed\n" ++ take 65536 output ++ take 65536 errors)
      putStr output
  scopeOutput (readScopeOutput packet)

writePacket :: FixtureDelivery -> Maybe CapturedCompilerFixture -> FixtureSelection -> IO FixtureOutput
writePacket delivery input selection = do
  issuer <- lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER" >>= maybe
    (fail "genuine fixture requires the matched tidepool-toolchain libtest executable in TIDEPOOL_CANDIDATE_FIXTURE_ISSUER") pure
  producer <- maybe requiredProducer (pure . capturedProducer) input
  packet <- newPacketDirectory (fixtureDeliveryRoot delivery) "candidate-delivery"
  let text = encodeString . T.pack
      names values = encodeListLen (fromIntegral (length values)) <> foldMap text values
      optional = maybe encodeNull text
      includes = maybe [] (fixtureIncludeRoots . capturedInput) input
      source = (fixtureSourcePath . capturedInput) <$> input
      capture = capturedPacket <$> input
      wantsScope = case delivery of ScopeDelivery _ -> True; CandidateDelivery _ -> False
  BS.writeFile (packet </> "request.cbor") (toStrictByteString
    (encodeListLen 10 <> text "TPSOURCEBOOTFIXTURE4" <> optional source
      <> names includes <> names (fixtureCandidates selection) <> names (fixtureInterfaces selection)
      <> names (fixtureNativeOwners selection) <> encodeBool wantsScope
      <> text producer <> names (fixtureLexicalRoots selection) <> optional capture))
  bracket (lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")
    (maybe (unsetEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET") (setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")) $ \_ -> do
      setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET" packet
      (status, output, errors) <- readProcessWithExitCode issuer
        ["--exact", "module_candidates::fixture_packets::source_boot_candidate_packet_producer"
        , "--ignored", "--nocapture"] ""
      unless (status == ExitSuccess) $
        fail ("genuine Rust fixture issuance failed\n" ++ take 65536 output ++ take 65536 errors)
      putStr output
  if wantsScope then readScopeOutput packet
    else pure (CandidateManifestOutput (fixtureDeliveryRoot delivery </> "module-candidates.cbor"))

-- Select the actual checked root; projection policy and all emission belong to
-- the same internal compiler owner used by the production worker.
capturePacket :: [FilePath] -> FilePath -> PreparedPipelineResult -> IO ()
capturePacket includes packet prepared = do
  let result = pprPipelineResult prepared
      environment = prHscEnv result
      owner = tcg_mod (prTargetTcGblEnv result)
  context <- prepareCompilerProjectionContext prepared Map.empty owner "__result" [] Nothing
  let products = projectOriginalHomeModuleProducts environment
        (pprProductInterfaces prepared) context (pprModules prepared)
  originals <- newOriginalInterfaceArtifacts environment (pprFinalizedModules prepared)
    (retainedOriginalInterfaces prepared) packet
  void (writeCertifiedProductsKeeping includes originals packet prepared (Just products) [])
