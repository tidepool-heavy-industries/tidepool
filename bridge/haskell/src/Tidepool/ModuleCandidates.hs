-- | Optional, bounded cache suggestions. The compiler checks each candidate
-- against its current downsweep before a source module may be skipped.
module Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateImport(..), CandidateQualifier(..)
  , readModuleCandidates ) where

import Codec.CBOR.Decoding (Decoder, decodeBool, decodeListLen, decodeString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Control.Exception (IOException, try)
import Control.Monad (replicateM, unless, when)
import Data.Char (isHexDigit)
import Data.List (stripPrefix)
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
  , candidateEvidenceSha256 :: String
  , candidateImports :: [CandidateImport]
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
  unless (version == "3") (fail "unsupported candidate manifest version")
  total <- decodeListLen
  when (total > maxCandidates) (fail "too many module candidates")
  candidates <- replicateM total decodeCandidate
  let owners = Set.fromList [(candidateUnit c, candidateModule c) | c <- candidates]
  unless (Set.size owners == length candidates) (fail "duplicate module candidate")
  pure candidates

decodeCandidate :: Decoder s ModuleCandidate
decodeCandidate = do
  count <- decodeListLen
  unless (count == 10) (fail "module candidate must have ten fields")
  let text = T.unpack <$> decodeString
  candidate <- ModuleCandidate <$> text <*> text <*> text
    <*> text <*> text <*> text <*> text <*> text <*> text
    <*> decodeImports
  unless (not (null (candidateUnit candidate))
      && not (null (candidateModule candidate))
      && isAbsolute (candidateSource candidate)
      && isAbsolute (candidateInterface candidate)
      && isDigest (candidateSourceSha256 candidate)
      && isDigest (candidateInterfaceSha256 candidate)
      && isDigest (candidateModuleVersion candidate)
      && isDigest (candidateProductSha256 candidate)
      && isDigest (candidateEvidenceSha256 candidate))
    (fail "invalid module candidate identity or digest")
  pure candidate

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
