-- | Persisted declaration interfaces preserve original Names and implementation
-- dependencies while contributing only the selected lexical instance inventory.
module Tidepool.DeclarationJoin
  ( ModuleSnapshot(..), ExportNamespace(..), DeclarationKind(..), ExportIdentity(..), DeclarationExport(..)
  , ClassInstanceEvidence(..), InstanceInventory(..), JoinDecision(..), JoinRejection(..)
  , buildJoinedInterface, validateRetainedFamilyInstances
  , exportIdentity, interfaceExports, interfaceInventory
  , ReservedJoin(..), DeclarationArtifact(..), DeclarationWrite(..)
  , DeclarationJoinInput(..), DeclarationJoinOutcome(..)
  , readDeclarationJoin, encodeDeclarationJoin, validateDeclarationJoin, renderDeclarationJoinOutcome
  , DeclarationInventory(..), DeclarationOperation(..), DeclarationInventoryOutcome(..)
  , readDeclarationOperation, encodeDeclarationInventory, inspectDeclarationArtifacts
  , renderDeclarationInventoryOutcome
  , HostBindingInterfaceInput(..), BindingInterfacePurpose(..), encodeHostBindingInterface, encodeBindingInterfacePurpose
  , renderDeclarationSelection
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Encoding
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import Control.DeepSeq (rnf)
import Control.Exception (bracket, evaluate)
import Control.Monad (forM, replicateM, unless, when)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.List (elemIndex, intercalate, nubBy, sort, sortBy, tails)
import Data.Maybe (catMaybes, isJust)
import Data.Text qualified as T
import Data.Word (Word64)
import GHC.Core.FamInstEnv
import GHC.Core.InstEnv
import GHC.Core.Class (classTyVars)
import GHC.Core.TyCon (isClassTyCon, tyConInjectivityInfo, Injectivity(..), tyConAssoc_maybe, tyConName, tyConTyVars)
import GHC.Core.Coercion (etaExpandCoAxBranch)
import GHC.Core.Coercion.Axiom (coAxiomName, coAxiomTyCon, coAxiomSingleBranch)
import GHC.Core.Unify (tcMatchTys, tcUnifyTys)
import GHC.Data.FastString (unpackFS)
import GHC.Driver.Env (HscEnv, lookupType, hscEPS, hsc_HUG, hsc_dflags)
import GHC.Driver.Session (targetProfile)
import GHC.Iface.Binary (writeBinIface, TraceBinIFace(..), CompressionIFace(..))
import GHC.Iface.Recomp (addFingerprints)
import GHC.Iface.Make (mkIfaceExports)
import GHC.Iface.Syntax (IfaceClsInst(..), IfaceFamInst(..))
import GHC.Iface.Load (loadSysInterface)
import GHC.Tc.Instance.FunDeps (checkFunDeps)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.Avail (AvailInfo(..), availName, availNames)
import GHC.Types.Name
import GHC.Types.TyThing (TyThing(..))
import GHC.Types.Unique.Set (addOneToUniqSet, elementOfUniqSet, emptyUniqSet)
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit, stableModuleCmp, mkModule, mkModuleName)
import GHC.Unit.Module.Env (mkModuleSet, emptyModuleSet)
import GHC.Unit.Module.Deps (Dependencies(..), noDependencies)
import GHC.Unit.Module.ModIface
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt)
import GHC.Unit.Env (unitEnv_hpts)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Types (unitString, stringToUnit)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Numeric (showHex)
import System.Directory (doesFileExist, getFileSize, removeFile)
import System.FilePath (isAbsolute, takeDirectory)
import System.IO (openBinaryTempFile, hClose)
import System.Posix.Files (createLink)
import Tidepool.ExactHydration
import Tidepool.CheckedCell (CheckedSignature, decodeCheckedSignature, encodeCheckedSignature, validateCheckedTypeWitnessBytes)
import Tidepool.Json (jsonString)
import Tidepool.Timing (readTimingEnabled, timeDetailPhase)
import Tidepool.PackageWitness
  ( PackageImportRoot(..), PackageImportEvidence(..), emptyPackageImports, readPackageImports, sealPackageImports, validatePackageImportRoot )

data ModuleSnapshot = ModuleSnapshot
  { snapshotModule :: String, snapshotPath :: FilePath, snapshotSha256 :: String
  } deriving (Eq, Show)

data ExportNamespace = ValueNamespace | TypeNamespace | ConstructorNamespace | FieldNamespace
  deriving (Eq, Ord, Show)

data DeclarationKind = ValueDeclaration | TypeDeclaration | ClassDeclaration
  deriving (Eq, Ord, Show)

data ExportIdentity = ExportIdentity
  { exportUnit :: String, exportModule :: String
  , exportNamespace :: ExportNamespace, exportOccurrence :: String
  , exportRecordParent :: Maybe String
  } deriving (Eq, Ord, Show)

data DeclarationExport = DeclarationExport
  { exportKind :: DeclarationKind, exportHead :: ExportIdentity, exportChildren :: [ExportIdentity]
  } deriving (Eq, Ord, Show)

data ClassInstanceEvidence = ClassInstanceEvidence
  { instanceDfun :: ExportIdentity, instanceClass :: ExportIdentity
  , instanceSelectedAxioms :: [ExportIdentity]
  } deriving (Eq, Ord, Show)

data InstanceInventory = InstanceInventory
  { inventoryClasses :: [ClassInstanceEvidence], inventoryFamilies :: [ExportIdentity]
  } deriving (Eq, Show)

data JoinRejection = ArtifactChanged | ExportMismatch | ClassInstanceConflict
  | FamilyInstanceConflict | InstanceMismatch | Unprovable
  deriving (Eq, Show)
data JoinDecision = JoinAccepted | JoinRejected JoinRejection String
  deriving (Eq, Show)

sha256 :: BS.ByteString -> String
sha256 = concatMap (\byte -> let digits = showHex byte "" in replicate (2 - length digits) '0' ++ digits)
  . BS.unpack . SHA.hash

-- Force only the issued scalar observations, never the hydrated GHC knot.
-- Otherwise an IO action returning a lazy Either would charge its checks to
-- the later interface writer instead of the phase that owns them.
measureJoin :: Bool -> String -> (a -> ()) -> IO a -> IO a
measureJoin enabled phase forceResult action =
  timeDetailPhase enabled "declaration_join" phase $ do
    result <- action
    when enabled (evaluate (forceResult result))
    pure result

forceIdentity :: ExportIdentity -> ()
forceIdentity identity = rnf
  (exportUnit identity, exportModule identity, exportNamespace identity `seq` (),
    exportOccurrence identity, exportRecordParent identity)

forceIdentities :: [ExportIdentity] -> ()
forceIdentities = foldr (\identity rest -> forceIdentity identity `seq` rest) ()

forceInventory :: InstanceInventory -> ()
forceInventory inventory =
  foldr (\record rest -> forceIdentity (instanceDfun record) `seq`
    forceIdentity (instanceClass record) `seq`
    forceIdentities (instanceSelectedAxioms record) `seq` rest)
    (forceIdentities (inventoryFamilies inventory)) (inventoryClasses inventory)

forceExport :: DeclarationExport -> ()
forceExport exported = exportKind exported `seq` forceIdentity (exportHead exported)
  `seq` forceIdentities (exportChildren exported)

forceEither :: (a -> ()) -> Either String a -> ()
forceEither _ (Left diagnostic) = rnf diagnostic
forceEither forceResult (Right result) = forceResult result

forceDecision :: JoinDecision -> ()
forceDecision JoinAccepted = ()
forceDecision (JoinRejected reason diagnostic) = reason `seq` rnf diagnostic

-- | Construct a source-less reexport interface from already verified, hydrated
-- immutable implementation artifacts. Original Names, dfuns and coercion axioms
-- remain owned by those artifacts; the Join introduces no declaration bodies.
-- The caller installs this interface through the same exact hydration boundary
-- that every later consumer uses.
buildJoinedInterface
  :: HscEnv -> Module -> FilePath -> [ModIface]
  -> [DeclarationExport] -> InstanceInventory -> [ExportIdentity]
  -> IO (Either (JoinRejection, String) ModuleSnapshot)
buildJoinedInterface hsc joined path originals exports selected familyClosure = do
  timing <- readTimingEnabled
  exportChoices <- measureJoin timing "exports"
    (forceEither (foldr (\avail rest -> forceIdentities (map exportIdentity (availNames avail)) `seq` rest) ()) . sequence)
    (mapM selectExport exports)
  inventories <- measureJoin timing "inventories"
    (\rows -> case sequence exportChoices of
      Left _ -> ()
      Right _ -> forceEither (foldr (\inventory rest -> forceInventory inventory `seq` rest) ()) (sequence rows))
    (mapM (interfaceInventory hsc) originals)
  case (sequence exportChoices, sequence inventories) of
    (Left diagnostic, _) -> pure (Left (ExportMismatch, diagnostic))
    (_, Left diagnostic) -> pure (Left (Unprovable, diagnostic))
    (Right avails, Right actualInventories)
      | any ((== joined) . mi_module) originals ->
          pure (Left (ArtifactChanged, "reserved Join module already exists in implementation closure"))
      | normalizeInventory selected /= actualSelected actualInventories ->
          pure (Left (InstanceMismatch, "selected dfun or axiom is absent from the exact implementation closure"))
      | sort (nubBy (==) familyClosure) /= actualFamilies ->
          pure (Left (InstanceMismatch, "retained family inventory differs from the exact implementation closure"))
      | otherwise -> do
          let hmis = [hmi | hpt <- unitEnv_hpts (hsc_HUG hsc), hmi <- eltsHpt hpt]
              allFamilies = concatMap (md_fam_insts . hm_details) hmis
              selectedClassNames = map instanceDfun (inventoryClasses selected)
              selectedClasses = [i | hmi <- hmis, i <- instEnvElts (md_insts (hm_details hmi))
                  , exportIdentity (is_dfun_name i) `elem` selectedClassNames]
              selectedFamilyNames = inventoryFamilies selected
              selectedFamilies = [i | i <- allFamilies
                  , exportIdentity (coAxiomName (fi_axiom i)) `elem` selectedFamilyNames]
              selectedOwners = map (nameModule . is_dfun_name) selectedClasses
              orphanOwners = stableModules
                (selectedOwners ++ concatMap (dep_orphs . mi_deps) originals)
              familyOwners = stableModules
                (map mi_module (filter (not . null . mi_fam_insts) originals)
                  ++ concatMap (dep_finsts . mi_deps) originals)
          -- Force the pinned package closure before taking an EPS snapshot.
          -- Home lookups must resolve from the already complete HPT, so these
          -- metadata loads cannot add hidden home instances to EPS.
          _ <- timeDetailPhase timing "declaration_join" "package_visibility" $
            initIfaceCheck (ppr joined) hsc $
            mapM (loadSysInterface (ppr joined)) (stableModules (orphanOwners ++ familyOwners))
          eps <- hscEPS hsc
          let classes = InstEnvs (eps_inst_env eps)
                  (mkInstEnv (nubBy (\a b -> is_dfun_name a == is_dfun_name b) selectedClasses))
                  (mkModuleSet orphanOwners)
              selectedEnv = (emptyFamInstEnv, extendFamInstEnvList emptyFamInstEnv selectedFamilies)
          consistency <- measureJoin timing "selected_consistency" forceDecision
            (pure (validateInstances classes selectedEnv))
          familyConsistency <- case consistency of
            JoinRejected {} -> pure JoinAccepted
            JoinAccepted -> measureJoin timing "retained_family_consistency" forceDecision
              (pure (validateRetainedFamilyInstances (eps_fam_inst_env eps) allFamilies))
          case firstRejection consistency familyConsistency of
            Just rejection -> pure (Left rejection)
            Nothing -> do
              let deps = noDependencies
                    { dep_orphs = orphanOwners
                    , dep_finsts = familyOwners
                    , dep_direct_pkgs = foldMap (dep_direct_pkgs . mi_deps) originals
                    , dep_plugin_pkgs = foldMap (dep_plugin_pkgs . mi_deps) originals
                    , dep_trusted_pkgs = foldMap (dep_trusted_pkgs . mi_deps) originals
                    }
                  partial = set_mi_fam_insts families $ set_mi_insts instances $
                    set_mi_exports (mkIfaceExports avails) $ set_mi_deps deps (emptyPartialModIface joined)
              iface <- measureJoin timing "fingerprints"
                (\value -> mi_iface_hash (mi_final_exts value) `seq` ())
                (addFingerprints hsc partial)
              -- Hard-link publication is exclusive: an existing immutable Join
              -- artifact is never replaced, including a retry's old output.
              digest <- measureJoin timing "write_interface" rnf $ bracket
                (do (temporary, handle) <- openBinaryTempFile (takeDirectory path) "join-iface.tmp"
                    hClose handle
                    pure temporary)
                removeFile $ \temporary -> do
                  writeBinIface (targetProfile (hsc_dflags hsc)) QuietBinIFace NormalCompression temporary iface
                  hash <- sha256 <$> BS.readFile temporary
                  createLink temporary path
                  pure hash
              pure (Right (ModuleSnapshot (moduleNameString (moduleName joined)) path digest))
  where
    originalAvails = concatMap mi_exports originals
    instances = nubBy (\a b -> ifDFun a == ifDFun b)
      [i | iface <- originals, i <- mi_insts iface,
          exportIdentity (ifDFun i) `elem` map instanceDfun (inventoryClasses selected)]
    families = nubBy (\a b -> ifFamInstAxiom a == ifFamInstAxiom b)
      [i | iface <- originals, i <- mi_fam_insts iface,
          exportIdentity (ifFamInstAxiom i) `elem` inventoryFamilies selected]
    actualSelected inventories = normalizeInventory $ InstanceInventory
      [record { instanceSelectedAxioms = filter (`elem` inventoryFamilies selected) (instanceSelectedAxioms record) }
        | inventory <- inventories, record <- inventoryClasses inventory,
          instanceDfun record `elem` map (exportIdentity . ifDFun) instances]
      (map (exportIdentity . ifFamInstAxiom) families)
    actualFamilies = sort (nubBy (==)
      [exportIdentity (ifFamInstAxiom i) | iface <- originals, i <- mi_fam_insts iface])
    selectExport requested = case
        [avail | avail <- originalAvails, exportIdentity (availName avail) == exportHead requested
          , all (`elem` map exportIdentity (availNames avail)) (exportChildren requested)] of
      [] -> pure (Left "requested original export is absent from the implementation interfaces")
      avail : _ -> do
        thing <- lookupType hsc (availName avail)
        let kind = case thing of
              Just (ATyCon tycon) | isClassTyCon tycon -> ClassDeclaration
              _ | isTcOcc (nameOccName (availName avail)) -> TypeDeclaration
                | otherwise -> ValueDeclaration
        pure $ if kind /= exportKind requested
          then Left "requested original export kind differs from its implementation interface"
          else Right $ case avail of
            Avail n -> Avail n
            AvailTC n ns -> AvailTC n
              [child | child <- ns, exportIdentity child == exportHead requested
                || exportIdentity child `elem` exportChildren requested]
    firstRejection (JoinRejected reason diagnostic) _ = Just (reason, diagnostic)
    firstRejection _ (JoinRejected reason diagnostic) = Just (reason, diagnostic)
    firstRejection _ _ = Nothing
    stableModules = sortBy stableModuleCmp . nubBy (==)

-- | Full axiom consistency is independent of lexical reduction visibility.
-- Call on joins and after every authored declaration, before publishing any
-- compiler receipt. Hidden class dfuns are deliberately not checked here:
-- functional-dependency improvement needs Wanted equality evidence, whereas
-- retained family axioms already carry coercions.
validateRetainedFamilyInstances :: FamInstEnv -> [FamInst] -> JoinDecision
validateRetainedFamilyInstances packages retained = validateInstances
  (InstEnvs emptyInstEnv emptyInstEnv emptyModuleSet)
  (packages, extendFamInstEnvList emptyFamInstEnv
    (uniqueFamilyInstances retained))

-- CoAxiom equality is equality of its Unique. Preserve the first original
-- instance without comparing it to every preceding retained axiom.
uniqueFamilyInstances :: [FamInst] -> [FamInst]
uniqueFamilyInstances = go emptyUniqSet
  where
    go _ [] = []
    go seen (family : rest)
      | fi_axiom family `elementOfUniqSet` seen = go seen rest
      | otherwise = family : go (addOneToUniqSet seen (fi_axiom family)) rest

normalizeExport :: DeclarationExport -> DeclarationExport
normalizeExport e = e { exportChildren = sort (exportChildren e) }

-- | Local additions only. A persisted Join reports its selected inventory;
-- authored interfaces report their original local dfuns and family axioms.
interfaceInventory :: HscEnv -> ModIface -> IO (Either String InstanceInventory)
interfaceInventory hsc iface = pure $ do
  unless (sort (map (exportIdentity . coAxiomName . fi_axiom) localFamilies)
      == sort (map (exportIdentity . ifFamInstAxiom) (mi_fam_insts iface)))
    (Left "original family inventory is incomplete after hydration")
  unless (sort (map (exportIdentity . is_dfun_name) localClasses)
      == sort (map (exportIdentity . ifDFun) (mi_insts iface)))
    (Left "original class inventory is incomplete after hydration")
  associations <- mapM associatedOwner localFamilies
  pure $ normalizeInventory $ InstanceInventory
    [ClassInstanceEvidence (exportIdentity (ifDFun instance_))
      (exportIdentity (ifInstCls instance_))
      [axiom | (axiom, Just dfun) <- associations, dfun == exportIdentity (ifDFun instance_)]
      | instance_ <- mi_insts iface]
    (map (exportIdentity . ifFamInstAxiom) (mi_fam_insts iface))
  where
    hmis = [hmi | hpt <- unitEnv_hpts (hsc_HUG hsc), hmi <- eltsHpt hpt]
    implementationClasses = nubBy (\a b -> is_dfun_name a == is_dfun_name b)
      [instance_ | hmi <- hmis, instance_ <- instEnvElts (md_insts (hm_details hmi))]
    localClasses = [instance_ | instance_ <- implementationClasses,
      exportIdentity (is_dfun_name instance_) `elem` map (exportIdentity . ifDFun) (mi_insts iface)]
    localFamilies = uniqueFamilyInstances
      [family | hmi <- hmis, family <- md_fam_insts (hm_details hmi),
      exportIdentity (coAxiomName (fi_axiom family)) `elem`
        map (exportIdentity . ifFamInstAxiom) (mi_fam_insts iface)]
    -- GHC's consistency check projects the family parameters shared with its
    -- enclosing class. Interfaces retain that structural relation, but do not
    -- retain a pointer from an associated axiom to its particular dfun.
    associatedOwner family = case tyConAssoc_maybe (coAxiomTyCon (fi_axiom family)) of
      Nothing -> Right (axiom, Nothing)
      Just parent -> case [exportIdentity (is_dfun_name instance_)
        | instance_ <- implementationClasses, is_cls_nm instance_ == tyConName parent,
          nameModule (is_dfun_name instance_) == nameModule (coAxiomName (fi_axiom family)),
          sameInstantiation instance_ family] of
        [dfun] -> Right (axiom, Just dfun)
        [] -> Left "associated axiom has no provable original class-instance owner"
        _ -> Left "associated axiom has ambiguous original class-instance owners"
      where axiom = exportIdentity (coAxiomName (fi_axiom family))
    sameInstantiation instance_ family =
      let (_, arguments, _) = etaExpandCoAxBranch (coAxiomSingleBranch (fi_axiom family))
          shared = [(is_tys instance_ !! index, argument)
            | (variable, argument) <- zip (tyConTyVars (coAxiomTyCon (fi_axiom family))) arguments,
              Just index <- [elemIndex variable (classTyVars (is_cls instance_))]]
          (classArguments, familyArguments) = unzip shared
      in isJust (tcMatchTys classArguments familyArguments)
        && isJust (tcMatchTys familyArguments classArguments)

interfaceExports :: HscEnv -> ModIface -> IO [DeclarationExport]
interfaceExports hsc = exportsFromAvails hsc . mi_exports

exportsFromAvails :: HscEnv -> [AvailInfo] -> IO [DeclarationExport]
exportsFromAvails hsc avails = forM avails $ \avail -> do
  let headName = availName avail
  thing <- lookupType hsc headName
  let kind = case thing of
        Just (ATyCon tycon) | isClassTyCon tycon -> ClassDeclaration
        _ | isTcOcc (nameOccName headName) -> TypeDeclaration
          | otherwise -> ValueDeclaration
  pure $ normalizeExport (DeclarationExport kind (exportIdentity headName)
    (map exportIdentity (filter (/= headName) (availNames avail))))

exportIdentity :: Name -> ExportIdentity
exportIdentity name = case nameModule_maybe name of
      Nothing -> error "checked declaration export has no original module"
      Just owner -> ExportIdentity (unitString (moduleUnit owner))
        (moduleNameString (moduleName owner)) (namespace name) (occNameString (nameOccName name))
        (unpackFS <$> fieldOcc_maybe (nameOccName name))
  where
    namespace n
      | isJust (fieldOcc_maybe (nameOccName n)) = FieldNamespace
      | isTcOcc (nameOccName n) = TypeNamespace
      | isDataOcc (nameOccName n) = ConstructorNamespace
      | otherwise = ValueNamespace

normalizeInventory :: InstanceInventory -> InstanceInventory
normalizeInventory (InstanceInventory classes families) = InstanceInventory
  (sort [record { instanceSelectedAxioms = sort (nubBy (==)
      (concatMap instanceSelectedAxioms (filter (sameOwner record) classes))) }
    | record <- nubBy sameOwner classes]) (sort (nubBy (==) families))
  where sameOwner a b = instanceDfun a == instanceDfun b && instanceClass a == instanceClass b

validateInstances :: InstEnvs -> FamInstEnvs -> JoinDecision
validateInstances classes families
  | not (null classConflicts) = JoinRejected ClassInstanceConflict (showSDocUnsafe (ppr classConflicts))
  | not (null familyConflicts) = JoinRejected FamilyInstanceConflict (showSDocUnsafe (ppr familyConflicts))
  | not (null injectivityConflicts) = JoinRejected FamilyInstanceConflict (showSDocUnsafe (ppr injectivityConflicts))
  | otherwise = JoinAccepted
  where
    visible = nubBy (\a b -> is_dfun_name a == is_dfun_name b) $
      filter (instIsVisible (ie_visible classes)) (instEnvElts (ie_global classes) ++ instEnvElts (ie_local classes))
    pairs = [(a,b) | a : rest <- tails visible, b <- rest, is_cls_nm a == is_cls_nm b]
    classConflicts = [(a,b) | (a,b) <- pairs, incompatible a b]
      ++ [(a,b) | a <- visible, b <- checkFunDeps classes a, is_dfun_name a /= is_dfun_name b]
    incompatible a b
      | identicalClsInstHead a b = True
      | not (isJust (tcUnifyTys instanceBindFun (is_tys a) (is_tys b))) = False
      | isIncoherent a || isIncoherent b = False
      | isJust (tcMatchTys (is_tys a) (is_tys b)) && (isOverlappable a || isOverlapping b) = False
      | isJust (tcMatchTys (is_tys b) (is_tys a)) && (isOverlappable b || isOverlapping a) = False
      | otherwise = True
    allFamilies = famInstEnvElts (fst families) ++ famInstEnvElts (snd families)
    familyConflicts = [(a,b) | a <- allFamilies
      , b <- lookupFamInstEnvConflicts families a, fi_axiom a /= fi_axiom b]
    -- GHC's injectiveBranches accepts a branch compared with itself, including
    -- polymorphic branches. Keeping self changes no conflict result; GHC's
    -- per-family lookup can reuse the complete package/home indices directly.
    injectivityConflicts = [branch | a <- allFamilies
      , Injective flags <- [tyConInjectivityInfo (famInstTyCon a)]
      , branch <- lookupFamInstEnvInjectivityConflicts flags families a]

-- | The runtime owns the ordered merge and opaque public snapshot. Compiler
-- validation certifies that manifest without deriving a second merge policy.
data ReservedJoin = ReservedJoin
  { reservedUnit :: String, reservedModule :: String, reservedPath :: FilePath
  } deriving (Eq, Show)
data DeclarationArtifact = DeclarationArtifact
  { artifactInterface :: ExactIfaceArtifact, artifactProduct :: Maybe ModuleSnapshot
  } deriving (Eq, Show)
data DeclarationWrite = DeclarationWrite
  { writeGeneration :: Word64, writeModule :: ModuleSnapshot
  , writeExports :: [DeclarationExport], writeRetractions :: [ExportIdentity]
  } deriving (Eq, Show)
data DeclarationJoinInput = DeclarationJoinInput
  { expectedPublicVersion :: String, publicModule :: Maybe ModuleSnapshot
  , privateBase :: Maybe ModuleSnapshot, privateTip :: Maybe ModuleSnapshot
  , declarationWrites :: [DeclarationWrite], joinReservation :: ReservedJoin
  , expectedExports :: [DeclarationExport], expectedInstances :: InstanceInventory
  , implementationArtifacts :: [DeclarationArtifact], retainedFamilyClosure :: [ExportIdentity]
  } deriving (Eq, Show)
data DeclarationJoinOutcome = DeclarationJoinOutcome
  { outcomeInput :: DeclarationJoinInput, outcomeArtifact :: Maybe ModuleSnapshot
  , outcomeDecision :: JoinDecision
  } deriving (Eq, Show)

readDeclarationJoin :: FilePath -> IO DeclarationJoinInput
readDeclarationJoin path = do
  size <- getFileSize path
  when (size > 4 * 1024 * 1024) (fail "declaration join exceeds four MiB")
  bytes <- BS.readFile path
  case deserialiseFromBytes decodeDeclarationJoin (BL.fromStrict bytes) of
    Left failure -> fail (show failure)
    Right (remaining, input) -> do
      unless (BL.null remaining && encodeDeclarationJoin input == bytes)
        (fail "declaration join must be canonical CBOR with no trailing bytes")
      pure input

decodeDeclarationJoin :: Decoder s DeclarationJoinInput
decodeDeclarationJoin = do
  array 12
  magic <- text
  version <- text
  unless (magic == "TPDJOIN" && version == "3") (fail "unsupported declaration join")
  DeclarationJoinInput <$> text <*> optional snapshot <*> optional snapshot
    <*> optional snapshot <*> vector write <*> reservation <*> vector declarationExport
    <*> inventory <*> vector artifact <*> vector identity
  where
    snapshot = do
      array 3
      value <- ModuleSnapshot <$> nonempty <*> absolutePath <*> digest
      pure value
    reservation = array 3 >> ReservedJoin <$> nonempty <*> nonempty <*> absolutePath
    write = array 4 >> DeclarationWrite <$> decodeWord64 <*> snapshot
      <*> vector declarationExport <*> vector identity
    declarationExport = array 3 >> DeclarationExport <$> kind <*> identity <*> vector identity
    inventory = array 2 >> InstanceInventory <$> vector classInstance <*> vector identity
    classInstance = array 3 >> ClassInstanceEvidence <$> identity <*> identity <*> vector identity
    identity = array 5 >> ExportIdentity <$> nonempty <*> nonempty <*> namespace
      <*> nonempty <*> optional text
    namespace = text >>= \case
      "value" -> pure ValueNamespace
      "type" -> pure TypeNamespace
      "constructor" -> pure ConstructorNamespace
      "field" -> pure FieldNamespace
      _ -> fail "invalid declaration export namespace"
    kind = text >>= \case
      "value" -> pure ValueDeclaration
      "type" -> pure TypeDeclaration
      "class" -> pure ClassDeclaration
      _ -> fail "invalid declaration export kind"
    artifact = array 2 >> DeclarationArtifact <$> exact <*> optional snapshot
    exact = array 5 >> ExactIfaceArtifact <$> nonempty <*> nonempty <*> absolutePath
      <*> digest <*> vector (array 2 >> (,) <$> nonempty <*> nonempty)
    nonempty = text >>= \value -> if null value then fail "empty declaration identity" else pure value
    absolutePath = text >>= \value -> if isAbsolute value then pure value else fail "relative declaration artifact path"
    digest = text >>= \value -> if length value == 64 && all (`elem` ['0'..'9'] ++ ['a'..'f']) value
      then pure value else fail "invalid declaration artifact SHA256"

array :: Int -> Decoder s ()
array expected = decodeListLen >>= \count -> unless (count == expected) (fail "invalid join field count")
text :: Decoder s String
text = T.unpack <$> decodeString
vector :: Decoder s a -> Decoder s [a]
vector item = do
  count <- decodeListLen
  when (count > 16384) (fail "too many declaration join items")
  replicateM count item
optional :: Decoder s a -> Decoder s (Maybe a)
optional item = peekTokenType >>= \case
  TypeNull -> decodeNull >> pure Nothing
  _ -> Just <$> item

encodeDeclarationJoin :: DeclarationJoinInput -> BS.ByteString
encodeDeclarationJoin input = toStrictByteString $
  encodeListLen 12 <> wireText "TPDJOIN" <> wireText "3" <> wireText (expectedPublicVersion input)
  <> wireOptional wireSnapshot (publicModule input) <> wireOptional wireSnapshot (privateBase input)
  <> wireOptional wireSnapshot (privateTip input) <> wireList wireWrite (declarationWrites input)
  <> wireReservation (joinReservation input) <> wireList wireExport (expectedExports input)
  <> wireInventory (expectedInstances input) <> wireList wireArtifact (implementationArtifacts input)
  <> wireList wireIdentity (retainedFamilyClosure input)

wireText :: String -> Encoding
wireText = encodeString . T.pack
wireList :: (a -> Encoding) -> [a] -> Encoding
wireList f values = encodeListLen (fromIntegral (length values)) <> foldMap f values
wireOptional :: (a -> Encoding) -> Maybe a -> Encoding
wireOptional = maybe encodeNull
wireSnapshot :: ModuleSnapshot -> Encoding
wireSnapshot value = encodeListLen 3 <> foldMap wireText
  [snapshotModule value, snapshotPath value, snapshotSha256 value]
wireReservation :: ReservedJoin -> Encoding
wireReservation value = encodeListLen 3 <> foldMap wireText
  [reservedUnit value, reservedModule value, reservedPath value]
wireIdentity :: ExportIdentity -> Encoding
wireIdentity value = encodeListLen 5 <> foldMap wireText
  [exportUnit value, exportModule value, namespaceWire (exportNamespace value), exportOccurrence value]
  <> wireOptional wireText (exportRecordParent value)
wireExport :: DeclarationExport -> Encoding
wireExport value = encodeListLen 3 <> wireText (kindWire (exportKind value))
  <> wireIdentity (exportHead value) <> wireList wireIdentity (exportChildren value)
wireInventory :: InstanceInventory -> Encoding
wireInventory value = encodeListLen 2 <> wireList wireClass (inventoryClasses value)
  <> wireList wireIdentity (inventoryFamilies value)
  where wireClass record = encodeListLen 3 <> wireIdentity (instanceDfun record)
          <> wireIdentity (instanceClass record) <> wireList wireIdentity (instanceSelectedAxioms record)
wireWrite :: DeclarationWrite -> Encoding
wireWrite value = encodeListLen 4 <> encodeWord64 (writeGeneration value)
  <> wireSnapshot (writeModule value) <> wireList wireExport (writeExports value)
  <> wireList wireIdentity (writeRetractions value)
wireArtifact :: DeclarationArtifact -> Encoding
wireArtifact value = encodeListLen 2 <> exact (artifactInterface value)
  <> wireOptional wireSnapshot (artifactProduct value)
  where
    exact iface = encodeListLen 5 <> foldMap wireText
      [exactUnit iface, exactModule iface, exactPath iface, exactSha256 iface]
      <> wireList (\(unit, name) -> encodeListLen 2 <> wireText unit <> wireText name) (exactRequirements iface)
namespaceWire :: ExportNamespace -> String
namespaceWire = \case
  ValueNamespace -> "value"
  TypeNamespace -> "type"
  ConstructorNamespace -> "constructor"
  FieldNamespace -> "field"
kindWire :: DeclarationKind -> String
kindWire = \case
  ValueDeclaration -> "value"
  TypeDeclaration -> "type"
  ClassDeclaration -> "class"

-- | Use a fresh compiler transaction and exact home interfaces. Package
-- evidence follows the ordinary pinned package policy. Infrastructure failures
-- propagate to the worker handler; semantic rejection is a successful receipt.
validateDeclarationJoin :: HscEnv -> DeclarationJoinInput -> IO DeclarationJoinOutcome
validateDeclarationJoin initial input = do
  timing <- readTimingEnabled
  let reject reason diagnostic = pure (DeclarationJoinOutcome input Nothing (JoinRejected reason diagnostic))
      artifacts = implementationArtifacts input
      exacts = map artifactInterface artifacts
      reservation = joinReservation input
      anchors = catMaybes [publicModule input, privateBase input, privateTip input]
        ++ map writeModule (declarationWrites input)
      anchorPresent anchor = any (\iface -> snapshotModule anchor == exactModule iface
          && snapshotPath anchor == exactPath iface && snapshotSha256 anchor == exactSha256 iface) exacts
  if not (all anchorPresent anchors)
    then reject ArtifactChanged "a provenance anchor is absent from the exact interface closure"
    else do
      unchanged <- measureJoin timing "revalidate_before" rnf (artifactsUnchanged artifacts)
      if not unchanged then reject ArtifactChanged "implementation artifact bytes changed" else do
        fresh <- timeDetailPhase timing "declaration_join" "reset" (freshExactState initial)
        loaded <- readExactIfaceArtifacts fresh exacts
        case loaded of
          Left diagnostic -> reject ArtifactChanged diagnostic
          Right verified -> do
            hydrated <- hydrateExactScope fresh verified
            writeChecks <- measureJoin timing "write_delta" (rnf . and) $ forM (declarationWrites input) $ \write -> do
              let original = [iface | (_, iface) <- verified,
                    moduleNameString (moduleName (mi_module iface)) == snapshotModule (writeModule write)]
              case original of
                [iface] -> do
                  exported <- interfaceExports hydrated iface
                  let local = filter (\entry -> exportUnit (exportHead entry) == unitString (moduleUnit (mi_module iface))
                        && exportModule (exportHead entry) == snapshotModule (writeModule write)) exported
                  pure (sort (map normalizeExport local) == sort (map normalizeExport (writeExports write)))
                _ -> pure False
            packageRoots <- joinPackageRoots hydrated artifacts
            case (and writeChecks, packageRoots) of
              (False, _) -> reject ExportMismatch "authored write delta differs from its exact original interface"
              (_, Left diagnostic) -> reject ArtifactChanged diagnostic
              (True, Right roots) -> do
                let joined = mkModule (stringToUnit (reservedUnit reservation)) (mkModuleName (reservedModule reservation))
                result <- timeDetailPhase timing "declaration_join" "build_interface" $
                  buildJoinedInterface hydrated joined (reservedPath reservation)
                  (map snd verified) (expectedExports input) (expectedInstances input) (retainedFamilyClosure input)
                case result of
                  Left (reason, diagnostic) -> reject reason diagnostic
                  Right output -> do
                    let exact = ExactIfaceArtifact (reservedUnit reservation) (reservedModule reservation)
                          (snapshotPath output) (snapshotSha256 output) []
                    timeDetailPhase timing "declaration_join" "seal_packages" $
                      sealPackageImports (snapshotPath output ++ ".packages") exact roots
                    unchangedAfter <- measureJoin timing "revalidate_after" rnf (artifactsUnchanged artifacts)
                    rootsAfter <- joinPackageRoots hydrated artifacts
                    if not unchangedAfter || rootsAfter /= Right roots
                      then reject ArtifactChanged "implementation artifacts changed during validation"
                      else pure (DeclarationJoinOutcome input (Just output) JoinAccepted)

-- Advisory interface-only inspection can operate without authored package
-- seals. The protected Rust certification front door additionally requires
-- every input seal to match its owned bytes before and after this request.
joinPackageRoots :: HscEnv -> [DeclarationArtifact] -> IO (Either String PackageImportEvidence)
joinPackageRoots env artifacts = do
  captured <- forM artifacts $ \artifact -> do
    let exact = artifactInterface artifact
        path = exactPath exact ++ ".packages"
    exists <- doesFileExist path
    if not exists then pure (Right emptyPackageImports) else do
      bytes <- BS.readFile path
      readPackageImports path (sha256 bytes) exact
  case sequence captured of
    Left diagnostic -> pure (Left diagnostic)
    Right entries -> do
      let roots = sort (nubBy (==) (concatMap packageInterfaces entries))
          provided = sort (nubBy (==) (concatMap compilerProvided entries))
          sameOwner a b = packageUnit a == packageUnit b && packageModule a == packageModule b
      if length (nubBy sameOwner roots) /= length roots
        then pure (Left "package witness owner has differing exact selections")
        else do
          validated <- mapM (validatePackageImportRoot env) roots
          pure (PackageImportEvidence roots provided <$ sequence validated)

artifactsUnchanged :: [DeclarationArtifact] -> IO Bool
artifactsUnchanged artifacts = and <$> forM artifacts (\artifact -> do
  let iface = artifactInterface artifact
  interfaceHash <- sha256 <$> BS.readFile (exactPath iface)
  productMatches <- case artifactProduct artifact of
    Nothing -> pure True
    Just compiledProduct -> do
      hash <- sha256 <$> BS.readFile (snapshotPath compiledProduct)
      pure (snapshotModule compiledProduct == exactModule iface && hash == snapshotSha256 compiledProduct)
  pure (interfaceHash == exactSha256 iface && productMatches))

-- | Every outcome is bound to the same exact request, including semantic
-- rejection. Runtime alone compares the echoed paired snapshot for staleness.
renderDeclarationJoinOutcome :: DeclarationJoinOutcome -> String
renderDeclarationJoinOutcome outcome = object
  [ ("version", "3"), ("expected_public_version", jsonString (expectedPublicVersion input))
  , ("request_sha256", jsonString (sha256 (encodeDeclarationJoin input)))
  , ("reserved", object [("unit", jsonString (reservedUnit reserved)), ("module", jsonString (reservedModule reserved))
      , ("path", jsonString (reservedPath reserved))])
  , ("implementation_sha256", proof (wireList wireArtifact (implementationArtifacts input)))
  , ("exports_sha256", proof (wireList wireExport (expectedExports input)))
  , ("instances_sha256", proof (wireInventory (expectedInstances input)))
  , ("family_closure_sha256", proof (wireList wireIdentity (retainedFamilyClosure input)))
  , ("artifact", maybe "null" snapshotJson (outcomeArtifact outcome))
  , ("decision", case outcomeDecision outcome of
      JoinAccepted -> object [("status", jsonString "accepted")]
      JoinRejected reason diagnostic -> object [("status", jsonString "rejected")
        , ("reason", jsonString (reasonWire reason)), ("diagnostic", jsonString diagnostic)])
  ]
  where
    input = outcomeInput outcome
    reserved = joinReservation input
    proof = jsonString . sha256 . toStrictByteString
    object pairs = "{" ++ intercalate "," [jsonString key ++ ":" ++ value | (key, value) <- pairs] ++ "}"
    snapshotJson snapshot = object [("module", jsonString (snapshotModule snapshot))
      , ("path", jsonString (snapshotPath snapshot)), ("sha256", jsonString (snapshotSha256 snapshot))]
    reasonWire = \case
      ArtifactChanged -> "artifact_changed"
      ExportMismatch -> "export_mismatch"
      ClassInstanceConflict -> "class_instance_conflict"
      FamilyInstanceConflict -> "family_instance_conflict"
      InstanceMismatch -> "instance_mismatch"
      Unprovable -> "unprovable"

-- | The inventory query is the sole bridge from original GHC Names into the
-- runtime merge. Authored text and unqualified export names are insufficient.
data DeclarationInventory = DeclarationInventory
  { inventoryArtifact :: DeclarationArtifact, inventoryExports :: [DeclarationExport]
  , inventoryInstances :: InstanceInventory
  } deriving (Eq, Show)
data BindingInterfacePurpose = HostBuilt | OriginalLiveInput BS.ByteString
  deriving (Eq, Show)

encodeBindingInterfacePurpose :: BindingInterfacePurpose -> Encoding
encodeBindingInterfacePurpose HostBuilt = encodeListLen 1 <> wireText "host-built"
encodeBindingInterfacePurpose (OriginalLiveInput witness) =
  case validateCheckedTypeWitnessBytes witness of
    Left diagnostic -> error diagnostic
    Right () -> encodeListLen 2 <> wireText "original-live-input" <> encodeBytes witness

data HostBindingInterfaceInput = HostBindingInterfaceInput
  { hostInterfaceProducer :: String, hostInterfaceAdmission :: String
  , hostInterfaceGeneration :: Word64, hostInterfaceBinder :: String
  , hostInterfaceSignature :: CheckedSignature, hostInterfaceScope :: FilePath
  , hostInterfaceRoot :: FilePath, hostInterfacePurpose :: BindingInterfacePurpose
  } deriving (Eq, Show)

encodeHostBindingInterface :: HostBindingInterfaceInput -> BS.ByteString
encodeHostBindingInterface input = toStrictByteString $
  encodeListLen 10 <> wireText "TPHOSTBINDINGINTERFACE" <> wireText "2"
  <> wireText (hostInterfaceProducer input) <> wireText (hostInterfaceAdmission input)
  <> encodeWord64 (hostInterfaceGeneration input) <> wireText (hostInterfaceBinder input)
  <> encodeCheckedSignature (hostInterfaceSignature input)
  <> wireText (hostInterfaceScope input) <> wireText (hostInterfaceRoot input)
  <> encodeBindingInterfacePurpose (hostInterfacePurpose input)

data DeclarationOperation = InspectInventory [DeclarationArtifact] | ValidateJoin DeclarationJoinInput
  | EmitHostBindingInterface HostBindingInterfaceInput
  deriving (Eq, Show)
data DeclarationInventoryOutcome = DeclarationInventoryOutcome
  { inspectedArtifacts :: [DeclarationArtifact]
  , inspectionResult :: Either (JoinRejection, String) [DeclarationInventory]
  } deriving (Eq, Show)

readDeclarationOperation :: FilePath -> IO DeclarationOperation
readDeclarationOperation path = do
  size <- getFileSize path
  when (size > 4 * 1024 * 1024) (fail "declaration operation exceeds four MiB")
  bytes <- BS.readFile path
  case deserialiseFromBytes (decodeListLen >> text) (BL.fromStrict bytes) of
    Left failure -> fail (show failure)
    Right (_, "TPDJOIN") -> ValidateJoin <$> readDeclarationJoin path
    Right (_, "TPHOSTBINDINGINTERFACE") -> case deserialiseFromBytes decodeHostInterface (BL.fromStrict bytes) of
      Left failure -> fail (show failure)
      Right (remaining, input) -> do
        unless (BL.null remaining && encodeHostBindingInterface input == bytes)
          (fail "host interface operation must be canonical CBOR with no trailing bytes")
        pure (EmitHostBindingInterface input)
    Right (_, "TPDINVENTORY") -> case deserialiseFromBytes decodeInventory (BL.fromStrict bytes) of
      Left failure -> fail (show failure)
      Right (remaining, artifacts) -> do
        unless (BL.null remaining && encodeDeclarationInventory artifacts == bytes)
          (fail "inventory must be canonical CBOR with no trailing bytes")
        pure (InspectInventory artifacts)
    _ -> fail "unsupported declaration operation"
  where
    decodeHostInterface = do
      array 10
      magic <- text
      version <- text
      unless (magic == "TPHOSTBINDINGINTERFACE" && version == "2")
        (fail "unsupported host interface operation")
      producer <- digest
      admission <- digest
      generation <- decodeWord64
      unless (generation > 0) (fail "host interface generation must be nonzero")
      HostBindingInterfaceInput producer admission generation <$> nonempty
        <*> decodeCheckedSignature <*> absolutePath <*> absolutePath <*> purpose
    purpose = do
      count <- decodeListLen
      tag <- text
      case (count, tag) of
        (1, "host-built") -> pure HostBuilt
        (2, "original-live-input") -> do
          witness <- decodeBytes
          either fail pure (validateCheckedTypeWitnessBytes witness)
          pure (OriginalLiveInput witness)
        _ -> fail "unsupported host interface purpose"
    decodeInventory = do
      array 3
      magic <- text
      version <- text
      unless (magic == "TPDINVENTORY" && version == "3") (fail "unsupported declaration inventory")
      vector artifact
    artifact = array 2 >> DeclarationArtifact <$> exact <*> optional snapshot
    exact = array 5 >> ExactIfaceArtifact <$> nonempty <*> nonempty <*> absolutePath
      <*> digest <*> vector (array 2 >> (,) <$> nonempty <*> nonempty)
    snapshot = array 3 >> ModuleSnapshot <$> nonempty <*> absolutePath <*> digest
    nonempty = text >>= \value -> if null value then fail "empty declaration identity" else pure value
    absolutePath = text >>= \value -> if isAbsolute value then pure value else fail "relative declaration artifact path"
    digest = text >>= \value -> if length value == 64 && all (`elem` ['0'..'9'] ++ ['a'..'f']) value
      then pure value else fail "invalid declaration artifact SHA256"

encodeDeclarationInventory :: [DeclarationArtifact] -> BS.ByteString
encodeDeclarationInventory artifacts = toStrictByteString $
  encodeListLen 3 <> wireText "TPDINVENTORY" <> wireText "3" <> wireList wireArtifact artifacts

inspectDeclarationArtifacts :: HscEnv -> [DeclarationArtifact] -> IO DeclarationInventoryOutcome
inspectDeclarationArtifacts initial artifacts = do
  timing <- readTimingEnabled
  let rejected diagnostic = pure (DeclarationInventoryOutcome artifacts (Left (ArtifactChanged, diagnostic)))
  unchanged <- measureJoin timing "revalidate_before" rnf (artifactsUnchanged artifacts)
  if not unchanged then rejected "implementation artifact bytes changed" else do
    fresh <- timeDetailPhase timing "declaration_join" "reset" (freshExactState initial)
    loaded <- readExactIfaceArtifacts fresh (map artifactInterface artifacts)
    case loaded of
      Left diagnostic -> rejected diagnostic
      Right verified -> do
        hydrated <- hydrateExactScope fresh verified
        inventories <- measureJoin timing "inspection_inventories"
          (forceEither (foldr (\row rest ->
            foldr (\exported next -> forceExport exported `seq` next)
              (forceInventory (inventoryInstances row) `seq` rest) (inventoryExports row)) ()) . sequence) $
          forM (zip artifacts verified) $ \(artifact, (_, iface)) -> do
            exports <- interfaceExports hydrated iface
            fmap (DeclarationInventory artifact exports) <$> interfaceInventory hydrated iface
        unchangedAfter <- measureJoin timing "revalidate_after" rnf (artifactsUnchanged artifacts)
        if not unchangedAfter then rejected "implementation artifacts changed during inspection"
          else pure (DeclarationInventoryOutcome artifacts
            (case sequence inventories of
              Left diagnostic -> Left (Unprovable, diagnostic)
              Right values -> Right values))

renderDeclarationInventoryOutcome :: DeclarationInventoryOutcome -> String
renderDeclarationInventoryOutcome outcome = jsonObject
  [ ("version", "3")
  , ("request_sha256", jsonString (sha256 (encodeDeclarationInventory artifacts)))
  , ("implementation_sha256", jsonString (sha256 (toStrictByteString (wireList wireArtifact artifacts))))
  , ("inventories", either (const "null") (jsonArray . map inventoryJson) result)
  , ("family_closure", either (const "null") (jsonArray . map identityJson . familyInventory) result)
  , ("decision", case result of
      Right _ -> jsonObject [("status", jsonString "accepted")]
      Left (reason, diagnostic) -> jsonObject [("status", jsonString "rejected")
        , ("reason", jsonString (case reason of ArtifactChanged -> "artifact_changed"; _ -> "unprovable"))
        , ("diagnostic", jsonString diagnostic)])
  ]
  where
    artifacts = inspectedArtifacts outcome
    result = inspectionResult outcome
    familyInventory = sort . nubBy (==) . concatMap (inventoryFamilies . inventoryInstances)
    inventoryJson inventory = jsonObject
      [ ("artifact", artifactJson (inventoryArtifact inventory))
      , ("exports", jsonArray (map exportJson (inventoryExports inventory)))
      , ("instances", instancesJson (inventoryInstances inventory)) ]

-- The same-offer original declaration receipt uses the same identity,
-- instance and retained-family encoding as source-free artifact inspection.
renderDeclarationSelection :: [DeclarationExport] -> InstanceInventory -> [ExportIdentity] -> String
renderDeclarationSelection exports instances families = jsonObject
  [ ("exports", jsonArray (map exportJson exports))
  , ("instances", instancesJson instances)
  , ("family_closure", jsonArray (map identityJson families))
  ]

jsonObject :: [(String, String)] -> String
jsonObject pairs = "{" ++ intercalate "," [jsonString key ++ ":" ++ value | (key, value) <- pairs] ++ "}"
jsonArray :: [String] -> String
jsonArray values = "[" ++ intercalate "," values ++ "]"
identityJson :: ExportIdentity -> String
identityJson identity = jsonObject [("unit", jsonString (exportUnit identity)), ("module", jsonString (exportModule identity))
  , ("namespace", jsonString (namespaceWire (exportNamespace identity))), ("occurrence", jsonString (exportOccurrence identity))
  , ("record_parent", maybe "null" jsonString (exportRecordParent identity))]
exportJson :: DeclarationExport -> String
exportJson value = jsonObject [("kind", jsonString (kindWire (exportKind value))), ("head", identityJson (exportHead value))
  , ("children", jsonArray (map identityJson (exportChildren value)))]
instancesJson :: InstanceInventory -> String
instancesJson value = jsonObject [("classes", jsonArray (map classJson (inventoryClasses value)))
  , ("families", jsonArray (map identityJson (inventoryFamilies value)))]
  where classJson record = jsonObject [("dfun", identityJson (instanceDfun record))
          , ("class", identityJson (instanceClass record))
          , ("selected_axioms", jsonArray (map identityJson (instanceSelectedAxioms record)))]
artifactJson :: DeclarationArtifact -> String
artifactJson artifact = jsonObject [("interface", jsonObject
  [("unit", jsonString (exactUnit iface)), ("module", jsonString (exactModule iface)), ("path", jsonString (exactPath iface))
  , ("sha256", jsonString (exactSha256 iface)), ("requirements", jsonArray
      [jsonArray [jsonString unit, jsonString name] | (unit, name) <- exactRequirements iface])])
  , ("product", maybe "null" (\value -> jsonObject [("module", jsonString (snapshotModule value))
      , ("path", jsonString (snapshotPath value)), ("sha256", jsonString (snapshotSha256 value))]) (artifactProduct artifact))]
  where iface = artifactInterface artifact
