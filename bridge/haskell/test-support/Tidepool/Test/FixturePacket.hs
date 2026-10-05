-- Shared transport for the existing Rust fixture owners. Codec requests have
-- structural data only; genuine capture and grant policy stay in their owners.
module Tidepool.Test.FixturePacket
  ( PacketProducer(..), newPacketDirectory, runPacketProducer
  , issueCodecFixturePacket ) where

import Control.Exception (SomeException, bracket, bracketOnError, finally, try)
import Control.Monad (unless, void)
import Data.ByteString qualified as BS
import System.Directory (createDirectory, removeFile, removePathForcibly)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (readProcessWithExitCode)

newPacketDirectory :: FilePath -> String -> IO FilePath
newPacketDirectory work name = bracketOnError (openTempFile work name)
  (\(packet, handle) -> void (try (hClose handle `finally` removePathForcibly packet)
    :: IO (Either SomeException ()))) $ \(packet, handle) -> do
    hClose handle
    removeFile packet
    createDirectory packet
    pure packet

-- Codec requests carry structural test data only. This transport selects the
-- dedicated codec entry and never binds a compiler or requests capture/grants.
issueCodecFixturePacket :: FilePath -> BS.ByteString -> IO FilePath
issueCodecFixturePacket work request = do
  unless (BS.length request <= 4 * 1024 * 1024) (fail "codec fixture request exceeds metadata bound")
  packet <- newPacketDirectory work "codec-fixture"
  BS.writeFile (packet </> "request.cbor") request
  runPacketProducer CodecProducer packet
  pure packet

data PacketProducer = OriginalProductsProducer | AuthoredDeclarationProducer | CodecProducer

packetProducerTest :: PacketProducer -> String
packetProducerTest producer = "module_candidates::fixture_packets::" ++ case producer of
  OriginalProductsProducer -> "source_boot_candidate_packet_producer"
  AuthoredDeclarationProducer -> "source_boot_authored_declaration_packet_producer"
  CodecProducer -> "codec::source_boot_codec_packet_producer"

runPacketProducer :: PacketProducer -> FilePath -> IO ()
runPacketProducer producer packet = do
  issuer <- lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER" >>= maybe
    (fail "fixture packet requires the declared Rust libtest adapter") pure
  bracket (lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")
    (maybe (unsetEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET") (setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET")) $ \_ -> do
      setEnv "TIDEPOOL_CANDIDATE_FIXTURE_PACKET" packet
      (status, output, errors) <- readProcessWithExitCode issuer
        ["--exact", packetProducerTest producer, "--ignored", "--nocapture"] ""
      unless (status == ExitSuccess) $
        fail ("Rust fixture packet failed at " ++ packetProducerTest producer ++ "\n"
          ++ take 65536 output ++ take 65536 errors)
      putStr output
