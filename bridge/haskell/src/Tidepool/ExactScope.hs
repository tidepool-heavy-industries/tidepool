{-# LANGUAGE OverloadedStrings #-}

module Tidepool.ExactScope
  ( ExactScope(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..)
  , readExactScope, revalidateExactScope
  , writeExactCompilation
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Codec.CBOR.Encoding as E
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try)
import Control.Monad (replicateM, unless, when)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.Char (isHexDigit)
import Data.List (nub)
import qualified Data.Text as T
import GHC.Driver.Env (HscEnv)
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (getFileSize, createDirectory, createDirectoryIfMissing, makeAbsolute)
import System.FilePath (isAbsolute, takeDirectory, (</>))
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.PackageWitness
  ( readPackageImports, validatePackageImportRoot )
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), renderDependencyEvidence
  , revalidateDependencyEvidence )

data ExactScope = ExactScope
  { scopeManifestPath :: FilePath
  , scopeRequestSha256 :: String
  , scopeSemanticSha256 :: String
  , scopeInterfaces :: [(ExactIfaceArtifact, FilePath, String)]
  , scopeLexical :: [((String, String), [(String, String)])]
  , scopeProducts :: [ExactProduct]
  } deriving (Eq, Show)

data ExactProduct = ExactProduct
  { originalUnit :: String, originalModule :: String
  , originalVersion :: String, originalIfaceSha256 :: String
  , originalProductSha256 :: String, originalProductPath :: FilePath
  , originalGroups :: [ExactOriginalGroup]
  } deriving (Eq, Show)

data ExactOriginalGroup = ExactOriginalGroup
  { originalOrdinal :: Word, originalBinders :: [SymbolIdentity]
  , originalGlobals :: [(SymbolIdentity, Bool)]
  } deriving (Eq, Show)

data ExactCompilation = ExactCompilation
  { compilationScope :: ExactScope
  , compilationTransaction :: Word64
  , compilationSource :: FilePath
  , compilationImports :: [((String, String, Bool), [(String, String, Bool, String)])]
  } deriving (Eq, Show)

readExactScope :: FilePath -> IO (Either String ExactScope)
readExactScope path = do
  captured <- try (do
    unless (isAbsolute path) (fail "exact scope path must be absolute")
    size <- getFileSize path
    when (size > 4 * 1024 * 1024) (fail "exact scope exceeds four MiB")
    BS.readFile path)
    :: IO (Either IOException BS.ByteString)
  pure $ case captured of
    Left failure -> Left (show failure)
    Right bytes -> case deserialiseFromBytes decodeScope (BL.fromStrict bytes) of
      Left failure -> Left (show failure)
      Right (remaining, scope)
        | BL.null remaining -> Right scope
            { scopeManifestPath = path, scopeRequestSha256 = digest bytes }
        | otherwise -> Left "exact scope has trailing bytes"

-- Recheck the entire producer-owned closure in the consuming transaction;
-- no source file is a substitute for an admitted original interface.
revalidateExactScope :: HscEnv -> ExactScope -> IO (Either String ())
revalidateExactScope env scope = do
  result <- try (do
    bytes <- BS.readFile (scopeManifestPath scope)
    unless (digest bytes == scopeRequestSha256 scope) (fail "exact scope request changed")
    mapM_ checkInterface (scopeInterfaces scope)
    mapM_ checkProduct (scopeProducts scope))
    :: IO (Either IOException ())
  pure $ either (Left . show) Right result
  where
    checkInterface (iface, packages, packagesSha) = do
      roots <- readPackageImports packages packagesSha iface
      selected <- either fail pure roots
      mapM_ (\root -> validatePackageImportRoot env root >>= either fail pure) selected
    checkProduct originalProduct = do
      bytes <- BS.readFile (originalProductPath originalProduct)
      unless (digest bytes == originalProductSha256 originalProduct)
        (fail "exact original product changed")

-- Every successful compile owns a distinct immutable source snapshot. Check,
-- fold and inspection requests can consume several generated modules, so a
-- later successful transaction must not replace an earlier witness.
writeExactCompilation
  :: ExactCompilation -> DependencyEvidence -> IO ()
writeExactCompilation compilation evidence = do
  let scope = compilationScope compilation
      transaction = compilationTransaction compilation
      source = compilationSource compilation
      imports = compilationImports compilation
  path <- makeAbsolute source
  bytes <- BS.readFile path
  unless (any (\item -> dependencySourcePath item == path
      && dependencySourceSha256 item == digest bytes) (dependencySources evidence))
    (fail "exact compile source differs from consumed source")
  unchanged <- revalidateDependencyEvidence evidence
  unless unchanged (fail "exact compile consumed source changed before receipt")
  let directory = takeDirectory path </> ".exact-compilations" </> show transaction
      snapshot = directory </> "source.hs"
      encodeArray values = E.encodeListLen (fromIntegral (length values)) <> mconcat values
      text = E.encodeString . T.pack
      importRow (qualifier, name, boot, unit) = encodeArray
        [text qualifier, text name, E.encodeBool boot, text unit]
      moduleRow ((unit, name, boot), edges) = encodeArray
        [text unit, text name, E.encodeBool boot, encodeArray (map importRow edges)]
      receipt = encodeArray
        [text "TPEXACTCOMPILE", text "1", text (scopeRequestSha256 scope)
        , text (scopeSemanticSha256 scope), text path, text (digest bytes)
        , text snapshot, text (renderDependencyEvidence evidence)
        , encodeArray (map moduleRow imports)]
  createDirectoryIfMissing True (takeDirectory directory)
  createDirectory directory
  BS.writeFile snapshot bytes
  BS.writeFile (directory </> "receipt.cbor") (toStrictByteString receipt)

decodeScope :: Decoder s ExactScope
decodeScope = do
  array 6
  magic <- string
  version <- string
  unless (magic == "TPEXACTSCOPE" && version == "1") (fail "unsupported exact scope")
  semantic <- digestField
  interfaces <- bounded 4096 $ do
    array 7
    unit <- nonempty
    name <- nonempty
    path <- absolute
    sha <- digestField
    requirements <- bounded 4096 owner
    packages <- absolute
    packageSha <- digestField
    unique "exact requirements" requirements
    pure (ExactIfaceArtifact unit name path sha requirements, packages, packageSha)
  lexical <- bounded 4096 $ do
    array 2
    node <- owner
    imports <- bounded 4096 owner
    unique "exact lexical imports" imports
    pure (node, imports)
  products <- bounded 4096 $ do
    array 7
    originalProduct <- ExactProduct <$> nonempty <*> nonempty <*> digestField
      <*> digestField <*> digestField <*> absolute
      <*> bounded 65536 (do
        array 3
        ExactOriginalGroup <$> decodeWord <*> bounded 65536 identity
          <*> bounded 65536 (array 2 >> (,) <$> identity <*> decodeBool))
    unique "exact original ordinals" (map originalOrdinal (originalGroups originalProduct))
    let binders = concatMap originalBinders (originalGroups originalProduct)
    unique "exact original binders" binders
    unless (all (\binder -> T.unpack (symbolUnit binder) == originalUnit originalProduct
        && T.unpack (symbolModule binder) == originalModule originalProduct) binders)
      (fail "exact binder has another original owner")
    pure originalProduct
  let keys = [(exactUnit iface, exactModule iface) | (iface, _, _) <- interfaces]
      selected = map fst lexical
      productKeys = [(originalUnit originalProduct, originalModule originalProduct) | originalProduct <- products]
  unique "exact interface owners" keys
  unique "exact module names" (map snd keys)
  unique "exact lexical owners" selected
  unique "exact product owners" productKeys
  unless (all (`elem` keys) selected
      && all (`elem` selected) (concatMap snd lexical)
      && all (`elem` keys) productKeys
      && all (\(iface, _, _) -> all (`elem` keys) (exactRequirements iface)) interfaces
      && all (\originalProduct -> any (\(iface, _, _) ->
          (exactUnit iface, exactModule iface) == (originalUnit originalProduct, originalModule originalProduct)
          && exactSha256 iface == originalIfaceSha256 originalProduct) interfaces) products)
    (fail "incomplete or conflicting exact owner closure")
  pure (ExactScope "" "" semantic interfaces lexical products)

identity :: Decoder s SymbolIdentity
identity = do
  array 5
  unit <- decodeString
  name <- decodeString
  namespace <- decodeString
  occurrence <- decodeString
  token <- peekTokenType
  parent <- if token == TypeNull then decodeNull >> pure Nothing else Just <$> decodeString
  pure (SymbolIdentity unit name namespace occurrence parent)

owner :: Decoder s (String, String)
owner = array 2 >> (,) <$> nonempty <*> nonempty

array :: Int -> Decoder s ()
array count = decodeListLen >>= \actual -> unless (actual == count) (fail "invalid exact scope row")

bounded :: Int -> Decoder s a -> Decoder s [a]
bounded limit item = do
  count <- decodeListLen
  when (count > limit) (fail "exact scope inventory exceeds bound")
  replicateM count item

unique :: Eq a => String -> [a] -> Decoder s ()
unique label values = unless (length (nub values) == length values) (fail ("duplicate " ++ label))

string :: Decoder s String
string = T.unpack <$> decodeString

nonempty :: Decoder s String
nonempty = do
  value <- string
  unless (not (null value)) (fail "empty exact owner")
  pure value

absolute :: Decoder s FilePath
absolute = do
  value <- string
  unless (isAbsolute value) (fail "relative exact artifact path")
  pure value

digestField :: Decoder s String
digestField = do
  value <- string
  unless (length value == 64 && all isHexDigit value) (fail "invalid exact digest")
  pure value

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let value = showHex byte "" in replicate (2 - length value) '0' ++ value)
  . BS.unpack . SHA.hash
