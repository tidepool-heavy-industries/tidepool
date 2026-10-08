-- Structural fixtures and observations come from the production Rust codecs.
-- This adapter has no compiler capture or execution-admission operation.
module CodecFixtureSupport
  ( CandidateCodecCase(..), writeCandidateCodecFixture
  , PurposeCodecCase(..), readPurposeCodecFixture, readRequestTypesCodecFixture, readExpressionItemCodecFixture
  , ReceiptCodecFacts(..), readReceiptCodecFacts
  , CodecImportOwner(..), CertificateCodecFacts(..), readCertificateCodecFacts, readSegmentItemCodecFacts
  , CompilerInputCodecFacts(..), readCompilerInputCodecFacts
  , readCodecTerm
  ) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BSL
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Word (Word32, Word64)
import Tidepool.Test.CandidateCodec (CandidateCodecCase(..), writeCandidateCodecFixture)
import Tidepool.Test.FixturePacket (issueCodecFixturePacket)
import System.FilePath ((</>))
import System.IO (IOMode(ReadMode), withBinaryFile)
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.CheckedCell (RequestHelperRecipe(..))

data PurposeCodecCase
  = CodecCellPurpose
  | CodecItemPurpose
  | CodecInspectionPurpose

readPurposeCodecFixture :: FilePath -> [FilePath] -> PurposeCodecCase -> IO Term
readPurposeCodecFixture work includes purpose = do
  let (name, signature) = case purpose of
        CodecCellPurpose -> ("cell", TNull)
        CodecItemPurpose -> ("item", TNull)
        CodecInspectionPurpose -> ("inspection", TNull)
  packet <- codecRequest work "purpose" [text name,TList (map text includes),signature]
  readCodecTerm (packet </> "purpose.cbor")

readExpressionItemCodecFixture :: FilePath -> [FilePath] -> BS.ByteString -> IO Term
readExpressionItemCodecFixture work includes expression = do
  packet <- codecRequest work "expression_purpose" [TList (map text includes),TBytes expression]
  readCodecTerm (packet </> "purpose.cbor")

readRequestTypesCodecFixture
  :: FilePath -> BS.ByteString -> RequestHelperRecipe -> Maybe BS.ByteString -> IO Term
readRequestTypesCodecFixture work signatures recipe inner = do
  let name = case recipe of NoRequestHelpers -> "none"; ActorReplyHelpers -> "actor-reply"
  packet <- codecRequest work "request_types"
    [TBytes signatures,text name,maybe TNull TBytes inner]
  readCodecTerm (packet </> "purpose.cbor")

data ReceiptCodecFacts = ReceiptCodecFacts
  { codecReceiptSource :: FilePath
  , codecReceiptCacheSafe :: Bool
  , codecReceiptSourceSelected :: [(String,String)]
  } deriving (Eq,Show)

readReceiptCodecFacts :: FilePath -> FilePath -> IO ReceiptCodecFacts
readReceiptCodecFacts work path = readFacts work "receipt_facts" [text path] $ \term -> do
  fields <- closedMap ["source_path","cache_safe","source_selected"] term
  ReceiptCodecFacts <$> field fields "source_path" string
    <*> field fields "cache_safe" boolean
    <*> field fields "source_selected" (array owner)

data CodecImportOwner
  = CodecSourceOwner String String (Maybe String) Word32 SymbolIdentity
  | CodecRetainedOwner SymbolIdentity Word64
  | CodecPackageOwner String String String SymbolIdentity
  | CodecRetainedPackageOwner String String String SymbolIdentity Word64
  deriving (Eq,Show)

data CertificateCodecFacts = CertificateCodecFacts
  { codecCertificateOwners :: [CodecImportOwner]
  , codecCertificatePackages :: [(String,String,FilePath,String)]
  , codecCertificateModules :: [(String,String,[Word32])]
  , codecCertificateTargets :: [(String,[Int])]
  , codecCertificateGlobalSeals :: [String]
  } deriving (Eq,Show)

readCertificateCodecFacts :: FilePath -> FilePath -> IO CertificateCodecFacts
readCertificateCodecFacts = readProductCodecFacts "certificate_facts"

readSegmentItemCodecFacts :: FilePath -> FilePath -> IO CertificateCodecFacts
readSegmentItemCodecFacts = readProductCodecFacts "segment_item_facts"

readProductCodecFacts :: String -> FilePath -> FilePath -> IO CertificateCodecFacts
readProductCodecFacts operation work path = readFacts work operation [text path] $ \term -> do
  fields <- closedMap ["owners","packages","modules","targets","global_sha256"] term
  CertificateCodecFacts <$> field fields "owners" (array importOwner)
    <*> field fields "packages" (array packageFact)
    <*> field fields "modules" (array moduleFact)
    <*> field fields "targets" (array targetFact)
    <*> field fields "global_sha256" (array digestString)
  where
    packageFact term = do
      fields <- closedMap ["unit","module","interface_path","interface_sha256"] term
      (,,,) <$> field fields "unit" string <*> field fields "module" string
        <*> field fields "interface_path" string <*> field fields "interface_sha256" digestString
    targetFact term = do
      fields <- closedMap ["name","references"] term
      (,) <$> field fields "name" string <*> field fields "references" (array natural)
    moduleFact term = do
      fields <- closedMap ["unit","module","ordinals"] term
      (,,) <$> field fields "unit" string <*> field fields "module" string
        <*> field fields "ordinals" (array natural)

data CompilerInputCodecFacts
  = CodecCheckedInputs [((String,String),[(String,String)])] [(String,String)]
  | CodecUnsupportedBoot String String
  | CodecUnsupportedWired String String String String
  deriving (Eq,Show)

readCompilerInputCodecFacts :: FilePath -> FilePath -> FilePath -> IO (CompilerInputCodecFacts,String)
readCompilerInputCodecFacts work proof evidence = readFacts work "input_facts" [text proof,text evidence] $ \term -> do
  category <- named "category" term >>= string
  bodySeal <- named "body_sha256" term >>= digestString
  facts <- case category of
    "checked" -> do
      fields <- closedMap ["category","direct","closure","body_sha256"] term
      CodecCheckedInputs <$> field fields "direct" (array direct)
        <*> field fields "closure" (array owner)
    "unsupported-boot" -> do
      fields <- closedMap ["category","unit","module","body_sha256"] term
      CodecUnsupportedBoot <$> field fields "unit" string <*> field fields "module" string
    "unsupported-wired" -> do
      fields <- closedMap ["category","unit","module","imported_unit","imported_module","body_sha256"] term
      CodecUnsupportedWired <$> field fields "unit" string <*> field fields "module" string
        <*> field fields "imported_unit" string <*> field fields "imported_module" string
    _ -> Left "another compiler input observation category"
  pure (facts,bodySeal)
  where
    direct term = do
      fields <- closedMap ["owner","packages"] term
      (,) <$> field fields "owner" owner <*> field fields "packages" (array owner)

codecRequest :: FilePath -> String -> [Term] -> IO FilePath
codecRequest work operation arguments = issueCodecFixturePacket work
  (toStrictByteString (encodeTerm (TList [text "TPCODECFIXTURE1",text operation,TList arguments])))

readFacts :: FilePath -> String -> [Term] -> (Term -> Either String a) -> IO a
readFacts work operation arguments decode = do
  packet <- codecRequest work operation arguments
  envelope <- readCodecTerm (packet </> "facts.cbor")
  case envelope of
    TList [TString "TPCODECFACTS1",TString actual, facts] | actual == T.pack operation ->
      either (fail . ("invalid typed codec observation: " ++)) pure (decode facts)
    _ -> fail "another typed codec observation envelope"

readCodecTerm :: FilePath -> IO Term
readCodecTerm path = do
  let limit = 64 * 1024 * 1024
  bytes <- withBinaryFile path ReadMode (\handle -> BS.hGet handle (limit + 1))
  unless (BS.length bytes <= limit) (fail "codec fixture exceeds observation bound")
  case deserialiseFromBytes decodeTerm (BSL.fromStrict bytes) of
    Right (remaining,term) | BSL.null remaining -> pure term
    Left reason -> fail ("invalid codec fixture CBOR: " ++ show reason)
    _ -> fail "codec fixture has trailing CBOR bytes"

text :: String -> Term
text = TString . T.pack

closedMap :: [T.Text] -> Term -> Either String (Map.Map T.Text Term)
closedMap keys (TMap rows) = do
  pairs <- mapM (\case (TString key,value) -> Right (key,value); _ -> Left "non-text observation field") rows
  let fields = Map.fromList pairs
  unless (length pairs == Map.size fields && Map.keysSet fields == Set.fromList keys)
    (Left "invalid observation fields")
  pure fields
closedMap _ _ = Left "observation is not a named record"

named :: T.Text -> Term -> Either String Term
named key (TMap rows) = case [value | (TString actual,value) <- rows, actual == key] of
  [value] -> Right value
  _ -> Left "missing or repeated observation discriminator"
named _ _ = Left "observation is not a named record"

field :: Map.Map T.Text Term -> T.Text -> (Term -> Either String a) -> Either String a
field fields key decode = maybe (Left "missing observation field") decode (Map.lookup key fields)

string :: Term -> Either String String
string (TString value) = Right (T.unpack value)
string _ = Left "observation text expected"

digestString :: Term -> Either String String
digestString term = do
  value <- string term
  unless (length value == 64 && all (`elem` (['0'..'9'] ++ ['a'..'f'])) value)
    (Left "observation digest expected")
  pure value

boolean :: Term -> Either String Bool
boolean (TBool value) = Right value
boolean _ = Left "observation boolean expected"

array :: (Term -> Either String a) -> Term -> Either String [a]
array decode (TList values) = mapM decode values
array _ _ = Left "observation array expected"

natural :: forall a. (Integral a, Bounded a) => Term -> Either String a
natural term = do
  value <- case term of TInt number -> Right (toInteger number); TInteger number -> Right number; _ -> Left "observation integer expected"
  if value >= 0 && value <= toInteger (maxBound :: a)
    then Right (fromInteger value) else Left "observation integer out of range"

owner :: Term -> Either String (String,String)
owner term = do
  fields <- closedMap ["unit","module"] term
  (,) <$> field fields "unit" string <*> field fields "module" string

identity :: Term -> Either String SymbolIdentity
identity term = do
  fields <- closedMap ["unit","module","namespace","occurrence","record_parent"] term
  SymbolIdentity <$> field fields "unit" textField <*> field fields "module" textField
    <*> field fields "namespace" textField <*> field fields "occurrence" textField
    <*> field fields "record_parent" (optional textField)
  where textField value = T.pack <$> string value

optional :: (Term -> Either String a) -> Term -> Either String (Maybe a)
optional _ TNull = Right Nothing
optional decode value = Just <$> decode value

importOwner :: Term -> Either String CodecImportOwner
importOwner term = do
  kind <- named "kind" term >>= string
  case kind of
    "source" -> do
      fields <- closedMap ["kind","unit","module","version","ordinal","binder"] term
      CodecSourceOwner <$> field fields "unit" string <*> field fields "module" string
        <*> field fields "version" (optional string) <*> field fields "ordinal" natural
        <*> field fields "binder" identity
    "retained" -> do
      fields <- closedMap ["kind","binder","generation"] term
      CodecRetainedOwner <$> field fields "binder" identity <*> field fields "generation" natural
    "package" -> do
      fields <- closedMap ["kind","unit","module","interface_sha256","binder"] term
      CodecPackageOwner <$> field fields "unit" string <*> field fields "module" string
        <*> field fields "interface_sha256" digestString <*> field fields "binder" identity
    "retained-package" -> do
      fields <- closedMap ["kind","unit","module","interface_sha256","binder","generation"] term
      CodecRetainedPackageOwner <$> field fields "unit" string <*> field fields "module" string
        <*> field fields "interface_sha256" digestString <*> field fields "binder" identity
        <*> field fields "generation" natural
    _ -> Left "another import owner observation category"
