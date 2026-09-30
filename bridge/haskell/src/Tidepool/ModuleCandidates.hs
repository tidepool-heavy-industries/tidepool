-- | Optional, bounded cache suggestions. The compiler checks each candidate
-- against its current downsweep before a source module may be skipped.
module Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateImport(..), CandidateQualifier(..)
  , CandidateGroup(..), CandidateGlobal(..)
  , readModuleCandidates ) where

import Codec.CBOR.Decoding
  ( Decoder, TokenType(..), decodeBool, decodeListLen, decodeNull
  , decodeString, decodeWord, peekTokenType )
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
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..), RuntimeRep(..), Signature(..), ResultContract(..) )

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
  , candidateGroups :: [CandidateGroup]
  , candidatePackageImports :: FilePath
  , candidatePackageImportsSha256 :: String
  } deriving (Eq, Show)

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
  unless (version == "5") (fail "unsupported candidate manifest version")
  total <- decodeListLen
  when (total > maxCandidates) (fail "too many module candidates")
  candidates <- replicateM total decodeCandidate
  let owners = Set.fromList [(candidateUnit c, candidateModule c) | c <- candidates]
  unless (Set.size owners == length candidates) (fail "duplicate module candidate")
  pure candidates

decodeCandidate :: Decoder s ModuleCandidate
decodeCandidate = do
  count <- decodeListLen
  unless (count == 13) (fail "module candidate must have thirteen fields")
  let text = T.unpack <$> decodeString
  candidate <- ModuleCandidate <$> text <*> text <*> text
    <*> text <*> text <*> text <*> text <*> text <*> text
    <*> decodeImports <*> decodeGroups <*> text <*> text
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
      && isDigest (candidatePackageImportsSha256 candidate))
    (fail "invalid module candidate identity or digest")
  pure candidate

decodeGroups :: Decoder s [CandidateGroup]
decodeGroups = do
  total <- decodeListLen
  when (total > 4096) (fail "too many original groups")
  replicateM total $ do
    count <- decodeListLen
    unless (count == 3) (fail "original group must have three fields")
    ordinal <- decodeWord
    binders <- boundedList 65536 decodeIdentity
    globals <- boundedList 65536 decodeGlobal
    pure (CandidateGroup ordinal binders globals)

decodeGlobal :: Decoder s CandidateGlobal
decodeGlobal = do
  count <- decodeListLen
  unless (count == 5) (fail "global inventory must have five fields")
  CandidateGlobal <$> decodeIdentity <*> decodeRep <*> nullable decodeSignature
    <*> decodeBool <*> nullable decodeWord

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
