-- | Exact package selections shared by authored compiler evidence and joins.
module Tidepool.PackageWitness
  ( PackageImportRoot(..), PackageImportEvidence(..), CompilerProvidedImport(..)
  , emptyPackageImports, encodeCompilerProvidedImport, packageImportRoot, validatePackageImportRoot
  , sealPackageImports, readPackageImports, revalidatePackageImports, encodePackageImports, decodeCapturedPackageImports
  , AdmittedPackageImports, emptyAdmittedPackageImports, extendAdmittedPackageImports
  , revalidateAdmittedPackageImports
  , packageInputClosure ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Encoding
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try, bracket, evaluate)
import Control.Monad (replicateM, unless, when, foldM)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Text qualified as T
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Unit.Module.ModIface (mi_module, mi_usages, mi_deps)
import GHC.Unit.Module.Deps (Usage(..), Dependencies(..))
import Tidepool.FatIface (readExactInterface)
import GHC.Driver.Config.Finder (initFinderOpts)
import GHC.Driver.Env (HscEnv(..), hsc_HUG, hsc_units, hsc_home_unit_maybe, hsc_home_unit)
import GHC.Unit.Env (HomeUnitEnv(..))
import GHC.Unit.Home (isHomeUnit)
import GHC.Unit.Finder (InstalledFindResult(..), findExactModule)
import GHC.Unit.Module (Module, moduleUnit, moduleName, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (unitString, unitIdString, stringToUnit, toUnitId, GenWithIsBoot(..))
import Numeric (showHex)
import System.Directory (removeFile)
import System.FilePath (isAbsolute, takeDirectory)
import System.IO (openBinaryTempFile, hClose)
import System.Posix.Files (createLink)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.Timing (readTimingEnabled, timeDetailPhase, emitCount)
import Tidepool.BoundedRead (FileObservations, FileObservation(..), observeFile, withFileObservations, readFileAtMost)

-- Directness is certified by the authored import evidence owner. This witness
-- authenticates the actual selected package interface, including unused imports.
data PackageImportRoot = PackageImportRoot
  { packageUnit :: String, packageModule :: String
  , packagePath :: FilePath, packageSha256 :: String
  } deriving (Eq, Ord, Show)

-- Compiler-provided imports have no installed .hi and grant no native lease.
data CompilerProvidedImport = CompilerPrimitive deriving (Eq, Ord, Show)

data PackageImportEvidence = PackageImportEvidence
  { packageInterfaces :: [PackageImportRoot]
  , compilerProvided :: [CompilerProvidedImport]
  } deriving (Eq, Ord, Show)

emptyPackageImports :: PackageImportEvidence
emptyPackageImports = PackageImportEvidence [] []

-- Issued only from authenticated canonical sidecars. Extensions append exact
-- owners; each current proof still resolves and hashes all selected roots.
data AdmittedPackageImports = AdmittedPackageImports
  (Map.Map (String,String) PackageImportRoot) Integer Int deriving (Eq, Show)

emptyAdmittedPackageImports :: AdmittedPackageImports
emptyAdmittedPackageImports = AdmittedPackageImports Map.empty 0 0

extendAdmittedPackageImports :: AdmittedPackageImports
  -> [(ExactIfaceArtifact, FilePath, String)] -> IO (Either String AdmittedPackageImports)
extendAdmittedPackageImports initial witnesses = do
  result <- foldM authenticate (Right initial) witnesses
  case result of
    Right _ | not (null witnesses) -> do
      timing <- readTimingEnabled
      emitCount timing "package_proof.union_extensions" 1
    _ -> pure ()
  pure result
  where
    authenticate (Left reason) _ = pure (Left reason)
    authenticate (Right (AdmittedPackageImports selected references count)) (iface,path,sha) = do
      authenticated <- readAuthenticatedPackageImports path sha iface
      pure $ do
        evidence <- authenticated
        staged <- foldM insertRoot selected (packageInterfaces evidence)
        let total = references + fromIntegral (length (packageInterfaces evidence))
        total `seq` pure (AdmittedPackageImports staged total (count + 1))
    insertRoot selected root = case Map.lookup (packageUnit root,packageModule root) selected of
      Just previous | previous /= root -> Left "conflicting package import witnesses for one owner"
                    | otherwise -> Right selected
      Nothing -> Right (Map.insert (packageUnit root,packageModule root) root selected)

revalidateAdmittedPackageImports :: FileObservations -> HscEnv
  -> AdmittedPackageImports -> IO (Either String ())
revalidateAdmittedPackageImports observations env (AdmittedPackageImports roots references count) = do
  timing <- readTimingEnabled
  emitCount timing "package_proof.authenticated_sidecars" (fromIntegral count)
  emitCount timing "package_proof.staged_references" references
  emitCount timing "package_proof.staged_full_witnesses" (fromIntegral (Map.size roots))
  foldM (validate timing) (Right ()) (Map.elems roots)
  where
    validate _ (Left reason) _ = pure (Left reason)
    validate timing (Right ()) expected = timeDetailPhase timing "package_imports" "root" $ do
      let owner = mkModule (stringToUnit (packageUnit expected)) (mkModuleName (packageModule expected))
      selected <- selectedPackageInterface env owner
      case selected of
        Right path | path == packagePath expected -> do
          captured <- try (observeFile observations path Nothing) :: IO (Either IOException FileObservation)
          pure $ case captured of
            Right actual | observedSha256 actual == packageSha256 expected -> Right ()
            _ -> Left "package selection or interface bytes differ from the certified import root"
        _ -> pure (Left "package selection or interface bytes differ from the certified import root")

encodeCompilerProvidedImport :: CompilerProvidedImport -> Encoding
encodeCompilerProvidedImport CompilerPrimitive = encodeListLen 3
  <> encodeString (T.pack "primitive")
  <> encodeString (T.pack (unitString (moduleUnit gHC_PRIM)))
  <> encodeString (T.pack (moduleNameString (moduleName gHC_PRIM)))

packageImportRoot :: HscEnv -> Module -> IO (Either String PackageImportRoot)
packageImportRoot env owner
  | isHomeUnit (hsc_home_unit env) (moduleUnit owner) = pure (Left "package root refers to the home unit")
  | otherwise = do
      timing <- readTimingEnabled
      timeDetailPhase timing "package_imports" "root" (resolve timing)
 where
  resolve timing = do
    selected <- selectedPackageInterface env owner
    case selected of
      Right path -> do
        captured <- try (BS.readFile path) :: IO (Either IOException BS.ByteString)
        case captured of
          Left _ -> pure (Left "selected package interface is unavailable")
          Right bytes -> do
            sha <- measuredDigest timing "package_root" bytes
            pure (Right (PackageImportRoot (unitString (moduleUnit owner))
              (moduleNameString (moduleName owner)) path sha))
      Left reason -> pure (Left reason)

selectedPackageInterface :: HscEnv -> Module -> IO (Either String FilePath)
selectedPackageInterface env owner
  | isHomeUnit (hsc_home_unit env) (moduleUnit owner) = pure (Left "package root refers to the home unit")
  | otherwise = do
      found <- findExactModule (hsc_FC env) (initFinderOpts (hsc_dflags env))
        (fmap (initFinderOpts . homeUnitEnv_dflags) (hsc_HUG env))
        (hsc_units env) (hsc_home_unit_maybe env) (toUnitId <$> owner)
      pure $ case found of
        InstalledFound location _ -> Right (ml_hi_file location)
        _ -> Left "package interface does not resolve in the matched compiler"

validatePackageImportRoot :: HscEnv -> PackageImportRoot -> IO (Either String ())
validatePackageImportRoot env expected = do
  actual <- packageImportRoot env (mkModule (stringToUnit (packageUnit expected)) (mkModuleName (packageModule expected)))
  pure $ case actual of
    Right witness | witness == expected -> Right ()
    _ -> Left "package selection or interface bytes differ from the certified import root"

-- | Authored compilation supplies actual direct resolved imports; interface
-- dependency fields cannot recover unused or instance-only source imports.
-- Empty selections are sealed explicitly, so missing evidence is never empty.
sealPackageImports :: FilePath -> ExactIfaceArtifact -> PackageImportEvidence -> IO ()
sealPackageImports path iface roots = bracket
  (do (temporary, handle) <- openBinaryTempFile (takeDirectory path) "package-imports.tmp"
      hClose handle
      pure temporary)
  removeFile $ \temporary -> do
    BS.writeFile temporary (encodeRoots iface roots)
    createLink temporary path

readPackageImports
  :: FilePath -> String -> ExactIfaceArtifact -> IO (Either String PackageImportEvidence)
readPackageImports path expectedDigest iface = do
  timing <- readTimingEnabled
  timeDetailPhase timing "package_imports" "read" $ do
    authenticated <- readAuthenticatedPackageImports path expectedDigest iface
    result <- case authenticated of
      Left reason -> pure (Left reason)
      Right roots -> do
        packageBytes <- try (mapM (BS.readFile . packagePath) (packageInterfaces roots)) :: IO (Either IOException [BS.ByteString])
        case packageBytes of
          Left _ -> pure (Left "selected package interface bytes changed")
          Right values -> do
            hashes <- mapM (measuredDigest timing "package_selection") values
            pure $ if hashes == map packageSha256 (packageInterfaces roots)
              then Right roots else Left "selected package interface bytes changed"
    if timing then evaluate result else pure result

-- | One exact-scope proof authenticates every owning interface and sidecar,
-- then observes each distinct package owner once through the current resolver.
-- Matching the complete resolved witness proves both the expected path bytes
-- and the current selection. No current byte observation survives a proof.
revalidatePackageImports
  :: HscEnv -> [(ExactIfaceArtifact, FilePath, String)] -> IO (Either String ())
revalidatePackageImports env witnesses = do
  admitted <- extendAdmittedPackageImports emptyAdmittedPackageImports witnesses
  case admitted of
    Left reason -> pure (Left reason)
    Right roots -> withFileObservations (\observations ->
      revalidateAdmittedPackageImports observations env roots)

-- Private authentication deliberately grants no current package-byte proof.
-- Standalone readers also check recorded paths; collective proofs resolve and
-- authenticate the strict union before they return success.
readAuthenticatedPackageImports
  :: FilePath -> String -> ExactIfaceArtifact -> IO (Either String PackageImportEvidence)
readAuthenticatedPackageImports path expectedDigest iface = do
  timing <- readTimingEnabled
  timeDetailPhase timing "package_imports" "authenticate" $ do
    result <- readEvidence timing
    -- A returned Either may defer its digest guards. Only diagnostics force
    -- this verdict here so its CPU is charged to the owning validation span.
    if timing then evaluate result else pure result
  where
    readEvidence timing = do
      captured <- try (do
        bytes <- readFileAtMost path (4 * 1024 * 1024 + 1)
        when (BS.length bytes > 4 * 1024 * 1024) (fail "package import evidence exceeds four MiB")
        (,) bytes <$> BS.readFile (exactPath iface))
        :: IO (Either IOException (BS.ByteString, BS.ByteString))
      case captured of
        Left (_ :: IOException) -> pure (Left "sealed package import evidence is unavailable")
        Right (bytes, ifaceBytes) -> do
          evidenceSha <- measuredDigest timing "package_evidence" bytes
          ifaceSha <- measuredDigest timing "owning_iface" ifaceBytes
          if evidenceSha /= expectedDigest || ifaceSha /= exactSha256 iface
            then pure (Left "sealed package import evidence or owning interface bytes changed")
            else do
              emitCount timing "package_proof.sidecar_decodes" 1
              pure $ case deserialiseFromBytes decodeRoots (BL.fromStrict bytes) of
                Left _ -> Left "invalid sealed package import evidence"
                Right (remaining, (owner, roots))
                  | not (BL.null remaining) || owner /= (exactUnit iface, exactModule iface, exactSha256 iface)
                      || encodeRoots iface roots /= bytes -> Left "package import evidence has a different owner or encoding"
                  | otherwise -> Right roots

-- Authenticate an already captured sidecar against its selected interface.
-- Current package resolution remains the collective proof's responsibility.
decodeCapturedPackageImports
  :: ExactIfaceArtifact -> BS.ByteString -> Either String PackageImportEvidence
decodeCapturedPackageImports iface bytes = case deserialiseFromBytes decodeRoots (BL.fromStrict bytes) of
  Left _ -> Left "invalid captured package import evidence"
  Right (remaining,(owner,roots))
    | not (BL.null remaining) || owner /= (exactUnit iface,exactModule iface,exactSha256 iface)
        || encodeRoots iface roots /= bytes -> Left "captured package evidence differs from its interface"
    | otherwise -> Right roots

-- Only enabled diagnostics force the digest before reporting its bytes. The
-- ordinary path retains the caller's lazy digest evaluation. No contents or
-- paths are retained; digest identities allow repeated work to be counted.
measuredDigest :: Bool -> String -> BS.ByteString -> IO String
measuredDigest timing purpose bytes = do
  let sha = digest bytes
  when timing $ do
    _ <- evaluate (length sha)
    emitCount timing ("hash_bytes." ++ purpose ++ "." ++ sha) (fromIntegral (BS.length bytes))
  pure sha

encodePackageImports :: ExactIfaceArtifact -> PackageImportEvidence -> BS.ByteString
encodePackageImports = encodeRoots

encodeRoots :: ExactIfaceArtifact -> PackageImportEvidence -> BS.ByteString
encodeRoots iface evidence = toStrictByteString $ encodeListLen 5 <> string "TPPKGROOTS" <> string "2"
  <> encodeListLen 3 <> foldMap string [exactUnit iface, exactModule iface, exactSha256 iface]
  <> encodeListLen (fromIntegral (length (packageInterfaces evidence))) <> foldMap root (packageInterfaces evidence)
  <> encodeListLen (fromIntegral (length (compilerProvided evidence))) <> foldMap encodeCompilerProvidedImport (compilerProvided evidence)
  where
    string = encodeString . T.pack
    root value = encodeListLen 4 <> foldMap string
      [packageUnit value, packageModule value, packagePath value, packageSha256 value]

decodeRoots :: Decoder s ((String, String, String), PackageImportEvidence)
decodeRoots = do
  array 5
  magic <- string
  version <- string
  unless (magic == "TPPKGROOTS" && version == "2") (fail "unsupported package import evidence")
  array 3
  owner <- (,,) <$> nonempty <*> nonempty <*> hash
  count <- decodeListLen
  when (count > 16384) (fail "too many package import roots")
  roots <- replicateM count $ do
    array 4
    PackageImportRoot <$> nonempty <*> nonempty <*> path <*> hash
  providedCount <- decodeListLen
  when (providedCount > 1) (fail "too many compiler-provided imports")
  provided <- replicateM providedCount $ do
    array 3
    category <- string
    unit <- nonempty
    name <- nonempty
    unless (category == "primitive" && unit == unitString (moduleUnit gHC_PRIM)
      && name == moduleNameString (moduleName gHC_PRIM)) $
      fail "unknown compiler-provided import"
    pure CompilerPrimitive
  unless (length roots == Set.size (Set.fromList roots)) (fail "duplicate package import root")
  unless (length roots == Set.size (Set.fromList [(packageUnit root, packageModule root) | root <- roots])) $
    fail "ambiguous package import owner"
  pure (owner, PackageImportEvidence roots provided)
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

-- | Inputs follow the exact installed interface graph, independently of which
-- home bodies were projected. Warm EPS contents are not input authority.
packageInputClosure :: HscEnv -> [PackageImportRoot]
  -> IO (Either String [PackageImportRoot])
packageInputClosure env roots = do
  selected <- foldM addRoot (Right Map.empty) roots
  case selected of
    Left reason -> pure (Left reason)
    Right required -> visit Map.empty required
  where
    key root = (packageUnit root, packageModule root)
    addRoot (Left reason) _ = pure (Left reason)
    addRoot (Right selected) root = pure $ case Map.lookup (key root) selected of
      Just old | old /= root -> Left "conflicting checked package interface selection"
      _ -> Right (Map.insert (key root) root selected)
    visit done pending
      | Map.null pending = pure (Right (Map.elems done))
      | Map.size done + Map.size pending > 16384 = pure (Left "package input closure exceeds bound")
      | otherwise = do
          let ((ownerKey, expected), rest) = Map.deleteFindMin pending
              owner = mkModule (stringToUnit (packageUnit expected)) (mkModuleName (packageModule expected))
          before <- validatePackageImportRoot env expected
          iface <- readExactInterface env owner
          case (before, iface) of
            (Right (), Right (interface, _)) | mi_module interface == owner -> do
              after <- validatePackageImportRoot env expected
              case after of
                Left reason -> pure (Left reason)
                Right () -> do
                  let dependencies = Set.toAscList $ Set.fromList $
                        concatMap usage (mi_usages interface)
                        ++ [mkModule (stringToUnit (unitIdString unit)) (gwib_mod name)
                           | (unit, name) <- Set.toList (dep_direct_mods (mi_deps interface))]
                        ++ dep_orphs (mi_deps interface) ++ dep_finsts (mi_deps interface)
                      known = Map.insert ownerKey expected done
                  resolved <- foldM (dependency known) (Right rest) dependencies
                  case resolved of
                    Left reason -> pure (Left reason)
                    Right next -> visit known next
            _ -> pure (Left "exact package input interface is unavailable or changed")
    dependency _ (Left reason) _ = pure (Left reason)
    dependency known (Right pending) owner
      | owner == gHC_PRIM = pure (Right pending)
      | isHomeUnit (hsc_home_unit env) (moduleUnit owner) =
          pure (Left "installed package input depends on an authored home owner")
      | otherwise = case Map.lookup ownerKey known of
          Just _ -> pure (Right pending)
          Nothing | Map.member ownerKey pending -> pure (Right pending)
          Nothing -> do
            root <- packageImportRoot env owner
            case root of
              Left reason -> pure (Left reason)
              Right selected -> addRoot (Right pending) selected
      where ownerKey = (unitString (moduleUnit owner), moduleNameString (moduleName owner))
    usage UsagePackageModule{usg_mod = owner} = [owner]
    usage UsageHomeModule{usg_mod_name = name, usg_unit_id = unit} =
      [mkModule (stringToUnit (unitIdString unit)) name]
    usage UsageHomeModuleInterface{usg_mod_name = name, usg_unit_id = unit} =
      [mkModule (stringToUnit (unitIdString unit)) name]
    usage UsageMergedRequirement{usg_mod = owner} = [owner]
    -- Installed package inputs consume the resulting .hi, not the source
    -- files used to build that package. Their fingerprints remain in its bytes.
    usage UsageFile{} = []
