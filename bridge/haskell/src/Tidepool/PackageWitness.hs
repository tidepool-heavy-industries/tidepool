-- | Exact package selections shared by authored compiler evidence and joins.
module Tidepool.PackageWitness
  ( PackageImportRoot(..), packageImportRoot, validatePackageImportRoot
  , sealPackageImports, readPackageImports, encodePackageImports ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Encoding
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try, bracket)
import Control.Monad (replicateM, unless, when)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Text qualified as T
import GHC.Driver.Config.Finder (initFinderOpts)
import GHC.Driver.Env (HscEnv(..), hsc_HUG, hsc_units, hsc_home_unit_maybe, hsc_home_unit)
import GHC.Unit.Env (HomeUnitEnv(..))
import GHC.Unit.Home (isHomeUnit)
import GHC.Unit.Finder (InstalledFindResult(..), findExactModule)
import GHC.Unit.Module (Module, moduleUnit, moduleName, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (unitString, stringToUnit, toUnitId)
import Numeric (showHex)
import System.Directory (getFileSize, removeFile)
import System.FilePath (isAbsolute, takeDirectory)
import System.IO (openBinaryTempFile, hClose)
import System.Posix.Files (createLink)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))

-- Directness is certified by the authored import evidence owner. This witness
-- authenticates the actual selected package interface, including unused imports.
data PackageImportRoot = PackageImportRoot
  { packageUnit :: String, packageModule :: String
  , packagePath :: FilePath, packageSha256 :: String
  } deriving (Eq, Ord, Show)

packageImportRoot :: HscEnv -> Module -> IO (Either String PackageImportRoot)
packageImportRoot env owner
  | isHomeUnit (hsc_home_unit env) (moduleUnit owner) = pure (Left "package root refers to the home unit")
  | otherwise = resolve
 where
  resolve = do
    found <- findExactModule (hsc_FC env) (initFinderOpts (hsc_dflags env))
      (fmap (initFinderOpts . homeUnitEnv_dflags) (hsc_HUG env))
      (hsc_units env) (hsc_home_unit_maybe env) (toUnitId <$> owner)
    case found of
      InstalledFound location _ -> do
        let path = ml_hi_file location
        captured <- try (BS.readFile path) :: IO (Either IOException BS.ByteString)
        pure $ case captured of
          Left _ -> Left "selected package interface is unavailable"
          Right bytes -> Right (PackageImportRoot (unitString (moduleUnit owner))
            (moduleNameString (moduleName owner)) path (digest bytes))
      _ -> pure (Left "package interface does not resolve in the matched compiler")

validatePackageImportRoot :: HscEnv -> PackageImportRoot -> IO (Either String ())
validatePackageImportRoot env expected = do
  actual <- packageImportRoot env (mkModule (stringToUnit (packageUnit expected)) (mkModuleName (packageModule expected)))
  pure $ case actual of
    Right witness | witness == expected -> Right ()
    _ -> Left "package selection or interface bytes differ from the certified import root"

-- | Authored compilation supplies actual direct resolved imports; interface
-- dependency fields cannot recover unused or instance-only source imports.
-- Empty selections are sealed explicitly, so missing evidence is never empty.
sealPackageImports :: FilePath -> ExactIfaceArtifact -> [PackageImportRoot] -> IO ()
sealPackageImports path iface roots = bracket
  (do (temporary, handle) <- openBinaryTempFile (takeDirectory path) "package-imports.tmp"
      hClose handle
      pure temporary)
  removeFile $ \temporary -> do
    BS.writeFile temporary (encodeRoots iface roots)
    createLink temporary path

readPackageImports
  :: FilePath -> String -> ExactIfaceArtifact -> IO (Either String [PackageImportRoot])
readPackageImports path expectedDigest iface = do
  captured <- try $ do
    size <- getFileSize path
    when (size > 4 * 1024 * 1024) (fail "package import evidence exceeds four MiB")
    (,) <$> BS.readFile path <*> BS.readFile (exactPath iface)
    :: IO (Either IOException (BS.ByteString, BS.ByteString))
  case captured of
    Left (_ :: IOException) -> pure (Left "sealed package import evidence is unavailable")
    Right (bytes, ifaceBytes)
      | digest bytes /= expectedDigest || digest ifaceBytes /= exactSha256 iface ->
          pure (Left "sealed package import evidence or owning interface bytes changed")
      | otherwise -> case deserialiseFromBytes decodeRoots (BL.fromStrict bytes) of
          Left _ -> pure (Left "invalid sealed package import evidence")
          Right (remaining, (owner, roots))
            | not (BL.null remaining) || owner /= (exactUnit iface, exactModule iface, exactSha256 iface)
                || encodeRoots iface roots /= bytes -> pure (Left "package import evidence has a different owner or encoding")
            | otherwise -> do
                packageBytes <- try (mapM (BS.readFile . packagePath) roots) :: IO (Either IOException [BS.ByteString])
                pure $ case packageBytes of
                  Right values | map digest values == map packageSha256 roots -> Right roots
                  _ -> Left "selected package interface bytes changed"

encodePackageImports :: ExactIfaceArtifact -> [PackageImportRoot] -> BS.ByteString
encodePackageImports = encodeRoots

encodeRoots :: ExactIfaceArtifact -> [PackageImportRoot] -> BS.ByteString
encodeRoots iface roots = toStrictByteString $ encodeListLen 4 <> string "TPPKGROOTS" <> string "1"
  <> encodeListLen 3 <> foldMap string [exactUnit iface, exactModule iface, exactSha256 iface]
  <> encodeListLen (fromIntegral (length roots)) <> foldMap root roots
  where
    string = encodeString . T.pack
    root value = encodeListLen 4 <> foldMap string
      [packageUnit value, packageModule value, packagePath value, packageSha256 value]

decodeRoots :: Decoder s ((String, String, String), [PackageImportRoot])
decodeRoots = do
  array 4
  magic <- string
  version <- string
  unless (magic == "TPPKGROOTS" && version == "1") (fail "unsupported package import evidence")
  array 3
  owner <- (,,) <$> nonempty <*> nonempty <*> hash
  count <- decodeListLen
  when (count > 16384) (fail "too many package import roots")
  roots <- replicateM count $ do
    array 4
    PackageImportRoot <$> nonempty <*> nonempty <*> path <*> hash
  pure (owner, roots)
  where
    array size = decodeListLen >>= \actual -> unless (size == actual) (fail "invalid package evidence field count")
    string = T.unpack <$> decodeString
    nonempty = string >>= \value -> if null value then fail "empty package identity" else pure value
    path = string >>= \value -> if isAbsolute value then pure value else fail "relative package interface path"
    hash = string >>= \value -> if length value == 64 && all (`elem` ['0'..'9'] ++ ['a'..'f']) value
      then pure value else fail "invalid package interface digest"

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let digits = showHex byte "" in replicate (2 - length digits) '0' ++ digits)
  . BS.unpack . SHA.hash
