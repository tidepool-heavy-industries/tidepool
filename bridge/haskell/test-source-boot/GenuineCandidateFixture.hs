-- Genuine source-boot packets keep GHC capture and Rust issuance separate.
-- The configured Rust libtest adapter consumes the production owners; this
-- module never writes durable certificates or candidate/scope descriptors.
module GenuineCandidateFixture
  ( writeGenuineCandidateManifestFor, writeGenuineMetadataScope
  , writeGenuineEmptyMetadataScope, writeGenuineCandidateNativeScope ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (bracket)
import Control.Monad (forM, unless)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Char8 qualified as BSC
import Data.Map.Strict qualified as Map
import Data.Text qualified as T
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import Numeric (showHex)
import System.Directory (createDirectory, removeFile)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>), takeDirectory)
import System.IO (hClose, openTempFile)
import System.Process (readProcessWithExitCode)
import Tidepool.CertifiedProducts (encodeCertifiedProducts)
import Tidepool.DependencyEvidence
import Tidepool.ExactHydration (ExactIfaceArtifact(..), newOriginalInterfaceArtifacts, originalInterfaceBytes)
import Tidepool.ExecutionEncode (encodeModuleProducts)
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.FinalizedModuleArtifacts (captureFinalizedModuleArtifacts)
import Tidepool.GhcPipeline (PreparedPipelineResult(..), PipelineResult(..))
import Tidepool.PackageWitness (encodePackageImports)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedJson (resolveJsonAuthority)
import Tidepool.PreparedTime (resolveTimeAuthority)

writeGenuineCandidateManifestFor
  :: [String] -> FilePath -> FilePath -> [FilePath] -> PreparedPipelineResult -> IO ()
writeGenuineCandidateManifestFor names work source includes prepared =
  writePacket work (Just (source, includes, prepared)) names [] [] Nothing

writeGenuineMetadataScope
  :: FilePath -> FilePath -> FilePath -> [FilePath] -> [String] -> PreparedPipelineResult -> IO ()
writeGenuineMetadataScope destination work source includes names prepared =
  writePacket work (Just (source, includes, prepared)) [] names [] (Just destination)

writeGenuineEmptyMetadataScope :: FilePath -> IO ()
writeGenuineEmptyMetadataScope destination =
  writePacket (takeDirectory destination) Nothing [] [] [] (Just destination)

-- Native products and candidate descriptors share one immutable finalization
-- packet. The Rust owner retains the complete canonical interface closure and
-- admits native execution only from its genuinely certified original products.
-- Candidate interface custody and native execution roots are selected separately.
writeGenuineCandidateNativeScope
  :: [String] -> [String] -> FilePath -> FilePath -> [FilePath] -> FilePath -> PreparedPipelineResult -> IO ()
writeGenuineCandidateNativeScope candidates nativeOwners work source includes destination prepared =
  writePacket work (Just (source, includes, prepared)) candidates candidates nativeOwners (Just destination)

writePacket
  :: FilePath -> Maybe (FilePath, [FilePath], PreparedPipelineResult)
  -> [String] -> [String] -> [String] -> Maybe FilePath -> IO ()
writePacket work input candidates exactOwners nativeOwners destination = do
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
    Just (_, _, prepared) -> capturePacket work packet prepared
  let text = encodeString . T.pack
      names values = encodeListLen (fromIntegral (length values)) <> foldMap text values
      optional = maybe encodeNull text
      includes = maybe [] (\(_, roots, _) -> roots) input
      source = (\(path, _, _) -> path) <$> input
  BS.writeFile (packet </> "request.cbor") (toStrictByteString
    (encodeListLen 8 <> text "TPSOURCEBOOTFIXTURE2" <> optional source
      <> names includes <> names candidates <> names exactOwners <> names nativeOwners
      <> optional destination <> text producer))
  bracket (lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")
    (maybe (unsetEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET") (setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")) $ \_ -> do
      setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET" packet
      (status, output, errors) <- readProcessWithExitCode issuer
        ["--exact", "module_candidates::fixture_packets::source_boot_candidate_packet_producer"
        , "--ignored", "--nocapture"] ""
      unless (status == ExitSuccess) $
        fail ("genuine Rust fixture issuance failed\n" ++ output ++ errors)
      putStr output

capturePacket :: FilePath -> FilePath -> PreparedPipelineResult -> IO ()
capturePacket work packet prepared = do
  let env = prHscEnv (pprPipelineResult prepared)
  formatting <- resolveFormattingAuthority env
  time <- resolveTimeAuthority env
  json <- resolveJsonAuthority env
  text <- resolveTextPackageUnit env
  let context = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        (SymbolIdentity "main" "Fixture" "value" "__result" Nothing)
        [] formatting time json text
      outcomes = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts
        env (pprProductInterfaces prepared) context (pprModules prepared))
      availability = Map.fromList
        [((unitString (moduleUnit owner), moduleNameString (moduleName owner)),
          either (const ProductProjectionRejected) (const ProductReady) outcome)
        | (owner, outcome) <- outcomes]
      evidence = (pprDependencies prepared) { dependencyModules =
        [node { dependencyModuleProduct = if dependencyModuleBoot node then ProductBoot
          else Map.findWithDefault (dependencyModuleProduct node)
            (dependencyModuleUnit node, dependencyModuleName node) availability }
        | node <- dependencyModules (pprDependencies prepared)] }
  originals <- newOriginalInterfaceArtifacts env (pprFinalizedModules prepared) [] work
  rows <- forM [(owner, groups) | (owner, Right groups) <- outcomes] $ \(owner, groups) -> do
    bytes <- originalInterfaceBytes originals owner
      >>= maybe (fail "genuine fixture lost its finalized original interface") pure
    roots <- maybe (fail "genuine fixture lost its actual package selections") pure
      (Map.lookup (moduleName owner) (pprPackageImports prepared))
    let unit = T.pack (unitString (moduleUnit owner))
        name = T.pack (moduleNameString (moduleName owner))
        -- encodeCertifiedProducts validates this package sidecar against the
        -- same original interface bytes used in the native product below.
        artifact = ExactIfaceArtifact (T.unpack unit) (T.unpack name) "" (shaHex bytes) []
    pure ((unit, name, bytes, groups), encodePackageImports artifact roots)
  let fresh = map fst rows
      products = encodeModuleProducts fresh
      dependencies = BSC.pack (renderDependencyEvidence evidence)
  finalized <- captureFinalizedModuleArtifacts originals env (pprFinalizedModules prepared)
    (pprPackageImports prepared) evidence work
  receipt <- encodeCertifiedProducts env (pprProductInterfaces prepared) finalized [] Nothing
    fresh [] evidence products dependencies >>= either fail pure
  BS.writeFile (packet </> "module-products.cbor") products
  BS.writeFile (packet </> "certified-products.cbor") receipt
  BS.writeFile (packet </> "dependencies.json") dependencies
  BS.writeFile (packet </> "module-package-imports.cbor") (toStrictByteString
    (encodeListLen 3 <> encodeString "TPPKGBUNDLES" <> encodeWord 1
      <> encodeListLen (fromIntegral (length rows))
      <> foldMap (\((unit, name, _, _), packages) -> encodeListLen 3
        <> encodeString unit <> encodeString name <> encodeBytes packages) rows))

shaHex :: BS.ByteString -> String
shaHex = concatMap (\byte -> let rendered = showHex byte ""
  in replicate (2 - length rendered) '0' ++ rendered) . BS.unpack . SHA.hash
