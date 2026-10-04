-- Genuine source-boot packets keep GHC capture and Rust issuance separate.
-- The configured Rust libtest adapter consumes the production owners; this
-- module never writes durable certificates or candidate/scope descriptors.
module GenuineCandidateFixture
  ( writeGenuineCandidateManifestFor, writeGenuineMetadataScope
  , writeGenuineEmptyMetadataScope, writeGenuineCandidateNativeScope
  , writeGenuineCandidateLexicalScope
  , writeGenuineExecutionScope
  , writeGenuineAuthoredDeclarationScope ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (bracket)
import Control.Monad (unless, void)
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import Data.Text qualified as T
import GHC.Tc.Types (tcg_mod)
import System.Directory (createDirectory, removeFile)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>), takeDirectory)
import System.IO (hClose, openTempFile)
import System.Process (readProcessWithExitCode)
import Tidepool.ExactHydration (newOriginalInterfaceArtifacts)
import Tidepool.ExecutionProjection (projectOriginalHomeModuleProducts)
import Tidepool.GhcPipeline (PreparedPipelineResult(..), PipelineResult(..))
import Tidepool.CompilerProducts
  ( prepareCompilerProjectionContext, retainedOriginalInterfaces, writeCertifiedProductsKeeping )
import Tidepool.Session (Generation(..), SessionModule(..), SessionModuleKind(..))

writeGenuineCandidateManifestFor
  :: [String] -> FilePath -> FilePath -> [FilePath] -> PreparedPipelineResult -> IO ()
writeGenuineCandidateManifestFor names work source includes prepared =
  writePacket work (Just (source, includes, prepared)) names [] [] [] Nothing

writeGenuineMetadataScope
  :: FilePath -> FilePath -> FilePath -> [FilePath] -> [String] -> PreparedPipelineResult -> IO ()
writeGenuineMetadataScope destination work source includes names prepared =
  writePacket work (Just (source, includes, prepared)) [] names [] [] (Just destination)

writeGenuineEmptyMetadataScope :: FilePath -> IO ()
writeGenuineEmptyMetadataScope destination =
  writePacket (takeDirectory destination) Nothing [] [] [] [] (Just destination)

-- Native products and candidate descriptors share one immutable finalization
-- packet. The Rust owner retains the complete canonical interface closure and
-- admits native execution only from its genuinely certified original products.
-- Candidate interface custody and native execution roots are selected separately.
writeGenuineCandidateNativeScope
  :: [String] -> [String] -> FilePath -> FilePath -> [FilePath] -> FilePath -> PreparedPipelineResult -> IO ()
writeGenuineCandidateNativeScope candidates nativeOwners work source includes destination prepared =
  writePacket work (Just (source, includes, prepared)) candidates candidates nativeOwners [] (Just destination)

-- Compiler dependency closure and native implementation selection are separate.
-- Lexical adjacency comes from the same admitted original compilation.
writeGenuineExecutionScope
  :: [String] -> [String] -> FilePath -> FilePath -> [FilePath] -> FilePath -> PreparedPipelineResult -> IO ()
writeGenuineExecutionScope nativeOwners lexicalRoots work source includes destination prepared =
  writePacket work (Just (source, includes, prepared)) [] nativeOwners nativeOwners lexicalRoots (Just destination)

-- Source-free metadata imports retain the actual original source closure.
-- Candidate delivery and lexical selection share this one immutable capture;
-- neither requests native execution products in the delivered scope.
writeGenuineCandidateLexicalScope
  :: [String] -> [String] -> FilePath -> FilePath -> [FilePath] -> FilePath -> PreparedPipelineResult -> IO ()
writeGenuineCandidateLexicalScope candidates lexicalOwners work source includes destination prepared =
  writePacket work (Just (source, includes, prepared)) candidates lexicalOwners [] lexicalOwners (Just destination)

-- This packet invokes the existing protected authored producer once. It does
-- not relabel an ordinary finalized module as a native declaration.
writeGenuineAuthoredDeclarationScope
  :: SessionModule -> [FilePath] -> FilePath -> FilePath -> IO ()
writeGenuineAuthoredDeclarationScope owner includes source destination = do
  let Generation generation = smGen owner
      work = takeDirectory destination
  unless (smKind owner == LibMod && generation > 0 && not (null includes))
    (fail "genuine authored fixture requires a positive reserved Lib generation and source roots")
  issuer <- lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER" >>= maybe
    (fail "genuine authored fixture requires the matched Rust libtest issuer") pure
  (packet, handle) <- openTempFile work "authored-origin-fixture"
  hClose handle
  removeFile packet
  createDirectory packet
  let text = encodeString . T.pack
      names values = encodeListLen (fromIntegral (length values)) <> foldMap text values
  BS.writeFile (packet </> "request.cbor") (toStrictByteString
    (encodeListLen 5 <> text "TPSOURCEBOOTAUTHORED1" <> encodeWord64 generation
      <> names includes <> text source <> text destination))
  bracket (lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")
    (maybe (unsetEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET") (setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")) $ \_ -> do
      setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET" packet
      (status, output, errors) <- readProcessWithExitCode issuer
        ["--exact", "module_candidates::fixture_packets::source_boot_authored_declaration_packet_producer"
        , "--ignored", "--nocapture"] ""
      unless (status == ExitSuccess) $
        fail ("genuine authored Rust fixture issuance failed\n" ++ output ++ errors)
      putStr output

writePacket
  :: FilePath -> Maybe (FilePath, [FilePath], PreparedPipelineResult)
  -> [String] -> [String] -> [String] -> [String] -> Maybe FilePath -> IO ()
writePacket work input candidates exactOwners nativeOwners lexicalOwners destination = do
  issuer <- lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER" >>= maybe
    (fail "genuine fixture requires the matched tidepool-toolchain libtest executable in TIDEPOOL_CANDIDATE_FIXTURE_ISSUER") pure
  -- The adapter verifies this configured producer against its admitted endpoint.
  producer <- lookupEnv "TIDEPOOL_COMPILER_PRODUCER" >>= maybe
    (fail "genuine fixture requires the matched TIDEPOOL_COMPILER_PRODUCER") pure
  (packet, handle) <- openTempFile work "candidate-fixture"
  hClose handle
  removeFile packet
  createDirectory packet
  case input of
    Nothing -> pure ()
    Just (_, includes, prepared) -> capturePacket includes packet prepared
  let text = encodeString . T.pack
      names values = encodeListLen (fromIntegral (length values)) <> foldMap text values
      optional = maybe encodeNull text
      includes = maybe [] (\(_, roots, _) -> roots) input
      source = (\(path, _, _) -> path) <$> input
  BS.writeFile (packet </> "request.cbor") (toStrictByteString
    (encodeListLen 9 <> text "TPSOURCEBOOTFIXTURE3" <> optional source
      <> names includes <> names candidates <> names exactOwners <> names nativeOwners
      <> optional destination <> text producer <> names lexicalOwners))
  bracket (lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")
    (maybe (unsetEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET") (setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")) $ \_ -> do
      setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET" packet
      (status, output, errors) <- readProcessWithExitCode issuer
        ["--exact", "module_candidates::fixture_packets::source_boot_candidate_packet_producer"
        , "--ignored", "--nocapture"] ""
      unless (status == ExitSuccess) $
        fail ("genuine Rust fixture issuance failed\n" ++ output ++ errors)
      putStr output

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
