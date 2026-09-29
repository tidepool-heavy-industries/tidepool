-- | Optional, bounded cache suggestions. The compiler checks each candidate
-- against its current downsweep before a source module may be skipped.
module Tidepool.ModuleCandidates
  ( ModuleCandidate(..), readModuleCandidates ) where

import Codec.CBOR.Decoding (Decoder, decodeListLen, decodeString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Control.Exception (IOException, try)
import Control.Monad (replicateM, unless, when)
import Data.Char (isHexDigit)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import qualified Data.Set as Set
import qualified Data.Text as T
import System.Directory (getFileSize)
import System.FilePath (isAbsolute)

data ModuleCandidate = ModuleCandidate
  { candidateUnit :: String
  , candidateModule :: String
  , candidateSource :: FilePath
  , candidateSourceSha256 :: String
  , candidateInterface :: FilePath
  , candidateInterfaceSha256 :: String
  , candidateModuleVersion :: String
  , candidateProductSha256 :: String
  } deriving (Eq, Show)

maxManifestBytes :: Integer
maxManifestBytes = 1024 * 1024

maxCandidates :: Int
maxCandidates = 128

readModuleCandidates :: FilePath -> IO (Either String [ModuleCandidate])
readModuleCandidates path = do
  result <- try (do
    size <- getFileSize path
    if size > maxManifestBytes
      then pure (Left "candidate manifest exceeds one MiB")
      else do
        bytes <- BS.readFile path
        pure $ case deserialiseFromBytes decodeManifest (BL.fromStrict bytes) of
          Left failure -> Left (show failure)
          Right (remaining, candidates)
            | BL.null remaining -> Right candidates
            | otherwise -> Left "candidate manifest has trailing bytes")
    :: IO (Either IOException (Either String [ModuleCandidate]))
  pure $ case result of
    Left failure -> Left (show failure)
    Right decoded -> decoded

decodeManifest :: Decoder s [ModuleCandidate]
decodeManifest = do
  count <- decodeListLen
  unless (count == 3) (fail "candidate manifest header must have three fields")
  magic <- decodeString
  unless (magic == "TPMCAN") (fail "candidate manifest has wrong magic")
  version <- decodeString
  unless (version == "2") (fail "unsupported candidate manifest version")
  total <- decodeListLen
  when (total > maxCandidates) (fail "too many module candidates")
  candidates <- replicateM total decodeCandidate
  let owners = Set.fromList [(candidateUnit c, candidateModule c) | c <- candidates]
  unless (Set.size owners == length candidates) (fail "duplicate module candidate")
  pure candidates

decodeCandidate :: Decoder s ModuleCandidate
decodeCandidate = do
  count <- decodeListLen
  unless (count == 8) (fail "module candidate must have eight fields")
  let text = T.unpack <$> decodeString
  candidate <- ModuleCandidate <$> text <*> text <*> text
    <*> text <*> text <*> text <*> text <*> text
  unless (not (null (candidateUnit candidate))
      && not (null (candidateModule candidate))
      && isAbsolute (candidateSource candidate)
      && isAbsolute (candidateInterface candidate)
      && isDigest (candidateSourceSha256 candidate)
      && isDigest (candidateInterfaceSha256 candidate)
      && isDigest (candidateModuleVersion candidate)
      && isDigest (candidateProductSha256 candidate))
    (fail "invalid module candidate identity or digest")
  pure candidate
  where
    isDigest bytes = length bytes == 64 && all isHexDigit bytes
