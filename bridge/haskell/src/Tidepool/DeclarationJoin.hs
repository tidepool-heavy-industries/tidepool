-- | Persisted declaration interfaces preserve original Names and implementation
-- dependencies while contributing only the selected lexical instance inventory.
module Tidepool.DeclarationJoin
  ( ModuleSnapshot(..), ExportNamespace(..), DeclarationKind(..), ExportIdentity(..), DeclarationExport(..)
  , InstanceInventory(..), JoinDecision(..), JoinRejection(..)
  , buildJoinedInterface, validateRetainedFamilyInstances
  , exportIdentity, interfaceExports, interfaceInventory
  ) where

import Control.Exception (bracket)
import Control.Monad (forM)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.List (nubBy, sort, sortBy, tails)
import Data.Maybe (isJust)
import GHC.Core.FamInstEnv
import GHC.Core.InstEnv
import GHC.Core.TyCon (isClassTyCon, tyConInjectivityInfo, Injectivity(..))
import GHC.Core.Coercion.Axiom (coAxiomName)
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
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit, stableModuleCmp)
import GHC.Unit.Module.Env (mkModuleSet, emptyModuleSet)
import GHC.Unit.Module.Deps (Dependencies(..), noDependencies)
import GHC.Unit.Module.ModIface
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt)
import GHC.Unit.Env (unitEnv_hpts)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Types (unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Numeric (showHex)
import System.Directory (removeFile)
import System.FilePath (takeDirectory)
import System.IO (openBinaryTempFile, hClose)
import System.Posix.Files (createLink)

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

data InstanceInventory = InstanceInventory
  { inventoryClasses :: [ExportIdentity], inventoryFamilies :: [ExportIdentity]
  } deriving (Eq, Show)

data JoinRejection = ArtifactChanged | ExportMismatch | ClassInstanceConflict
  | FamilyInstanceConflict | InstanceMismatch | Unprovable
  deriving (Eq, Show)
data JoinDecision = JoinAccepted | JoinRejected JoinRejection String
  deriving (Eq, Show)

sha256 :: BS.ByteString -> String
sha256 = concatMap (\byte -> let digits = showHex byte "" in replicate (2 - length digits) '0' ++ digits)
  . BS.unpack . SHA.hash

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
  exportChoices <- mapM selectExport exports
  case sequence exportChoices of
    Left diagnostic -> pure (Left (ExportMismatch, diagnostic))
    Right avails
      | any ((== joined) . mi_module) originals ->
          pure (Left (ArtifactChanged, "reserved Join module already exists in implementation closure"))
      | normalizeInventory selected /= actualSelected ->
          pure (Left (InstanceMismatch, "selected dfun or axiom is absent from the exact implementation closure"))
      | sort (nubBy (==) familyClosure) /= actualFamilies ->
          pure (Left (InstanceMismatch, "retained family inventory differs from the exact implementation closure"))
      | otherwise -> do
          let hmis = [hmi | hpt <- unitEnv_hpts (hsc_HUG hsc), hmi <- eltsHpt hpt]
              allFamilies = concatMap (md_fam_insts . hm_details) hmis
              selectedClassNames = inventoryClasses selected
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
          _ <- initIfaceCheck (ppr joined) hsc $
            mapM (loadSysInterface (ppr joined)) (stableModules (orphanOwners ++ familyOwners))
          eps <- hscEPS hsc
          let classes = InstEnvs (eps_inst_env eps)
                  (mkInstEnv (nubBy (\a b -> is_dfun_name a == is_dfun_name b) selectedClasses))
                  (mkModuleSet orphanOwners)
              selectedEnv = (emptyFamInstEnv, extendFamInstEnvList emptyFamInstEnv selectedFamilies)
              consistency = validateInstances classes selectedEnv
              familyConsistency = validateRetainedFamilyInstances (eps_fam_inst_env eps) allFamilies
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
              iface <- addFingerprints hsc partial
              -- Hard-link publication is exclusive: an existing immutable Join
              -- artifact is never replaced, including a retry's old output.
              digest <- bracket
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
      [i | iface <- originals, i <- mi_insts iface, exportIdentity (ifDFun i) `elem` inventoryClasses selected]
    families = nubBy (\a b -> ifFamInstAxiom a == ifFamInstAxiom b)
      [i | iface <- originals, i <- mi_fam_insts iface,
          exportIdentity (ifFamInstAxiom i) `elem` inventoryFamilies selected]
    actualSelected = normalizeInventory $ InstanceInventory
      (map (exportIdentity . ifDFun) instances) (map (exportIdentity . ifFamInstAxiom) families)
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
    (nubBy (\a b -> fi_axiom a == fi_axiom b) retained))

normalizeExport :: DeclarationExport -> DeclarationExport
normalizeExport e = e { exportChildren = sort (exportChildren e) }

-- | Local additions only. A persisted Join reports its selected inventory;
-- authored interfaces report their original local dfuns and family axioms.
interfaceInventory :: ModIface -> InstanceInventory
interfaceInventory iface = normalizeInventory $ InstanceInventory
  (map (exportIdentity . ifDFun) (mi_insts iface))
  (map (exportIdentity . ifFamInstAxiom) (mi_fam_insts iface))

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
normalizeInventory (InstanceInventory classes families) = InstanceInventory (sort (nubBy (==) classes)) (sort (nubBy (==) families))

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
    injectivityConflicts = [branch | a <- allFamilies
      , Injective flags <- [tyConInjectivityInfo (famInstTyCon a)]
      , let others = extendFamInstEnvList emptyFamInstEnv
              (filter (\b -> fi_axiom a /= fi_axiom b) allFamilies)
      , branch <- lookupFamInstEnvInjectivityConflicts flags (emptyFamInstEnv, others) a]
