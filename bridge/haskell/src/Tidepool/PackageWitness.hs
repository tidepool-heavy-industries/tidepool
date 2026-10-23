-- | Exact package selections shared by authored compiler evidence and joins.
module Tidepool.PackageWitness
  ( PackageImportRoot(..), PackageImportEvidence(..), CompilerProvidedImport(..)
  , emptyPackageImports, encodeCompilerProvidedImport, packageImportRoot, validatePackageImportRoot
  , sealPackageImports, readPackageImports, revalidatePackageImports, encodePackageImports, decodeCapturedPackageImports
  , AdmittedPackageImports, emptyAdmittedPackageImports, extendAdmittedPackageImports
  , revalidateAdmittedPackageImports, validateAdmittedPackageSelection, extendAdmittedPackageImportsWith
  , extendAdmittedPackageImportsWithFacts
  , PackageRootObservations, withPackageRootObservations
  , packageImportRootWithin, validatePackageImportRootWithin
  , packageInputClosure
  , captureSupportedPackageProfileWith, supportedPackageProfileIdentity
  , requireObservationPackageEnvironment
  , CompilerPackageBoot, ServicePackageAssociation, readCompilerPackageBoot
  , compilerPackageBootLibdir, servicePackageWorkerInstance, servicePackageProfile
  , validateCompilerPackageBootConfiguration
  , validateCompilerPackageBoot, emitCompilerPackageBootAcknowledgement ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Encoding
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try, bracket, evaluate)
import Control.Monad (replicateM, unless, when, foldM, forM, forM_)
import Control.Monad.State.Strict (StateT(..), evalStateT, get, put)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString.Builder (Builder, byteString, toLazyByteString, word32LE)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import Data.Text.Lazy qualified as LT
import Data.Text.Lazy.Encoding qualified as LTE
import Data.Word (Word32)
import Data.List (groupBy, sortOn)
import Data.Bits ((.|.), shiftL)
import Data.Char (digitToInt)
import GHC.Data.ShortText qualified as ST
import GHC.Driver.Session (DynFlags, globalPackageDatabasePath, platformMisc, projectVersion)
import GHC.Settings (platformMisc_targetPlatformString)
import GHC.Unit.Info (UnitInfo, unitId, unitAbiHash, unitImportDirs, unitLibraryDirs, unitLibraryDynDirs, unitIncludeDirs)
import GHC.Unit.State (listUnitInfo)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Unit.Module.ModIface (mi_module, mi_usages, mi_deps)
import GHC.Unit.Module.Deps (Usage(..), Dependencies(..))
import Tidepool.FatIface (readPackageInterface)
import Tidepool.ExtractUtil (shaHex, getLibdir)
import GHC.Driver.Config.Finder (initFinderOpts)
import GHC.Driver.Env (HscEnv(..), hsc_HUG, hsc_units, hsc_home_unit_maybe, hsc_home_unit)
import GHC.Unit.Env (HomeUnitEnv(..))
import GHC.Unit.Home (isHomeUnit)
import GHC.Unit.Finder (InstalledFindResult(..), findExactModule)
import GHC.Unit.Module (Module, moduleUnit, moduleName, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (unitString, unitIdString, stringToUnit, toUnitId, GenWithIsBoot(..))
import Numeric (showHex)
import System.Directory (removeFile, canonicalizePath)
import System.FilePath (isAbsolute, takeDirectory, splitDirectories, (</>))
import System.Environment (lookupEnv)
import System.IO (openBinaryTempFile, hClose, stdout, hFlush)
import System.Posix.Files (createLink)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.Timing (readTimingEnabled, timeDetailPhase, emitCount)
import Tidepool.BoundedRead (FileObservations, FileObservation(..), observeFile, withFileObservations, fileObservationTotals, readFileAtMost)

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

-- Issued only from authenticated canonical sidecars. Installed packages belong
-- to the matched pinned compiler universe, rather than home-original custody.
-- Admission observes new roots; environment checks resolve them freshly and
-- terminal validation observes every selected root again.
data AdmittedPackageImports = AdmittedPackageImports
  (Map.Map (String,String) PackageImportRoot) Integer Int deriving (Eq, Show)

emptyAdmittedPackageImports :: AdmittedPackageImports
emptyAdmittedPackageImports = AdmittedPackageImports Map.empty 0 0

extendAdmittedPackageImports :: AdmittedPackageImports
  -> [(ExactIfaceArtifact, FilePath, String)] -> IO (Either String AdmittedPackageImports)
extendAdmittedPackageImports = extendAdmittedPackageImportsWith (\path bound -> readFileAtMost path (bound + 1))

extendAdmittedPackageImportsWith :: (FilePath -> Int -> IO BS.ByteString) -> AdmittedPackageImports
  -> [(ExactIfaceArtifact, FilePath, String)] -> IO (Either String AdmittedPackageImports)
extendAdmittedPackageImportsWith readInput initial witnesses = do
  extendAdmittedPackageImportsWithFacts
    (\(iface,path,sha) -> readAuthenticatedPackageImportsWith readInput path sha iface)
    initial witnesses

-- A retained owner may share decoded sidecar facts. Installed package bytes
-- still authenticate freshly, and each environment resolves its selection.
extendAdmittedPackageImportsWithFacts :: ( (ExactIfaceArtifact,FilePath,String)
  -> IO (Either String PackageImportEvidence)) -> AdmittedPackageImports
  -> [(ExactIfaceArtifact,FilePath,String)] -> IO (Either String AdmittedPackageImports)
extendAdmittedPackageImportsWithFacts readFacts initial witnesses = do
  result <- foldM authenticate (Right initial) witnesses
  case result of
    Right _ | not (null witnesses) -> do
      timing <- readTimingEnabled
      emitCount timing "package_proof.union_extensions" 1
    _ -> pure ()
  pure result
  where
    authenticate (Left reason) _ = pure (Left reason)
    authenticate (Right (AdmittedPackageImports selected references count)) witness = do
      authenticated <- readFacts witness
      case authenticated of
        Left reason -> pure (Left reason)
        Right evidence -> do
          let fresh = [root | root <- packageInterfaces evidence
                , Map.notMember (packageUnit root,packageModule root) selected]
          checked <- try (mapM (\root -> do
            bytes <- readFileAtMost (packagePath root) (32 * 1024 * 1024 + 1)
            when (BS.length bytes > 32 * 1024 * 1024) (fail "installed package interface exceeds 32 MiB")
            unless (digest bytes == packageSha256 root) (fail "selected package interface bytes changed")) fresh) :: IO (Either IOException [()])
          pure $ do
            either (Left . show) (const (Right ())) checked
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

-- Fresh environment selection is checked at each compiler environment boundary;
-- installed bytes remain in the pinned universe and terminal validation
-- observes their seals independently.
validateAdmittedPackageSelection :: HscEnv -> AdmittedPackageImports -> IO (Either String ())
validateAdmittedPackageSelection env (AdmittedPackageImports roots _ _) =
  foldM validate (Right ()) (Map.elems roots)
  where
    validate (Left reason) _ = pure (Left reason)
    validate (Right ()) expected = do
      let owner = mkModule (stringToUnit (packageUnit expected)) (mkModuleName (packageModule expected))
      selected <- selectedPackageInterface env owner
      pure $ case selected of
        Right path | path == packagePath expected -> Right ()
        _ -> Left "package selection differs from the certified import root"

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

-- One read-only operation shares physical observations, never resolver answers.
-- Discard this owner before loading interfaces, compiler effects or publication.
-- Exact expected witnesses remain separate, including conflicting alternatives.
data PackageRootObservations = PackageRootObservations HscEnv FileObservations

withPackageRootObservations :: HscEnv -> (PackageRootObservations -> IO a) -> IO a
withPackageRootObservations env action = withFileObservations $ \observations -> do
  timing <- readTimingEnabled
  result <- timeDetailPhase timing "package_preflight" "read_only" $
    action (PackageRootObservations env observations)
  when timing $ do
    -- These totals count successful physical observations, not failed IO attempts.
    (reads,bytes) <- fileObservationTotals observations
    emitCount timing "package_preflight.physical_reads" (fromIntegral reads)
    emitCount timing "package_preflight.physical_bytes" bytes
  pure result

packageImportRootWithin :: PackageRootObservations -> Module -> IO (Either String PackageImportRoot)
packageImportRootWithin (PackageRootObservations env observations) owner = do
  timing <- readTimingEnabled
  emitCount timing "package_preflight.root_requests" 1
  timeDetailPhase timing "package_imports" "root" $ do
    selected <- selectedPackageInterface env owner
    case selected of
      Left reason -> pure (Left reason)
      Right path -> do
        captured <- try (observeFile observations path Nothing) :: IO (Either IOException FileObservation)
        pure $ case captured of
          Left _ -> Left "selected package interface is unavailable"
          Right actual -> Right (PackageImportRoot (unitString (moduleUnit owner))
            (moduleNameString (moduleName owner)) path (observedSha256 actual))

validatePackageImportRootWithin :: PackageRootObservations -> PackageImportRoot -> IO (Either String ())
validatePackageImportRootWithin observations expected = do
  actual <- packageImportRootWithin observations
    (mkModule (stringToUnit (packageUnit expected)) (mkModuleName (packageModule expected)))
  pure $ case actual of
    Right witness | witness == expected -> Right ()
    _ -> Left "package selection or interface bytes differ from the certified import root"

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
readAuthenticatedPackageImports = readAuthenticatedPackageImportsWith (\path bound -> readFileAtMost path (bound + 1))

readAuthenticatedPackageImportsWith :: (FilePath -> Int -> IO BS.ByteString)
  -> FilePath -> String -> ExactIfaceArtifact -> IO (Either String PackageImportEvidence)
readAuthenticatedPackageImportsWith readInput path expectedDigest iface = do
  timing <- readTimingEnabled
  timeDetailPhase timing "package_imports" "authenticate" $ do
    result <- readEvidence timing
    -- A returned Either may defer its digest guards. Only diagnostics force
    -- this verdict here so its CPU is charged to the owning validation span.
    if timing then evaluate result else pure result
  where
    readEvidence timing = do
      captured <- try (do
        bytes <- readInput path (4 * 1024 * 1024)
        when (BS.length bytes > 4 * 1024 * 1024) (fail "package import evidence exceeds four MiB")
        (,) bytes <$> readInput (exactPath iface) (32 * 1024 * 1024))
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
          iface <- readPackageInterface env owner
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

-- The enclosing immutable profile/provider owns these facts on reusable boot.
-- These bytes alone cannot establish roots, service lifetime or query authority.
requireObservationPackageEnvironment :: IO ()
requireObservationPackageEnvironment = do
  fields <- traverse lookupEnv compilerPackageBootFields
  unless (all (== Nothing) fields)
    (fail "package profile observation cannot inherit compiler service boot fields")

compilerPackageBootFields :: [String]
compilerPackageBootFields =
  [ "TIDEPOOL_COMPILER_PACKAGE_PROFILE"
  , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256"
  , "TIDEPOOL_COMPILER_SERVICE_EPOCH"
  , "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID"
  , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID"
  , "TIDEPOOL_COMPILER_WORKER_INSTANCE"
  ]

supportedPackageProfileIdentity :: BS.ByteString -> String
supportedPackageProfileIdentity bytes =
  shaHex ("Tidepool GHC supported package profile v2\0" <> bytes)

captureSupportedPackageProfileWith :: FilePath -> HscEnv -> IO BS.ByteString
captureSupportedPackageProfileWith libdir env = do
  canonicalLibdir <- canonicalAbsolute libdir
  globalDB <- rawAbsolute (globalPackageDatabasePath flags)
  -- UnitState can index an instantiated unit under more than one key. Emit
  -- each actual UnitId once, refusing inconsistent aliases rather than picking.
  units <- forM (groupBy sameUnit (sortOn unitKey (listUnitInfo (hsc_units env)))) $ \aliases ->
    case aliases of
      [] -> fail "empty package profile unit alias group"
      first:rest -> do
        unless (all (== first) rest) (fail "inconsistent package profile unit aliases")
        pure first
  builder <- flip evalStateT maxSupportedPackageProfileBytes $ do
    chargeProfile 8
    header <- traverse framedText
      [projectVersion flags, platformMisc_targetPlatformString (platformMisc flags),
       canonicalLibdir, globalDB]
    count <- counted (length units)
    rows <- traverse unitRow units
    pure (byteString "TPGHC002" <> mconcat header <> count <> mconcat rows)
  pure (BL.toStrict (toLazyByteString builder))
  where
    flags = hsc_dflags env
    unitKey = unitIdString . unitId
    sameUnit left right = unitKey left == unitKey right

-- Shared with the service profile wrapper: this is the existing machine
-- control-input budget, not a limit on source modules or compiler residency.
maxSupportedPackageProfileBytes :: Int
maxSupportedPackageProfileBytes = 16 * 1024 * 1024

type ProfileEncoding = StateT Int IO

unitRow :: UnitInfo -> ProfileEncoding Builder
unitRow info = do
  identity <- framedText (unitIdString (unitId info))
  abi <- framedText (ST.unpack (unitAbiHash info))
  directories <- forM
    [unitImportDirs info, unitLibraryDirs info, unitLibraryDynDirs info, unitIncludeDirs info] $ \paths -> do
      -- GHC consumes these expanded paths as written. The source qualifier
      -- must see their kernel-order alias traversal before rooting the profile;
      -- canonicalizing here would erase mutable intermediate hops.
      raw <- liftIO (traverse (rawAbsolute . ST.unpack) paths)
      count <- counted (length raw)
      values <- traverse framedText raw
      pure (count <> mconcat values)
  pure (identity <> abi <> mconcat directories)

canonicalAbsolute :: FilePath -> IO FilePath
canonicalAbsolute path = do
  _ <- rawAbsolute path
  canonicalizePath path

rawAbsolute :: FilePath -> IO FilePath
rawAbsolute path = do
  unless (isAbsolute path && '\0' `notElem` path)
    (fail "supported package profile requires absolute paths")
  pure path

framedText :: String -> ProfileEncoding Builder
framedText value = do
  remaining <- get
  unless (remaining >= 4) (fail "package profile exceeds 16 MiB control-input budget")
  let prefix = BL.take (fromIntegral (remaining - 4) + 1)
        (LTE.encodeUtf8 (LT.pack value))
  unless (BL.length prefix <= fromIntegral (remaining - 4))
    (fail "package profile exceeds 16 MiB control-input budget")
  let bytes = BL.toStrict prefix
  unless (not (BS.elem 0 bytes)) (fail "package profile text contains NUL")
  chargeProfile (4 + BS.length bytes)
  pure (word32LE (fromIntegral (BS.length bytes)) <> byteString bytes)

counted :: Int -> ProfileEncoding Builder
counted count = do
  unless (toInteger count <= toInteger (maxBound :: Word32))
    (fail "package profile count exceeds its wire field")
  chargeProfile 4
  pure (word32LE (fromIntegral count))

chargeProfile :: Int -> ProfileEncoding ()
chargeProfile count = do
  remaining <- get
  unless (count <= remaining) (fail "package profile exceeds 16 MiB control-input budget")
  put (remaining - count)

-- Only the actual service launcher supplies these six coupled fields. A
-- connecting client or ordinary observation cannot issue this association.
data CompilerPackageBoot
  = ObservationPackageBoot
  | RootedPackageBoot BS.ByteString [FilePath] FilePath FilePath ServicePackageAssociation

data ServicePackageAssociation = ServicePackageAssociation
  BS.ByteString BS.ByteString BS.ByteString BS.ByteString BS.ByteString
  -- worker instance, service epoch, source identity, profile identity, facts SHA

servicePackageWorkerInstance :: ServicePackageAssociation -> BS.ByteString
servicePackageWorkerInstance (ServicePackageAssociation worker _ _ _ _) = worker

servicePackageProfile :: ServicePackageAssociation -> BS.ByteString
servicePackageProfile (ServicePackageAssociation _ _ _ profile _) = profile

data SupportedPackageFacts = SupportedPackageFacts FilePath FilePath [FilePath]

readCompilerPackageBoot :: IO CompilerPackageBoot
readCompilerPackageBoot = do
  fields <- traverse lookupEnv compilerPackageBootFields
  case fields of
    [Nothing,Nothing,Nothing,Nothing,Nothing,Nothing] -> pure ObservationPackageBoot
    [Just path,Just rawSha,Just epoch,Just source,Just profile,Just worker] -> do
      loaderPath <- lookupEnv "LD_LIBRARY_PATH"
      loaderOverrides <- traverse lookupEnv ["LD_PRELOAD","LD_AUDIT"]
      unless (loaderPath == Nothing && all (maybe True null) loaderOverrides)
        (fail "rooted compiler requires the fixed Nix-only loader profile")
      expectedSha <- hexIdentity rawSha
      epochBytes <- hexIdentity epoch
      sourceBytes <- hexIdentity source
      profileBytes <- hexIdentity profile
      workerBytes <- hexIdentity worker
      canonicalProfile <- canonicalAbsolute path
      unless (canonicalProfile == path && nixStoreObject path)
        (fail "compiler service profile must be an immutable canonical Nix object")
      bytes <- readFileAtMost path (maxSupportedPackageProfileBytes + 1)
      unless (BS.length bytes <= maxSupportedPackageProfileBytes)
        (fail "compiler service profile exceeds 16 MiB control-input budget")
      unless (SHA.hash bytes == expectedSha) (fail "compiler service profile digest differs")
      (expected,objects) <- either fail pure (decodePackageProfileWrapper bytes)
      unless (supportedPackageProfileIdentity expected == profile)
        (fail "compiler service profile identity differs")
      forM_ objects $ \object -> do
        canonical <- canonicalAbsolute object
        unless (canonical == object && nixStoreObject object)
          (fail "compiler service closure object is not a canonical Nix object")
      SupportedPackageFacts expectedLibdir globalDB paths <- either fail pure (decodeSupportedPackageFacts expected)
      declaredLibdir <- lookupEnv "TIDEPOOL_GHC_LIBDIR" >>= maybe
        (fail "rooted compiler requires TIDEPOOL_GHC_LIBDIR") pure
      unless (isAbsolute declaredLibdir && coveredBy objects declaredLibdir
          && all (`notElem` [".",".."]) (splitDirectories declaredLibdir))
        (fail "declared compiler libdir is outside retained immutable sources")
      libdir <- canonicalAbsolute declaredLibdir
      unless (libdir == expectedLibdir) (fail "compiler service libdir differs from profile")
      -- Keep the source alias and immutable target distinct. A mutable /tmp
      -- symlink to an admitted libdir is not an admitted source path.
      unless (coveredBy objects (declaredLibdir </> "settings")
          && coveredBy objects (declaredLibdir </> ".." </> "bin" </> "ghc-iserv"))
        (fail "declared compiler settings or interpreter source is not retained")
      settings <- canonicalAbsolute (libdir </> "settings")
      interpreter <- canonicalAbsolute (libdir </> ".." </> "bin" </> "ghc-iserv")
      -- The launcher retains/authenticates the profile object independently
      -- above. Its own store hash cannot appear inside its wrapper contents.
      -- This allowlist covers only the GHC inputs described by that wrapper.
      -- Before runGhc reads settings or opens package databases, every declared
      -- immutable input must be inside the actual preheld service closure.
      forM_ (libdir:settings:interpreter:globalDB:paths) $ \input -> do
        canonical <- canonicalAbsolute input
        unless (coveredBy objects input && coveredBy objects canonical)
          (fail "compiler service profile input is outside its retained closure")
      let association = ServicePackageAssociation workerBytes epochBytes sourceBytes profileBytes (SHA.hash expected)
      pure (RootedPackageBoot expected objects libdir globalDB association)
    _ -> fail "compiler service boot fields must be supplied together"

-- GHC has read the already-covered immutable settings, but has not yet opened
-- the selected databases through the initial setSessionDynFlags call.
validateCompilerPackageBootConfiguration :: CompilerPackageBoot -> DynFlags -> IO ()
validateCompilerPackageBootConfiguration ObservationPackageBoot _ = pure ()
validateCompilerPackageBootConfiguration (RootedPackageBoot _ objects _ expectedDB _) flags = do
  unless (coveredBy objects (globalPackageDatabasePath flags))
    (fail "configured GHC package database source is not retained")
  actualDB <- rawAbsolute (globalPackageDatabasePath flags)
  targetDB <- canonicalAbsolute actualDB
  unless (actualDB == expectedDB && coveredBy objects targetDB)
    (fail "configured GHC package database differs from retained profile")

validateCompilerPackageBoot :: CompilerPackageBoot -> FilePath -> HscEnv
  -> IO (Maybe ServicePackageAssociation)
validateCompilerPackageBoot ObservationPackageBoot _ _ = pure Nothing
validateCompilerPackageBoot (RootedPackageBoot expected objects _ _ association) libdir env = do
  forM_ (listUnitInfo (hsc_units env)) $ \unit ->
    forM_ (concat [unitImportDirs unit,unitLibraryDirs unit,unitLibraryDynDirs unit,unitIncludeDirs unit]) $ \source -> do
      unless (coveredBy objects (ST.unpack source))
        (fail "fresh GHC package input source is not retained")
      target <- canonicalAbsolute (ST.unpack source)
      unless (coveredBy objects target)
        (fail "fresh GHC package input target is not retained")
  actual <- captureSupportedPackageProfileWith libdir env
  unless (actual == expected) (fail "fresh GHC package facts differ from retained service profile")
  SupportedPackageFacts actualLibdir globalDB paths <- either fail pure (decodeSupportedPackageFacts actual)
  unless (all (coveredBy objects) (actualLibdir:globalDB:paths))
    (fail "fresh GHC package facts escape retained service closure")
  pure (Just association)

-- Reusable boot consumes the validated immutable target, without resolving
-- the caller's original environment alias for a second time.
compilerPackageBootLibdir :: CompilerPackageBoot -> IO FilePath
compilerPackageBootLibdir ObservationPackageBoot = getLibdir
compilerPackageBootLibdir (RootedPackageBoot _ _ libdir _ _) = pure libdir

emitCompilerPackageBootAcknowledgement :: Maybe ServicePackageAssociation -> IO ()
emitCompilerPackageBootAcknowledgement Nothing = pure ()
emitCompilerPackageBootAcknowledgement (Just (ServicePackageAssociation worker epoch source profile facts)) = do
  BS.hPut stdout ("TPBOOT01" <> worker <> epoch <> source <> profile <> facts)
  hFlush stdout

coveredBy :: [FilePath] -> FilePath -> Bool
coveredBy objects path = any (\object -> path == object || (object ++ "/") `prefixOf` path) objects
  where prefixOf prefix value = take (length prefix) value == prefix

nixStoreObject :: FilePath -> Bool
nixStoreObject path = case splitAt 11 path of
  ("/nix/store/",name) -> case splitAt 32 name of
    (hash,'-':suffix) -> length hash == 32 && not (null suffix)
      && all (`elem` ("0123456789abcdfghijklmnpqrsvwxyz" :: String)) hash
      && '/' `notElem` suffix
    _ -> False
  _ -> False

hexIdentity :: String -> IO BS.ByteString
hexIdentity value = do
  unless (length value == 64 && all (`elem` ("0123456789abcdef" :: String)) value)
    (fail "compiler service identity must be canonical SHA-256 hex")
  pure (BS.pack (pairs value))
  where
    pairs [] = []
    pairs (a:b:rest) = fromIntegral (16 * digitToInt a + digitToInt b) : pairs rest
    pairs _ = []

type ProfileDecoding = StateT BS.ByteString (Either String)

profileReject :: String -> ProfileDecoding a
profileReject message = StateT (const (Left message))

profileTake :: Int -> ProfileDecoding BS.ByteString
profileTake count = do
  remaining <- get
  if count < 0 || count > BS.length remaining then profileReject "truncated compiler package profile"
    else do
      let (value,rest) = BS.splitAt count remaining
      put rest
      pure value

profileWord :: ProfileDecoding Integer
profileWord = do
  bytes <- profileTake 4
  pure (foldr (\index value -> (value `shiftL` 8) .|. toInteger (BS.index bytes index)) 0 [0..3])

profileFrame :: ProfileDecoding BS.ByteString
profileFrame = do
  count <- profileWord
  remaining <- get
  if count > toInteger (BS.length remaining) then profileReject "compiler profile field leaves bounded body"
    else profileTake (fromInteger count)

profileString :: ProfileDecoding String
profileString = do
  value <- profileText
  if null value then profileReject "compiler package profile contains empty text" else pure value

profileText :: ProfileDecoding String
profileText = do
  bytes <- profileFrame
  case TE.decodeUtf8' bytes of
    Left _ -> profileReject "compiler package profile contains invalid UTF-8"
    Right value | T.any (== '\0') value -> profileReject "compiler package profile contains NUL text"
                | otherwise -> pure (T.unpack value)

profileList :: Int -> ProfileDecoding a -> ProfileDecoding [a]
profileList minimumBytes decode = do
  count <- profileWord
  remaining <- get
  if count > toInteger (BS.length remaining `div` minimumBytes)
    then profileReject "compiler package profile count exceeds remaining body"
    else replicateM (fromInteger count) decode

profileEnd :: ProfileDecoding ()
profileEnd = do
  remaining <- get
  unless (BS.null remaining) (profileReject "trailing compiler package profile bytes")

decodePackageProfileWrapper :: BS.ByteString -> Either String (BS.ByteString,[FilePath])
decodePackageProfileWrapper = evalStateT $ do
  magic <- profileTake 8
  unless (magic == "TPSPF001") (profileReject "unsupported compiler service profile wrapper")
  expected <- profileFrame
  objects <- profileList 4 profileString
  profileEnd
  pure (expected,objects)

decodeSupportedPackageFacts :: BS.ByteString -> Either String SupportedPackageFacts
decodeSupportedPackageFacts = evalStateT $ do
  magic <- profileTake 8
  unless (magic == "TPGHC002") (profileReject "unsupported compiler package facts version")
  _version <- profileString
  _target <- profileString
  libdir <- profileString
  globalDB <- profileString
  units <- profileList 24 $ do
    identity <- profileString
    _abi <- profileText
    paths <- replicateM 4 (profileList 4 profileString)
    pure (identity,concat paths)
  unless (map fst units == Set.toAscList (Set.fromList (map fst units)))
    (profileReject "compiler package units must be unique and sorted")
  profileEnd
  pure (SupportedPackageFacts libdir globalDB (concatMap snd units))
