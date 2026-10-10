{-# LANGUAGE OverloadedStrings #-}

-- Shared transport for the existing Rust fixture owners. Codec requests have
-- structural data only; genuine capture and grant policy stay in their owners.
module Tidepool.Test.FixturePacket
  ( PacketProducer(..), PacketCompletion, completedPacketOutput
  , newPacketDirectory, runPacketProducer, issueCodecFixturePacket ) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm)
import Control.Exception (SomeException, bracket, bracketOnError, finally, try)
import Control.Monad (unless, void)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BSL
import Data.Map.Strict qualified as Map
import Data.Text qualified as T
import System.Directory (canonicalizePath, createDirectory, doesPathExist, removeFile, removePathForcibly)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>), isAbsolute, isRelative, makeRelative, splitDirectories)
import System.IO (IOMode(ReadMode), hClose, openTempFile, withBinaryFile)
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
issueCodecFixturePacket :: FilePath -> FilePath -> BS.ByteString -> IO FilePath
issueCodecFixturePacket work output request = do
  unless (BS.length request <= metadataBound) (fail "codec fixture request exceeds metadata bound")
  packet <- newPacketDirectory work "codec-fixture"
  BS.writeFile (packet </> "request.cbor") request
  completion <- runPacketProducer CodecProducer packet
  let path = packet </> output
  _ <- completedPacketOutput completion path
  pure path

data PacketProducer = OriginalProductsProducer | AuthoredDeclarationProducer | CodecProducer
  deriving (Eq, Show)

producerLabel :: PacketProducer -> T.Text
producerLabel producer = case producer of
  OriginalProductsProducer -> "original-products"
  AuthoredDeclarationProducer -> "authored-declaration"
  CodecProducer -> "codec"

packetProducerTest :: PacketProducer -> String
packetProducerTest producer = "module_candidates::fixture_packets::" ++ case producer of
  OriginalProductsProducer -> "source_boot_candidate_packet_producer"
  AuthoredDeclarationProducer -> "source_boot_authored_declaration_packet_producer"
  CodecProducer -> "codec::source_boot_codec_packet_producer"

-- Only successful request-bound issuance constructs this captured result. A
-- caller consumes these exact bytes instead of reopening a shared output path.
newtype PacketCompletion = PacketCompletion (Map.Map FilePath BS.ByteString)

completedPacketOutput :: PacketCompletion -> FilePath -> IO BS.ByteString
completedPacketOutput (PacketCompletion outputs) path = maybe
  (fail "fixture completion omitted its requested output") pure (Map.lookup path outputs)

metadataBound :: Int
metadataBound = 4 * 1024 * 1024

boundedBytes :: Int -> FilePath -> IO BS.ByteString
boundedBytes limit path = do
  bytes <- withBinaryFile path ReadMode (\handle -> BS.hGet handle (limit + 1))
  unless (BS.length bytes <= limit) (fail "fixture packet exceeds its byte bound")
  pure bytes

runPacketProducer :: PacketProducer -> FilePath -> IO PacketCompletion
runPacketProducer producer packet = do
  unless (isAbsolute packet) (fail "fixture packet must be absolute")
  let completionPath = packet </> "completion.cbor"
  exists <- doesPathExist completionPath
  unless (not exists) (fail "fixture packet already completed")
  request <- boundedBytes metadataBound (packet </> "request.cbor")
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
      receipt <- boundedBytes 8192 completionPath
      rows <- case deserialiseFromBytes decodeTerm (BSL.fromStrict receipt) of
        Right (trailing, TList [TString "TPFIXTURECOMPLETE1", TString kind, TString root,
          TBytes requestSha, TList rows])
          | BSL.null trailing && kind == producerLabel producer && T.unpack root == packet
              && requestSha == SHA.hash request && not (null rows) && length rows <= 16 -> pure rows
        _ -> fail "fixture completion does not match its producer, packet and consumed request"
      canonicalPacket <- canonicalizePath packet
      declared <- mapM outputRow rows
      unless (length declared == Map.size (Map.fromList [(path, ()) | (path, _, _) <- declared])
        && sum [toInteger size | (_, size, _) <- declared] <= toInteger (16 * metadataBound))
        (fail "fixture completion has duplicate or oversized outputs")
      outputs <- mapM (captureOutput canonicalPacket) declared
      putStr output
      pure (PacketCompletion (Map.fromList outputs))
  where
    outputRow (TList [TString path, TInt size, TBytes sha])
      | size >= 0 && size <= 16 * metadataBound && BS.length sha == 32 = pure (T.unpack path, size, sha)
    outputRow _ = fail "fixture completion output row is invalid"
    captureOutput root (path, size, sha) = do
      canonical <- canonicalizePath path
      let relative = makeRelative root canonical
      unless (isAbsolute path && isRelative relative && ".." `notElem` splitDirectories relative)
        (fail "fixture completion output identity is invalid")
      bytes <- boundedBytes size path
      unless (BS.length bytes == size && SHA.hash bytes == sha)
        (fail "fixture completion output changed after publication")
      pure (path, bytes)
