{-# LANGUAGE OverloadedStrings #-}

-- | Read-only projection of an already-issued TPHOMEOWNERS v5 certificate.
-- The constructor is private so callers cannot mint or alter native evidence.
module Tidepool.NativeOriginalCensus
  ( OriginalNativeCensus
  , readOriginalNativeCensus
  , nativeCensusOwner
  , nativeCensusGroups
  , nativeCensusRequirements
  , nativeCensusCanonicalCertificate
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Encoding
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (replicateM, unless, when)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Word (Word8, Word32, Word64)
import Numeric (showHex)
import System.FilePath (isAbsolute)
import Tidepool.BoundedRead (readFileAtMost)
import Tidepool.ExecutionSchema
  ( ResultContract(..), RuntimeRep(..), Signature(..), SymbolIdentity(..) )

data OriginalNativeCensus = OriginalNativeCensus
  { censusOwner :: !(String, String, String, String, String)
  , censusGroups :: ![(Word, [SymbolIdentity], [(SymbolIdentity, Bool)])]
  , censusRequirements :: !(Map.Map (String, String) String)
  , censusCanonicalCertificate :: !(Maybe String)
  } deriving (Eq, Show)

nativeCensusOwner :: OriginalNativeCensus -> (String, String, String, String, String)
nativeCensusOwner = censusOwner

nativeCensusGroups :: OriginalNativeCensus -> [(Word, [SymbolIdentity], [(SymbolIdentity, Bool)])]
nativeCensusGroups = censusGroups

nativeCensusRequirements :: OriginalNativeCensus -> Map.Map (String, String) String
nativeCensusRequirements = censusRequirements

nativeCensusCanonicalCertificate :: OriginalNativeCensus -> Maybe String
nativeCensusCanonicalCertificate = censusCanonicalCertificate

type HomeOwner = (String, String, String, String, String)
type NativeGroup = (Word, [SymbolIdentity], [NativeGlobal])
type Package = ((String, String), (FilePath, String))

data NativeGlobal = NativeGlobal
  { nativeIdentity :: !SymbolIdentity
  , nativeRep :: !RuntimeRep
  , nativeSignature :: !(Maybe Signature)
  , nativeEvaluated :: !Bool
  , nativeImportOwner :: !ImportOwner
  }

data ImportOwner
  = SourceImport String String String Word SymbolIdentity
  | RetainedImport SymbolIdentity Word64
  | PackageImport String String String SymbolIdentity
  | RetainedPackageImport String String String SymbolIdentity Word64

data ParsedCensus = ParsedCensus
  { parsedOwner :: !HomeOwner
  , parsedGroups :: ![NativeGroup]
  , parsedSources :: ![HomeOwner]
  , parsedPackages :: ![Package]
  , parsedExecutionSource :: !(Maybe String)
  , parsedRequirements :: ![((String, String), String)]
  , parsedCanonicalCertificate :: !(Maybe String)
  }

readOriginalNativeCensus :: FilePath -> String -> IO OriginalNativeCensus
readOriginalNativeCensus path expectedSha = do
  unless (isAbsolute path) (fail "native original census path must be absolute")
  unless (canonicalDigest expectedSha) (fail "invalid native original census descriptor SHA256")
  bytes <- readFileAtMost path (censusByteLimit + 1)
  when (BS.length bytes > censusByteLimit) (fail "native original census exceeds 32 MiB")
  unless (sha256 bytes == expectedSha) (fail "native original census descriptor SHA256 mismatch")
  case deserialiseFromBytes decodeCensus (BL.fromStrict bytes) of
    Left failure -> fail ("invalid native original census: " ++ show failure)
    Right (remaining, parsed) -> do
      unless (BL.null remaining && encodeCensus parsed == bytes)
        (fail "native original census must be canonical CBOR with no trailing bytes")
      pure (project parsed)

censusByteLimit :: Int
censusByteLimit = 32 * 1024 * 1024

decodeCensus :: Decoder s ParsedCensus
decodeCensus = do
  array 9
  magic <- text
  version <- decodeWord
  unless (magic == "TPHOMEOWNERS" && version == 5) (fail "unsupported TPHOMEOWNERS certificate")
  owner <- homeOwner
  groups <- vector 65536 nativeGroup
  sources <- vector 65536 homeOwner
  packages <- vector 4096 package
  executionSource <- optional nonzeroDigest
  requirements <- vector 65536 interfaceRequirement
  canonical <- optional nonzeroDigest
  validateParsed owner groups sources packages requirements
  pure (ParsedCensus owner groups sources packages executionSource requirements canonical)

nativeGroup :: Decoder s NativeGroup
nativeGroup = do
  array 3
  ordinal <- decodeU32
  binders <- vector 65536 identity
  globals <- vector 65536 nativeGlobal
  pure (ordinal, binders, globals)

nativeGlobal :: Decoder s NativeGlobal
nativeGlobal = do
  array 5
  identityValue <- identity
  rep <- runtimeRep
  signature <- optional decodeSignature
  evaluated <- decodeBool
  importOwner <- decodeImportOwner
  pure (NativeGlobal identityValue rep signature evaluated importOwner)

decodeImportOwner :: Decoder s ImportOwner
decodeImportOwner = do
  count <- decodeListLen
  tag <- text
  case (tag, count) of
    ("source", 6) -> do
      unit <- text
      moduleName <- text
      version <- optional digest
      ordinal <- decodeU32
      binder <- identity
      case version of
        Just value -> pure (SourceImport unit moduleName value ordinal binder)
        Nothing -> fail "stored home source owner lacks its module version"
    ("retained", 3) -> RetainedImport <$> identity <*> decodeWord64
    ("package", 5) -> PackageImport <$> text <*> text <*> digest <*> identity
    ("retained-package", 6) -> RetainedPackageImport <$> text <*> text <*> digest <*> identity <*> decodeWord64
    _ -> fail "invalid native import owner tag or arity"

homeOwner :: Decoder s HomeOwner
homeOwner = do
  array 5
  unit <- text
  moduleName <- text
  version <- digest
  interfaceSha <- digest
  productSha <- digest
  unless (not (null unit) && not (null moduleName)) (fail "empty native owner identity")
  pure (unit, moduleName, version, interfaceSha, productSha)

package :: Decoder s Package
package = do
  array 4
  unit <- text
  moduleName <- text
  path <- text
  sha <- digest
  unless (not (null unit) && not (null moduleName) && isAbsolute path)
    (fail "invalid native package witness")
  pure ((unit, moduleName), (path, sha))

interfaceRequirement :: Decoder s ((String, String), String)
interfaceRequirement = do
  array 3
  unit <- text
  moduleName <- text
  sha <- digest
  unless (not (null unit) && not (null moduleName) && sha /= zeroDigest)
    (fail "invalid native interface requirement")
  pure ((unit, moduleName), sha)

identity :: Decoder s SymbolIdentity
identity = do
  array 5
  SymbolIdentity <$> (T.pack <$> text) <*> (T.pack <$> text) <*> (T.pack <$> text)
    <*> (T.pack <$> text) <*> optional (T.pack <$> text)

runtimeRep :: Decoder s RuntimeRep
runtimeRep = do
  array 2
  tag <- text
  bits <- decodeWord
  case (tag, bits) of
    ("void", 0) -> pure VoidRep
    ("lifted", 0) -> pure LiftedRefRep
    ("unlifted", 0) -> pure UnliftedRefRep
    ("address", 0) -> pure AddressRep
    ("int", value) | value <= fromIntegral (maxBound :: Word8) -> pure (IntRep (fromIntegral value))
    ("word", value) | value <= fromIntegral (maxBound :: Word8) -> pure (WordRep (fromIntegral value))
    ("float", value) | value <= fromIntegral (maxBound :: Word8) -> pure (FloatRep (fromIntegral value))
    _ -> fail "invalid native runtime representation tag or width"

decodeSignature :: Decoder s Signature
decodeSignature = do
  array 2
  arguments <- vector 65536 runtimeRep
  array 2
  resultTag <- text
  results <- vector 65536 runtimeRep
  contract <- case resultTag of
    "returns" -> pure (Returns results)
    "no_success" | null results -> pure NoSuccess
    "caller_result" | null results -> pure CallerResult
    _ -> fail "invalid native signature result contract"
  pure (Signature arguments contract)

validateParsed :: HomeOwner -> [NativeGroup] -> [HomeOwner] -> [Package]
  -> [((String, String), String)] -> Decoder s ()
validateParsed owner groups sources packages requirements = do
  let ownerKey (unit, moduleName, _, _, _) = (unit, moduleName)
      ordinals = [ordinal | (ordinal, _, _) <- groups]
      binders = concat [values | (_, values, _) <- groups]
      sourceKeys = map ownerKey sources
      packageKeys = map fst packages
      requirementKeys = map fst requirements
      ownerBinders = Set.fromList binders
      sourceMap = Map.fromList [(ownerKey source, source) | source <- sources]
      packageMap = Map.fromList packages
      (ownerUnit, ownerModule, _, _, _) = owner
      isSorted values = and (zipWith (<) values (drop 1 values))
  unless (length ordinals == Set.size (Set.fromList ordinals))
    (fail "duplicate native group ordinal")
  unless (all (\(_, groupBinders, _) -> not (null groupBinders)) groups
      && all (\binder ->
      symbolUnit binder == T.pack ownerUnit && symbolModule binder == T.pack ownerModule) binders
      && length binders == Set.size ownerBinders)
    (fail "native group binders are empty, foreign, or duplicated")
  unless (isSorted sourceKeys && length sourceKeys == Set.size (Set.fromList sourceKeys))
    (fail "native source owners are not canonical and unique")
  unless (isSorted packageKeys && length packageKeys == Set.size (Set.fromList packageKeys))
    (fail "native package owners are not canonical and unique")
  unless (isSorted requirementKeys && length requirementKeys == Set.size (Set.fromList requirementKeys)
      && all (/= ownerKey owner) requirementKeys)
    (fail "native interface requirements are not canonical")
  let globals = concat [entries | (_, _, entries) <- groups]
      sourceUses = Set.fromList [ (unit, moduleName)
        | NativeGlobal{nativeImportOwner = SourceImport unit moduleName _ _ _} <- globals ]
      packageUses = Set.fromList [ (unit, moduleName)
        | NativeGlobal{nativeImportOwner = PackageImport unit moduleName _ _} <- globals ]
        `Set.union` Set.fromList [ (unit, moduleName)
        | NativeGlobal{nativeImportOwner = RetainedPackageImport unit moduleName _ _ _} <- globals ]
  unless (sourceUses == Map.keysSet sourceMap && packageUses == Map.keysSet packageMap)
    (fail "unused or missing native source/package owner witness")
  mapM_ (validateGlobal sourceMap packageMap) globals

validateGlobal :: Map.Map (String, String) HomeOwner
  -> Map.Map (String, String) (FilePath, String) -> NativeGlobal -> Decoder s ()
validateGlobal sources packages global = case nativeImportOwner global of
  SourceImport unit moduleName version _ binder ->
    case Map.lookup (unit, moduleName) sources of
      Just (_, _, sourceVersion, _, _) -> unless
        (version == sourceVersion && binder == nativeIdentity global
          && symbolUnit binder == T.pack unit && symbolModule binder == T.pack moduleName)
        (fail "native source global disagrees with its owner")
      Nothing -> fail "native source global lacks its owner declaration"
  PackageImport unit moduleName sha binder -> validatePackage unit moduleName sha binder
  RetainedPackageImport unit moduleName sha binder _ -> validatePackage unit moduleName sha binder
  RetainedImport identityValue _ -> unless (identityValue == nativeIdentity global)
    (fail "native retained global identity mismatch")
  where
    validatePackage unit moduleName sha binder = case Map.lookup (unit, moduleName) packages of
      Just (_, packageSha) -> unless (sha == packageSha && binder == nativeIdentity global
          && symbolUnit binder == T.pack unit && symbolModule binder == T.pack moduleName)
        (fail "native package global disagrees with its owner")
      Nothing -> fail "native package global lacks its owner declaration"

project :: ParsedCensus -> OriginalNativeCensus
project parsed = OriginalNativeCensus
  { censusOwner = parsedOwner parsed
  , censusGroups =
      [ (ordinal, binders,
          [(nativeIdentity global, requiresDefinition global) | global <- globals])
      | (ordinal, binders, globals) <- parsedGroups parsed
      ]
  , censusRequirements = Map.fromList (parsedRequirements parsed)
  , censusCanonicalCertificate = parsedCanonicalCertificate parsed
  }

requiresDefinition :: NativeGlobal -> Bool
requiresDefinition global = case nativeImportOwner global of
  SourceImport{} -> True
  PackageImport{} -> True
  RetainedImport{} -> False
  RetainedPackageImport{} -> False

encodeCensus :: ParsedCensus -> BS.ByteString
encodeCensus parsed = toStrictByteString $ encodeListLen 9
  <> encodeString "TPHOMEOWNERS" <> encodeWord 5
  <> encodeHome (parsedOwner parsed)
  <> encodeList (map encodeGroup (parsedGroups parsed))
  <> encodeList (map encodeHome (parsedSources parsed))
  <> encodeList (map encodePackage (parsedPackages parsed))
  <> encodeMaybe encodeText (parsedExecutionSource parsed)
  <> encodeList (map encodeRequirement (parsedRequirements parsed))
  <> encodeMaybe encodeText (parsedCanonicalCertificate parsed)
  where
    encodeGroup (ordinal, binders, globals) = encodeListLen 3
      <> encodeWord (fromIntegral ordinal)
      <> encodeList (map encodeIdentity binders)
      <> encodeList (map encodeGlobal globals)

encodeGlobal :: NativeGlobal -> Encoding
encodeGlobal global = encodeListLen 5 <> encodeIdentity (nativeIdentity global)
  <> encodeRep (nativeRep global) <> maybe encodeNull encodeSignature (nativeSignature global)
  <> encodeBool (nativeEvaluated global) <> encodeImport (nativeImportOwner global)

encodeImport :: ImportOwner -> Encoding
encodeImport owner = case owner of
  SourceImport unit moduleName version ordinal binder -> encodeListLen 6 <> encodeString "source"
    <> encodeText unit <> encodeText moduleName <> encodeText version
    <> encodeWord (fromIntegral ordinal) <> encodeIdentity binder
  RetainedImport binder generation -> encodeListLen 3 <> encodeString "retained"
    <> encodeIdentity binder <> encodeWord64 generation
  PackageImport unit moduleName sha binder -> encodeListLen 5 <> encodeString "package"
    <> encodeText unit <> encodeText moduleName <> encodeText sha <> encodeIdentity binder
  RetainedPackageImport unit moduleName sha binder generation -> encodeListLen 6
    <> encodeString "retained-package" <> encodeText unit <> encodeText moduleName
    <> encodeText sha <> encodeIdentity binder <> encodeWord64 generation

encodeHome :: HomeOwner -> Encoding
encodeHome (unit, moduleName, version, interfaceSha, productSha) = encodeListLen 5
  <> encodeText unit <> encodeText moduleName <> encodeText version
  <> encodeText interfaceSha <> encodeText productSha

encodePackage :: Package -> Encoding
encodePackage ((unit, moduleName), (path, sha)) = encodeListLen 4
  <> encodeText unit <> encodeText moduleName <> encodeText path <> encodeText sha

encodeRequirement :: ((String, String), String) -> Encoding
encodeRequirement ((unit, moduleName), sha) = encodeListLen 3
  <> encodeText unit <> encodeText moduleName <> encodeText sha

encodeIdentity :: SymbolIdentity -> Encoding
encodeIdentity identityValue = encodeListLen 5
  <> encodeString (symbolUnit identityValue) <> encodeString (symbolModule identityValue)
  <> encodeString (symbolNamespace identityValue) <> encodeString (symbolOccurrence identityValue)
  <> maybe encodeNull encodeString (symbolRecordParent identityValue)

encodeRep :: RuntimeRep -> Encoding
encodeRep rep = encodeListLen 2 <> encodeString tag <> encodeWord bits
  where
    (tag, bits) = case rep of
      VoidRep -> ("void", 0)
      LiftedRefRep -> ("lifted", 0)
      UnliftedRefRep -> ("unlifted", 0)
      AddressRep -> ("address", 0)
      IntRep width -> ("int", fromIntegral width)
      WordRep width -> ("word", fromIntegral width)
      FloatRep width -> ("float", fromIntegral width)

encodeSignature :: Signature -> Encoding
encodeSignature signature = encodeListLen 2
  <> encodeList (map encodeRep (signatureArguments signature))
  <> encodeListLen 2 <> encodeString tag <> encodeList (map encodeRep results)
  where
    (tag, results) = case signatureResults signature of
      Returns reps -> ("returns", reps)
      NoSuccess -> ("no_success", [])
      CallerResult -> ("caller_result", [])

encodeList :: [Encoding] -> Encoding
encodeList values = encodeListLen (fromIntegral (length values)) <> mconcat values

encodeMaybe :: (a -> Encoding) -> Maybe a -> Encoding
encodeMaybe _ Nothing = encodeNull
encodeMaybe render (Just value) = render value

array :: Int -> Decoder s ()
array expected = decodeListLen >>= \actual -> unless (actual == expected) (fail "invalid native census row arity")

text :: Decoder s String
text = T.unpack <$> decodeString

vector :: Int -> Decoder s a -> Decoder s [a]
vector limit item = do
  count <- decodeListLen
  when (count > limit) (fail "native census row count exceeds bound")
  replicateM count item

decodeU32 :: Decoder s Word
decodeU32 = do
  value <- decodeWord
  when (value > fromIntegral (maxBound :: Word32)) (fail "native ordinal exceeds u32")
  pure value

optional :: Decoder s a -> Decoder s (Maybe a)
optional item = peekTokenType >>= \case
  TypeNull -> decodeNull >> pure Nothing
  _ -> Just <$> item

digest :: Decoder s String
digest = text >>= \value -> if canonicalDigest value
  then pure value else fail "invalid native census SHA256"

nonzeroDigest :: Decoder s String
nonzeroDigest = digest >>= \value -> if value /= zeroDigest
  then pure value else fail "empty native census SHA256"

canonicalDigest :: String -> Bool
canonicalDigest value = length value == 64 && all valid value
  where valid char = (char >= '0' && char <= '9') || (char >= 'a' && char <= 'f')

zeroDigest :: String
zeroDigest = replicate 64 '0'

sha256 :: BS.ByteString -> String
sha256 = concatMap byteHex . BS.unpack . SHA.hash
  where byteHex byte = let digits = showHex byte ""
                       in replicate (2 - length digits) '0' ++ digits

encodeText :: String -> Encoding
encodeText = encodeString . T.pack
