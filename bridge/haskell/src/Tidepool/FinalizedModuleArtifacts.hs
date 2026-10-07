{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE LambdaCase #-}

-- | Captured finalization files and their exact dependency seals. The receipt
-- names these bounded files; it never duplicates their payloads inline.
module Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, captureFinalizedModuleArtifacts, materializeFinalizedModuleArtifacts
  , selectFinalizedSessionOutputs
  , emptyFinalizedModuleArtifacts, encodeFinalizedModuleArtifacts, finalizedInterfaceSeals, finalizedValueInterfaceSeals
  , LocalFinalizedAdmission, finalizedLocalAdmissions
  , localFinalizedInterface, localFinalizedHomeUnits, localFinalizedSourceSha256
  , localFinalizedRequirements, localFinalizedCore, revalidateLocalFinalizedAdmission
  , revalidateLocalFinalizedAdmissionWith
  , matchesCapturedFinalization ) where

import Codec.CBOR.Decoding qualified as D
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Encoding
import Control.Exception (Exception, IOException, bracket, evaluate, throwIO, try)
import Control.Monad (forM, forM_, unless, when, replicateM)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.List (sortOn)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC.Driver.Env (HscEnv, hsc_all_home_unit_ids)
import GHC.Unit.Home.ModInfo (HomeModInfo(..))
import GHC.Unit.Module (ModuleName, moduleName, moduleUnit, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.ModIface (mi_module)
import GHC.Unit.Types (unitIdString, unitString, stringToUnit)
import Numeric (showHex)
import System.Directory (makeAbsolute, createDirectoryIfMissing, removeFile)
import System.FilePath ((</>), normalise, isAbsolute)
import System.IO.Error (isAlreadyExistsError)
import System.IO (openBinaryTempFile, hClose, hIsClosed)
import System.Posix.Files (createLink, getSymbolicLinkStatus, isRegularFile)
import System.Mem.StableName (StableName, makeStableName)

import Tidepool.DependencyEvidence (DependencyEvidence(..), DependencyModule(..), DependencySource(..))
import Tidepool.BoundedRead (FileObservations, FileObservation(..), withFileObservations, observeFile, readFileAtMost)
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, ExactIfaceArtifact(..), originalInterfaceBytes, originalInterfaceSha256, originalSessionInterfaces, originalProducedSessionInterfaces )
import Tidepool.FinalizedModule (FinalizedModule(..), homeInterfaceUsageOwners)
import Tidepool.FinalizedCore (captureFinalizedCore, isUnsupportedFinalizedCore)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..), encodePackageImports, decodeCapturedPackageImports)
import Tidepool.Session (CapturedSessionInterface, capturedSessionInterface, capturedSessionInterfaceEvidence)

-- The producer's strict profile limits match the owning Rust receipt decoder.
interfaceLimit, packageLimit, coreLimit, payloadLimit :: Int
interfaceLimit = 32 * 1024 * 1024
packageLimit = 4 * 1024 * 1024
coreLimit = 32 * 1024 * 1024
payloadLimit = 128 * 1024 * 1024

data FinalizedArtifactFailure
  = MissingFinalizedSource String String
  | MissingFinalizedInterface String String
  | MissingFinalizedPackages String String
  | MissingFinalizedDependency String String
  | DuplicateFinalizedOwner String String
  | FinalizedOwnerKeyMismatch String String
  | InvalidFinalizedHomeOwner String String
  | DuplicateFinalizedSource String String
  | DuplicateFinalizedSourcePath FilePath
  | InvalidFinalizedSource FilePath
  | InvalidFinalizedPackage String String
  | FinalizedPayloadTooLarge String
  | FinalizedInventoryTooLarge
  | UnadmittedFinalizedSessionOutput String String
  | CapturedFinalizedPayloadChanged FilePath
  deriving Show
instance Exception FinalizedArtifactFailure

data CapturedPayload = CapturedPayload FilePath T.Text Int deriving (Eq, Show)

-- This identity pairs in-memory consumers with the exact compiler object that
-- issued their files. A stable name retains neither that object nor its Core.
newtype FinalizationIdentity = FinalizationIdentity (StableName FinalizedModule)
  deriving (Eq)

instance Show FinalizationIdentity where
  show _ = "FinalizationIdentity"

data CapturedModule = CapturedModule
  T.Text T.Text T.Text CapturedPayload CapturedPayload (Maybe CapturedPayload)
  [(T.Text,T.Text,T.Text)] FinalizationIdentity
  deriving (Eq, Show)

data CapturedModules = CapturedModules FilePath [CapturedModule]

data CapturedValueInterface = CapturedValueInterface
  T.Text T.Text CapturedPayload CapturedPayload [(T.Text,T.Text,T.Text)]

-- Selected type interfaces never enter the original source/native inventory.
data FinalizedModuleArtifacts = FinalizedModuleArtifacts [T.Text] (Maybe CapturedModules) [CapturedValueInterface]

-- Only this owner can project an admission from a completed capture. There is
-- no decoder or public constructor: serialized descriptors cannot mint it.
data LocalFinalizedAdmission = LocalFinalizedAdmission [T.Text] FilePath CapturedModule
  deriving (Eq, Show)

-- Owner-free targets still carry the complete compiler home-unit census.
emptyFinalizedModuleArtifacts :: HscEnv -> FinalizedModuleArtifacts
emptyFinalizedModuleArtifacts env = FinalizedModuleArtifacts
  (map (T.pack . unitIdString) (Set.toAscList (hsc_all_home_unit_ids env))) Nothing []

captureFinalizedModuleArtifacts
  :: OriginalInterfaceArtifacts -> HscEnv -> Map.Map ModuleName FinalizedModule
  -> Map.Map ModuleName PackageImportEvidence -> DependencyEvidence -> FilePath
  -> IO FinalizedModuleArtifacts
captureFinalizedModuleArtifacts originals env finalized packages evidence directory = do
  absoluteDirectory <- normalise <$> makeAbsolute directory
  let homeUnits = map (T.pack . unitIdString) (Set.toAscList (hsc_all_home_unit_ids env))
      owners = map originalKey (Map.elems finalized)
  when (length homeUnits > 128 || Map.size finalized > 128) $ throwIO FinalizedInventoryTooLarge
  forM_ (duplicates owners) $ \(unit,name) -> throwIO (DuplicateFinalizedOwner unit name)
  forM_ (Map.toAscList finalized) $ \(key,original) -> do
    let (unit,name) = originalKey original
    unless (moduleNameString key == name) $ throwIO (FinalizedOwnerKeyMismatch unit name)
    unless (T.pack unit `elem` homeUnits) $ throwIO (InvalidFinalizedHomeOwner unit name)
  let ordinaryNodes = filter (not . dependencyModuleBoot) (dependencyModules evidence)
  forM_ (duplicates [(dependencyModuleUnit node,dependencyModuleName node) | node <- ordinaryNodes]) $
    \(unit,name) -> throwIO (DuplicateFinalizedSource unit name)
  sourceModules <- forM (filter (not . null . dependencyModuleSource) ordinaryNodes) $ \node -> do
    path <- sourcePath (dependencyModuleSource node)
    pure ((dependencyModuleUnit node,dependencyModuleName node),path)
  sources <- forM (dependencySources evidence) $ \source -> do
    path <- sourcePath (dependencySourcePath source)
    unless (validDigest (dependencySourceSha256 source)) $ throwIO (InvalidFinalizedSource path)
    pure (path,dependencySourceSha256 source)
  forM_ (duplicates (map snd sourceModules) ++ duplicates (map fst sources)) $
    throwIO . DuplicateFinalizedSourcePath
  let sourceOwners = Map.fromList sourceModules
      sourceHashes = Map.fromList sources
      consumedSource unit name = Map.lookup (unit,name) sourceOwners >>= (`Map.lookup` sourceHashes)
      selectedPackages = Map.fromListWith Set.union
        [((packageUnit root,packageModule root),Set.singleton root)
        | original <- Map.elems finalized
        , Just roots <- [Map.lookup (moduleName (mi_module (hm_iface (finalizedHomeModInfo original)))) packages]
        , root <- packageInterfaces roots]
  forM_ (Map.keys (Map.filter ((> 1) . Set.size) selectedPackages)) $
    \(unit,name) -> throwIO (InvalidFinalizedPackage unit name)
  rows <- forM (sortOn originalKey (Map.elems finalized)) $ \original -> do
    identity <- FinalizationIdentity <$> (evaluate original >>= makeStableName)
    let iface = hm_iface (finalizedHomeModInfo original)
        owner = mi_module iface
        unit = unitString (moduleUnit owner)
        name = moduleNameString (moduleName owner)
    source <- T.pack <$> maybe (throwIO (MissingFinalizedSource unit name)) pure (consumedSource unit name)
    interfaceBytes <- originalInterfaceBytes originals owner
      >>= maybe (throwIO (MissingFinalizedInterface unit name)) pure
    roots <- maybe (throwIO (MissingFinalizedPackages unit name)) pure (Map.lookup (moduleName owner) packages)
    forM_ (duplicates [(packageUnit root,packageModule root) | root <- packageInterfaces roots]) $
      \(requiredUnit,requiredName) -> throwIO (InvalidFinalizedPackage requiredUnit requiredName)
    forM_ (packageInterfaces roots) $ \root ->
      when (T.pack (packageUnit root) `elem` homeUnits
          || null (packageUnit root) || null (packageModule root)
          || not (isAbsolute (packagePath root)) || not (validDigest (packageSha256 root))) $
        throwIO (InvalidFinalizedPackage (packageUnit root) (packageModule root))
    let interfaceSHA = digest interfaceBytes
        sidecar = encodePackageImports (ExactIfaceArtifact unit name "" (T.unpack interfaceSHA) []) roots
    interface <- capture absoluteDirectory "hi" interfaceLimit interfaceBytes
    package <- capture absoluteDirectory "packages.cbor" packageLimit sidecar
    core <- captureFinalizedCore env original absoluteDirectory >>= \case
      Right bytes -> Just <$> capture absoluteDirectory "core" coreLimit bytes
      Left refusal | isUnsupportedFinalizedCore refusal -> pure Nothing
                   | otherwise -> throwIO refusal
    requirements <- forM (homeInterfaceUsageOwners env iface) $ \(requiredUnit,requiredName) -> do
      unless (T.pack requiredUnit `elem` homeUnits) $
        throwIO (MissingFinalizedDependency requiredUnit requiredName)
      let requiredOwner = mkModule (stringToUnit requiredUnit)
            (mkModuleName requiredName)
      sha <- originalInterfaceSha256 originals requiredOwner
        >>= maybe (throwIO (MissingFinalizedDependency requiredUnit requiredName)) pure
      unless (validDigest sha) $ throwIO (MissingFinalizedDependency requiredUnit requiredName)
      pure (T.pack requiredUnit,T.pack requiredName,T.pack sha)
    pure (CapturedModule (T.pack unit) (T.pack name) source interface package core requirements identity)
  let required = Set.fromList [(unit,name)
        | CapturedModule _ _ _ _ _ _ dependencies _ <- rows, (unit,name,_) <- dependencies]
  values <- forM [snapshot | snapshot <- originalSessionInterfaces originals ++ originalProducedSessionInterfaces originals
      , let (owner,_) = capturedSessionInterface snapshot
      , (T.pack (unitString (moduleUnit owner)),T.pack (moduleNameString (moduleName owner))) `Set.member` required
          || maybe False (const True) (capturedSessionInterfaceEvidence snapshot)] $ \snapshot -> do
    let (owner,bytes) = capturedSessionInterface snapshot
        unit = unitString (moduleUnit owner)
        name = moduleNameString (moduleName owner)
        artifact = ExactIfaceArtifact unit name "" (T.unpack (digest bytes)) []
    (packageBytes,requirementBytes,nominalOwners) <- maybe
      (throwIO (MissingFinalizedPackages unit name)) pure (capturedSessionInterfaceEvidence snapshot)
    _ <- either fail pure (decodeCapturedPackageImports artifact packageBytes)
    owners <- case deserialiseFromBytes decodeValueRequirements (BL.fromStrict requirementBytes) of
      Right (remaining,selected) | BL.null remaining
          , Set.size (Set.fromList selected) == length selected
          , Set.fromList selected == Set.fromList nominalOwners -> pure selected
      _ -> fail "captured value requirements differ from its decoded binding types"
    dependencies <- forM (sortOn id owners) $ \(requiredUnit,requiredName) -> do
      unless (T.pack requiredUnit `elem` homeUnits && (requiredUnit,requiredName) /= (unit,name))
        (throwIO (MissingFinalizedDependency requiredUnit requiredName))
      sha <- originalInterfaceSha256 originals (mkModule (stringToUnit requiredUnit) (mkModuleName requiredName))
        >>= maybe (throwIO (MissingFinalizedDependency requiredUnit requiredName)) pure
      pure (T.pack requiredUnit,T.pack requiredName,T.pack sha)
    interface <- capture absoluteDirectory "value.hi" interfaceLimit bytes
    package <- capture absoluteDirectory "value.packages.cbor" packageLimit packageBytes
    pure (CapturedValueInterface (T.pack unit) (T.pack name) interface package dependencies)
  forM_ (duplicates [(unit,name) | CapturedValueInterface unit name _ _ _ <- values]) $
    \(unit,name) -> throwIO (DuplicateFinalizedOwner (T.unpack unit) (T.unpack name))
  let total = sum [size iface + size packages' + maybe 0 size core
        | CapturedModule _ _ _ iface packages' core _ _ <- rows]
        + sum [size iface + size package | CapturedValueInterface _ _ iface package _ <- values]
  when (total > payloadLimit) $ throwIO (FinalizedPayloadTooLarge "aggregate")
  pure (FinalizedModuleArtifacts homeUnits (Just (CapturedModules absoluteDirectory rows)) values)
  where
    originalKey original = let owner = mi_module (hm_iface (finalizedHomeModInfo original))
      in (unitString (moduleUnit owner),moduleNameString (moduleName owner))
    sourcePath path = do
      when (null path) $ throwIO (InvalidFinalizedSource path)
      normalise <$> makeAbsolute path
    capture absoluteDirectory suffix limit bytes = do
      when (BS.null bytes || BS.length bytes > limit) $ throwIO (FinalizedPayloadTooLarge suffix)
      let sha = digest bytes
          relative = T.unpack sha ++ ".finalized." ++ suffix
      BS.writeFile (absoluteDirectory </> relative) bytes
      pure (CapturedPayload relative sha (BS.length bytes))
    size (CapturedPayload _ _ count) = count

-- Each packet owns every relative payload it encodes, even when projection
-- reuses a finalization captured for another packet in the same transaction.
-- Destination-local exclusive publication preserves the captured bytes and
-- compiler-object identity without requiring the capture filesystem to match.
materializeFinalizedModuleArtifacts :: FilePath -> FinalizedModuleArtifacts -> IO FinalizedModuleArtifacts
materializeFinalizedModuleArtifacts directory artifacts@(FinalizedModuleArtifacts units captured values) =
  case captured of
    Nothing -> pure artifacts
    Just (CapturedModules source rows) -> do
      destination <- normalise <$> makeAbsolute directory
      if destination == source then pure artifacts else do
        createDirectoryIfMissing True destination
        let payloads = concat
              [[(interfaceLimit,interface),(packageLimit,package)]
                ++ maybe [] (\payload -> [(coreLimit,payload)]) core
              | CapturedModule _ _ _ interface package core _ _ <- rows]
              ++ concat [[(interfaceLimit,interface),(packageLimit,package)]
                | CapturedValueInterface _ _ interface package _ <- values]
        forM_ payloads $ \(limit,payload@(CapturedPayload _ sha count)) -> do
          let original = payloadPath source payload
              output = payloadPath destination payload
          when (count <= 0 || count > limit) $
            throwIO (CapturedFinalizedPayloadChanged original)
          originalStatus <- getSymbolicLinkStatus original
          unless (isRegularFile originalStatus) $
            throwIO (CapturedFinalizedPayloadChanged original)
          bytes <- readFileAtMost original (count + 1)
          unless (BS.length bytes == count && digest bytes == sha) $
            throwIO (CapturedFinalizedPayloadChanged original)
          bracket (openBinaryTempFile destination "finalized-payload.tmp")
            (\(temporary,handle) -> do
              closed <- hIsClosed handle
              unless closed (hClose handle)
              removeFile temporary)
            (\(temporary,handle) -> do
              BS.hPut handle bytes
              hClose handle
              linked <- try (createLink temporary output) :: IO (Either IOException ())
              case linked of
                Left failure | isAlreadyExistsError failure -> pure ()
                             | otherwise -> throwIO failure
                Right () -> pure ())
          outputStatus <- getSymbolicLinkStatus output
          unless (isRegularFile outputStatus) $
            throwIO (CapturedFinalizedPayloadChanged output)
          owned <- readFileAtMost output (count + 1)
          unless (owned == bytes) $
            throwIO (CapturedFinalizedPayloadChanged output)
        pure (FinalizedModuleArtifacts units (Just (CapturedModules destination rows)) values)

-- The full original/source inventory remains immutable. Only same-request
-- produced type outputs project to a packet's completed/current capture prefix;
-- imported interfaces and every retained dependency seal remain unchanged.
selectFinalizedSessionOutputs
  :: OriginalInterfaceArtifacts -> [CapturedSessionInterface]
  -> FinalizedModuleArtifacts -> IO FinalizedModuleArtifacts
selectFinalizedSessionOutputs originals selected artifacts@(FinalizedModuleArtifacts units modules values) = do
  let key snapshot = let (owner,_) = capturedSessionInterface snapshot
        in (T.pack (unitString (moduleUnit owner)), T.pack (moduleNameString (moduleName owner)))
      facts snapshot = (capturedSessionInterface snapshot, capturedSessionInterfaceEvidence snapshot)
      produced = Map.fromList [(key snapshot,snapshot) | snapshot <- originalProducedSessionInterfaces originals]
      selectedKeys = Set.fromList (map key selected)
      valueKey (CapturedValueInterface unit name _ _ _) = (unit,name)
      keep value = Map.notMember (valueKey value) produced || valueKey value `Set.member` selectedKeys
      kept = filter keep values
      dropped = Set.fromList [valueKey value | value <- values, not (keep value)]
      required = [(unit,name) | CapturedModule _ _ _ _ _ _ requirements _ <- capturedRows artifacts
            , (unit,name,_) <- requirements]
        ++ [(unit,name) | CapturedValueInterface _ _ _ _ requirements <- kept
            , (unit,name,_) <- requirements]
  forM_ (duplicates (map key selected)) $ \(unit,name) ->
    throwIO (DuplicateFinalizedOwner (T.unpack unit) (T.unpack name))
  forM_ selected $ \snapshot -> do
    let owner@(unit,name) = key snapshot
    unless (maybe False ((== facts snapshot) . facts) (Map.lookup owner produced)) $
      throwIO (UnadmittedFinalizedSessionOutput (T.unpack unit) (T.unpack name))
    (packages,_,_) <- maybe (throwIO (MissingFinalizedPackages (T.unpack unit) (T.unpack name))) pure
      (capturedSessionInterfaceEvidence snapshot)
    case [value | value <- values, valueKey value == owner] of
      [CapturedValueInterface _ _ (CapturedPayload _ interfaceSHA _) (CapturedPayload _ packageSHA _) _]
        | interfaceSHA == digest (snd (capturedSessionInterface snapshot))
          && packageSHA == digest packages -> pure ()
      _ -> throwIO (UnadmittedFinalizedSessionOutput (T.unpack unit) (T.unpack name))
  forM_ (filter (`Set.member` dropped) required) $ \(unit,name) ->
    throwIO (MissingFinalizedDependency (T.unpack unit) (T.unpack name))
  pure (FinalizedModuleArtifacts units modules kept)

finalizedInterfaceSeals :: FinalizedModuleArtifacts -> [((T.Text,T.Text),T.Text)]
finalizedInterfaceSeals artifacts =
  finalizedValueInterfaceSeals artifacts ++ [((unit,name),sha) | CapturedModule unit name _ (CapturedPayload _ sha _) _ _ _ _ <- capturedRows artifacts]

finalizedValueInterfaceSeals :: FinalizedModuleArtifacts -> [((T.Text,T.Text),T.Text)]
finalizedValueInterfaceSeals (FinalizedModuleArtifacts _ _ values) =
  [((unit,name),sha) | CapturedValueInterface unit name (CapturedPayload _ sha _) _ _ <- values]

decodeValueRequirements :: D.Decoder s [(String,String)]
decodeValueRequirements = do
  count <- D.decodeListLen
  when (count > 128) (fail "captured value requirement bound")
  replicateM count $ do
    width <- D.decodeListLen
    unless (width == 2) (fail "captured value requirement shape")
    (,) <$> (T.unpack <$> D.decodeString) <*> (T.unpack <$> D.decodeString)

finalizedLocalAdmissions :: FinalizedModuleArtifacts -> Map.Map (String,String) LocalFinalizedAdmission
finalizedLocalAdmissions (FinalizedModuleArtifacts units captured _) = case captured of
  Nothing -> Map.empty
  Just (CapturedModules directory rows) -> Map.fromList
    [((T.unpack unit,T.unpack name),LocalFinalizedAdmission units directory row)
    | row@(CapturedModule unit name _ _ _ _ _ _) <- rows]

localFinalizedInterface :: LocalFinalizedAdmission -> (ExactIfaceArtifact,FilePath,String)
localFinalizedInterface admission@(LocalFinalizedAdmission _ directory
    (CapturedModule unit name _ interface package _ _ _)) =
  ( ExactIfaceArtifact (T.unpack unit) (T.unpack name) (payloadPath directory interface)
      (payloadSha interface) (Map.keys (localFinalizedRequirements admission))
  , payloadPath directory package, payloadSha package )

localFinalizedHomeUnits :: LocalFinalizedAdmission -> Set.Set String
localFinalizedHomeUnits (LocalFinalizedAdmission units _ _) = Set.fromList (map T.unpack units)

localFinalizedSourceSha256 :: LocalFinalizedAdmission -> String
localFinalizedSourceSha256 (LocalFinalizedAdmission _ _ (CapturedModule _ _ source _ _ _ _ _)) = T.unpack source

localFinalizedRequirements :: LocalFinalizedAdmission -> Map.Map (String,String) String
localFinalizedRequirements (LocalFinalizedAdmission _ _ (CapturedModule _ _ _ _ _ _ requirements _)) =
  Map.fromList [((T.unpack unit,T.unpack name),T.unpack sha) | (unit,name,sha) <- requirements]

localFinalizedCore :: LocalFinalizedAdmission -> Maybe (FilePath,String)
localFinalizedCore (LocalFinalizedAdmission _ directory (CapturedModule _ _ _ _ _ core _ _)) =
  (\payload -> (payloadPath directory payload,payloadSha payload)) <$> core

matchesCapturedFinalization :: LocalFinalizedAdmission -> FinalizedModule -> IO Bool
matchesCapturedFinalization (LocalFinalizedAdmission _ _
    (CapturedModule _ _ _ _ _ _ _ (FinalizationIdentity captured))) original = do
  current <- evaluate original >>= makeStableName
  pure (captured == current)

-- Hash one bounded read of each captured file. Size/stat metadata is not proof;
-- substitutions, truncation and growth all refuse the local admission.
revalidateLocalFinalizedAdmission :: LocalFinalizedAdmission -> IO (Either String ())
revalidateLocalFinalizedAdmission admission =
  withFileObservations (\observations -> revalidateLocalFinalizedAdmissionWith observations admission)

revalidateLocalFinalizedAdmissionWith :: FileObservations -> LocalFinalizedAdmission -> IO (Either String ())
revalidateLocalFinalizedAdmissionWith observations (LocalFinalizedAdmission _ directory
    (CapturedModule _ _ _ interface package core _ _)) = do
  checked <- mapM validate ((interfaceLimit,interface):(packageLimit,package)
    : maybe [] (\payload -> [(coreLimit,payload)]) core)
  pure (() <$ sequence checked)
  where
    validate (limit,payload@(CapturedPayload _ sha count))
      | count <= 0 || count > limit = pure (Left "invalid captured finalized payload length")
      | otherwise = do
          captured <- try (observeFile observations (payloadPath directory payload) (Just count))
            :: IO (Either IOException FileObservation)
          pure $ case captured of
            Left _ -> Left "captured finalized payload is unavailable"
            Right observed
              | observedByteCount observed /= count -> Left "captured finalized payload length changed"
              | observedSha256 observed /= T.unpack sha -> Left "captured finalized payload bytes changed"
              | otherwise -> Right ()

capturedRows :: FinalizedModuleArtifacts -> [CapturedModule]
capturedRows (FinalizedModuleArtifacts _ captured _) = case captured of
  Nothing -> []
  Just (CapturedModules _ rows) -> rows

payloadPath :: FilePath -> CapturedPayload -> FilePath
payloadPath directory (CapturedPayload path _ _) = directory </> path

payloadSha :: CapturedPayload -> String
payloadSha (CapturedPayload _ sha _) = T.unpack sha

encodeFinalizedModuleArtifacts :: FinalizedModuleArtifacts -> Encoding
encodeFinalizedModuleArtifacts artifacts@(FinalizedModuleArtifacts units _ values) =
  array [encodeString "tidepool-ghc-finalized-module-v2",list encodeString units
    ,list encodeModule (capturedRows artifacts),list encodeValue (sortOn valueOwner values)]
  where
    encodeModule (CapturedModule unit name source interface package core requirements _) =
      array ([encodeString unit,encodeString name,encodeString source]
        ++ payloadFields interface ++ payloadFields package
        ++ [maybe encodeNull (array . payloadFields) core,list encodeRequirement requirements])
    valueOwner (CapturedValueInterface unit name _ _ _) = (unit,name)
    encodeValue (CapturedValueInterface unit name interface package requirements) =
      array ([encodeString unit,encodeString name] ++ payloadFields interface ++ payloadFields package
        ++ [list encodeRequirement requirements])
    encodeRequirement (unit,name,sha) = array (map encodeString [unit,name,sha])
    payloadFields (CapturedPayload path sha count) =
      [encodeString (T.pack path),encodeString sha,encodeWord64 (fromIntegral count)]

digest :: BS.ByteString -> T.Text
digest = T.pack . concatMap byteHex . BS.unpack . SHA256.hash
  where byteHex byte = let rendered = showHex byte "" in replicate (2-length rendered) '0' ++ rendered

validDigest :: String -> Bool
validDigest value = length value == 64 && all (`elem` (['0'..'9'] ++ ['a'..'f'])) value

duplicates :: Ord a => [a] -> [a]
duplicates = Map.keys . Map.filter (> (1 :: Int)) . Map.fromListWith (+) . map (\value -> (value,1))

array :: [Encoding] -> Encoding
array values = encodeListLen (fromIntegral (length values)) <> foldMap id values
list :: (a -> Encoding) -> [a] -> Encoding
list encode values = encodeListLen (fromIntegral (length values)) <> foldMap encode values
