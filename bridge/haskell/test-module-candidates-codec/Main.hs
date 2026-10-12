module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (bracket)
import Control.Monad (forM_, unless)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Text qualified as T
import System.Directory (getTemporaryDirectory, removePathForcibly)
import System.FilePath ((</>))
import Tidepool.ModuleCandidates (readModuleCandidates)
import Tidepool.Test.CandidateCodec (CandidateCodecCase(..), writeCandidateCodecFixture)
import Tidepool.Test.FixturePacket (newPacketDirectory)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "module-candidates-codec"
  [testCase "bounded candidate descriptor decoding" candidateManifestChecks]

candidateManifestChecks :: IO ()
candidateManifestChecks = do
  parent <- getTemporaryDirectory
  bracket (newPacketDirectory parent "candidate-codec") removePathForcibly $ \work -> do
    -- The Rust production codec issues the valid sample. Corruptions below
    -- test structural refusal without issuing capture or execution authority.
    issued <- writeCandidateCodecFixture work EmptyCandidateInventory
    readModuleCandidates issued >>= \result ->
      assert (case result of Right [_] -> True; _ -> False)
        "owner-issued candidate manifest did not decode"
    bytes <- BS.readFile issued
    fields <- case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
      Right (trailing, TList fields) | BL.null trailing -> pure fields
      _ -> fail "owner-issued candidate manifest is not a complete array"
    (version, candidate) <- case fields of
      [_, TString version, _, _, TList [candidate], _, _, _] -> pure (version, candidate)
      _ -> fail "owner-issued candidate manifest has an unexpected structure"
    current <- case reads (T.unpack version) of
      [(number, "")] -> pure (number :: Int)
      _ -> fail "owner-issued candidate version is not numeric"
    let replace index value values = take index values ++ [value] ++ drop (index + 1) values
        packet values = toStrictByteString (encodeTerm (TList values))
        refuse label corrupted = do
          let path = work </> "corrupted.cbor"
          BS.writeFile path corrupted
          readModuleCandidates path >>= \result ->
            assert (case result of Left _ -> True; _ -> False) label
        candidateRows value = packet (replace 4 (TList value) fields)
        corruptCandidate index value = case candidate of
          TList row -> candidateRows [TList (replace index value row)]
          _ -> error "owner-issued candidate row is not an array"
    forM_ ([0] ++ [6 .. current - 1] ++ [current + 1]) $ \unsupported ->
      refuse "candidate manifest accepted an unsupported version"
        (packet (replace 1 (TString (T.pack (show unsupported))) fields))
    refuse "candidate manifest accepted a relative original product path"
      (corruptCandidate 13 (TString "relative.tpmod"))
    refuse "candidate manifest accepted trailing bytes" (BS.snoc bytes 0)
    refuse "candidate manifest accepted duplicate owners" (candidateRows [candidate, candidate])
    refuse "candidate manifest accepted a relative source path"
      (corruptCandidate 2 (TString "relative.hs"))

assert :: Bool -> String -> IO ()
assert condition message = unless condition (fail message)
