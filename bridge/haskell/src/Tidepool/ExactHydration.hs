{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TypeApplications #-}

module Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, newOriginalInterfaceArtifacts, newOriginalInterfaceArtifactsWithSessionCaptures
  , originalInterfaceBytes, originalInterfaceSha256, originalSessionInterfaces, serializeOriginalInterface
  , ExactIfaceArtifact(..)
  , freshExactState, freshExactContext, forkExactContext
  , PackageFinderFacts, newPackageFinderFacts, forkExactContextWithPackageFacts
  , ExactContextForkFailure(..)
  , readExactIfaceArtifacts
  , hydrateExactScope, hydrateOriginalInterfaces, exactInterfaceSummary
  , exactHomeInstancesFor, withExactHomeInstances
  , CheckedValueImportAuthority
  , noCheckedValueImports
  , readCheckedValueImportAuthority
  , VerifiedExactIfaceClosure
  , readVerifiedExactIfaceClosure
  , readVerifiedExactIfaceClosureWithCheckedValues
  , selectVerifiedExactInterfaces
  , selectVerifiedValueInterfaces
  , checkedValueImportAuthorityFromVerified
  , CheckedTemplateInterface(..)
  , CheckedTemplateImports(..)
  , GeneratedScaffoldRecipe, generatedScaffoldRecipe, generatedCheckingTemplateRecipe
  , generatedActivationPreviewRecipe, captureGeneratedScaffoldTarget
  , permitsGeneratedScaffoldImport
  , GeneratedScaffoldImportAuthority, noGeneratedScaffoldImports, readGeneratedScaffoldImportAuthority
  , installExactLexicalGraphWithScaffold
  , installExactLexicalGraph
  ) where

import Tidepool.Timing (readTimingEnabled, emitCount, timeDetailPhase)
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString, CapturedSessionInterface, capturedSessionInterface, capturedSessionInterfaceEvidence)
import Control.Monad (forM, forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.List (mapAccumL)
import Control.Exception
  ( Exception, IOException, SomeException, SomeAsyncException, bracket, try, fromException, throwIO )
import Data.Char (isHexDigit, toLower)
import Data.Maybe (isJust, isNothing, catMaybes)
import qualified Data.ByteString as BS
import qualified Crypto.Hash.SHA256 as SHA256
import GHC.Driver.Env
  ( HscEnv(..), hscUpdateHPT_lazy, hptSomeThingsBelowUs, hsc_home_unit, hsc_home_unit_maybe, hsc_HPT, hscEPS, discardIC, hsc_all_home_unit_ids )
import GHC.Driver.Plugins
  ( Plugin(..), PluginWithArgs(..), Plugins(..), StaticPlugin(..), PluginRecompile(..), defaultPlugin )
import GHC.Tc.Types (TcGblEnv(..), ImportAvails(..))
import GHC.Tc.Utils.Monad (getTopEnv)
import qualified GHC.Linker.Loader as Linker
import GHC.Unit.Env (UnitEnv(..), HomeUnitEnv(..))
import GHC.Unit.External (ExternalUnitCache(..), initExternalUnitCache, ExternalPackageState(eps_PIT))
import GHC.Unit.Module.Env (lookupModuleEnv, moduleEnvToList, filterModuleEnv)
import GHC.Unit.Finder (initFinderCache)
import GHC.Unit.Finder.Types (FinderCache(..))
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Driver.Env.KnotVars (emptyKnotVars)
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), emptyHomeModInfoLinkable, emptyHomePackageTable, addToHpt
  , lookupHpt )
import GHC.Iface.Load (readIface)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Driver.Session (targetProfile, ghcMode, GhcMode(CompManager))
import Data.IORef (IORef, newIORef, readIORef, modifyIORef', atomicModifyIORef')
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Unit.Module (Module, ModuleName, moduleName, moduleUnit, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.Graph
  ( ModuleGraph, ModuleGraphNode(..), NodeKey(..), ModNodeKeyWithUid(..)
  , mgModSummaries', mkModuleGraph )
import GHC.Unit.Module.Location
  ( pattern ModLocation
  , ml_hs_file, ml_hi_file, ml_dyn_hi_file, ml_obj_file, ml_dyn_obj_file, ml_hie_file )
import GHC.Unit.Module.ModSummary (ModSummary(..))
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.PkgQual (PkgQual(..), RawPkgQual(..))
import GHC.Types.SrcLoc (Located, unLoc, getLoc, SrcSpan(..), srcSpanStartLine)
import GHC.Types.Avail (availNames)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import Tidepool.ExecutionSource (ExecutionSourceIdentity(..))
import Tidepool.DependencyEvidence (sourceEvidenceWithFingerprint, dependencySourceSha256)
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import GHC (Ghc, ParsedModule(..), getSession, setSession)
import GHC.Driver.Monad (reifyGhc, reflectGhc)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Core.InstEnv (instEnvElts, is_dfun_name, mkInstEnv)
import GHC.Core.FamInstEnv (fi_axiom)
import GHC.Core.Coercion.Axiom (coAxiomName)
import GHC.Parser.Annotation (getLocA)
import Language.Haskell.Syntax (HsModule(..))
import GHC.Hs (ImportDecl(..), ImportDeclQualifiedStyle(..))
import GHC.Unit.Module.Deps (dep_orphs, dep_finsts)
import GHC.Unit.Home (homeUnitAsUnit, homeUnitId, isHomeUnit, isHomeUnitDefinite)
import GHC.Unit.Types (GenWithIsBoot(..), InstalledModuleWithIsBoot, UnitId)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Utils.Fingerprint (fingerprint0, fingerprintByteString, fingerprintString)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_extra_decls, mi_exports, mi_insts, mi_fam_insts, mi_deps, mi_iface_hash, mi_orphan, mi_final_exts, mi_decls)
import GHC.Builtin.Names (gHC_PRIM)
import Tidepool.FatIface (readExactInterface)
import GHC.Unit.Types (unitString, stringToUnit, toUnitId)
import Tidepool.FinalizedModule (FinalizedModule(..))
import qualified GHC.Data.Maybe as MErr
import GHC.Utils.Outputable (text)
import Numeric (showHex)
import System.Directory (getTemporaryDirectory, removeFile, canonicalizePath)
import GHC.Fingerprint.Type (Fingerprint)
import System.IO (fixIO)
import System.IO (hClose, hIsClosed, openBinaryTempFile)
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map

-- An artifact is advisory until its bytes and GHC module identity have been
-- checked in the transaction that consumes it. Requirements cover the exact
-- implementation closure and are checked by the caller before hydration.
data ExactIfaceArtifact = ExactIfaceArtifact
  { exactUnit :: String
  , exactModule :: String
  , exactPath :: FilePath
  , exactSha256 :: String
  , exactRequirements :: [(String, String)]
  } deriving (Eq, Show)

-- Checked value modules are injected in dependency order after the lexical
-- graph is installed. Their verified identities authorize source imports at
-- that earlier boundary without exposing implementation-only originals.
newtype CheckedValueImportAuthority = CheckedValueImportAuthority (Set.Set (String, String))

newtype VerifiedExactIfaceClosure = VerifiedExactIfaceClosure
  (Map.Map (String, String) (ExactIfaceArtifact, ModIface, [ExactIfaceArtifact]))

readVerifiedExactIfaceClosure
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosure env artifacts = fmap (VerifiedExactIfaceClosure . Map.fromList
  . map (\(artifact,iface) -> ((exactUnit artifact,exactModule artifact),(artifact,iface,[artifact]))))
  <$> readExactIfaceArtifacts env artifacts

readVerifiedExactIfaceClosureWithCheckedValues
  :: HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosureWithCheckedValues env originals values
  | any (not . checkedValueOwner) values = pure (Left "checked value import has another owner")
  | length (map key values) /= Set.size (Set.fromList (map key values)) =
      pure (Left "duplicate checked value owner")
  | otherwise = do
      let originalKeys = Set.fromList (map key originals)
          inputs = originals ++ [value | value <- values, key value `Set.notMember` originalKeys]
      verified <- readVerifiedExactIfaceClosure env inputs
      case verified of
        Left reason -> pure (Left reason)
        Right (VerifiedExactIfaceClosure captured) -> do
          aliases <- forM values $ \value -> case Map.lookup (key value) captured of
            Just (original,iface,known)
              | value `elem` known -> pure (Right (key value,(original,iface,known)))
              | exactSha256 value == exactSha256 original
              , exactRequirements value == [] || exactRequirements value == exactRequirements original -> do
                  -- The closed original supplies the type dependency proof.
                  -- Verify this protected value's separate capture before its
                  -- path becomes an alias for those exact original bytes.
                  alias <- readOne env value
                  pure ((\_ -> (key value,(original,iface,value:known))) <$> alias)
            _ -> pure (Left "checked value alias conflicts with its captured original")
          pure $ do
            additions <- sequence aliases
            Right (VerifiedExactIfaceClosure (Map.union (Map.fromList additions) captured))
  where key artifact = (exactUnit artifact,exactModule artifact)

-- Requirements are validated in the complete captured closure. A later
-- dependency-ordered injection may select one value whose type mentions
-- another original without rereading or weakening that original proof.
selectVerifiedExactInterfaces
  :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact]
  -> Either String [(ExactIfaceArtifact, ModIface)]
selectVerifiedExactInterfaces (VerifiedExactIfaceClosure captured) artifacts =
  mapM select artifacts
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (original,iface,_) | original == artifact -> Right (original,iface)
      _ -> Left "requested exact interface differs from its verified closure"

-- Completed-value wire inputs bind bytes and selected exported Names. Their
-- type requirements come from this complete verified closure, including
-- worker-issued values created earlier in the same compiled cell.
selectVerifiedValueInterfaces
  :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact]
  -> Either String [(ExactIfaceArtifact, ModIface)]
selectVerifiedValueInterfaces (VerifiedExactIfaceClosure captured) artifacts = mapM select artifacts
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (original,iface,aliases)
        | checkedValueOwner artifact
        , any (\alias -> exactPath alias == exactPath artifact
            && exactSha256 alias == exactSha256 artifact) aliases
        , exactRequirements artifact == [] || exactRequirements artifact == exactRequirements original ->
            Right (original,iface)
      _ -> Left "completed value input differs from its verified closure"

checkedValueImportAuthorityFromVerified
  :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact]
  -> Either String CheckedValueImportAuthority
checkedValueImportAuthorityFromVerified closure artifacts
  | any (not . checkedValueOwner) artifacts = Left "checked value import has another owner"
  | otherwise = CheckedValueImportAuthority . Set.fromList
      . map (\(artifact, _) -> (exactUnit artifact, exactModule artifact))
      <$> selectVerifiedValueInterfaces closure artifacts

checkedValueOwner :: ExactIfaceArtifact -> Bool
checkedValueOwner artifact = exactUnit artifact == "main" && case parseSessionModule (exactModule artifact) of
  Just owner -> smKind owner == ValMod && sessionModuleString owner == exactModule artifact
  Nothing -> False

noCheckedValueImports :: CheckedValueImportAuthority
noCheckedValueImports = CheckedValueImportAuthority Set.empty

-- A protected turn producer records the compiler's import before authored
-- bytes are inserted. The resulting capability belongs to one rendered
-- target, not to the support module's name or to the surrounding scope.
data CheckedTemplateInterface = CheckedTemplateInterface
  { templateInterfaceUnit :: String
  , templateInterfaceModule :: String
  , templateInterfaceSha256 :: String
  , templateInterfaceImports :: [(String,String)]
  } deriving (Eq, Show)

-- Direct roots authorize imports written in a protected template. The graph
-- supplies only the sealed transitive support needed by those roots.
data CheckedTemplateImports = CheckedTemplateImports
  { checkedTemplateRoots :: [(String,String)]
  , checkedTemplateGraph :: [CheckedTemplateInterface]
  } deriving (Eq, Show)

data GeneratedScaffoldRecipe = GeneratedScaffoldRecipe FilePath String BS.ByteString Int
  [(CheckedTemplateInterface,Int,Maybe String)] [CheckedTemplateInterface]
  GeneratedScaffoldInstanceScope GeneratedScaffoldPurpose
  deriving (Eq)

data GeneratedScaffoldInstanceScope = ImportedTemplateInstances | OriginalPreviewInstances CheckedTemplateInterface
  deriving (Eq)

data GeneratedScaffoldPurpose = NativeTurnTemplate | CheckingCellTemplate
  deriving (Eq, Show)

instance Show GeneratedScaffoldRecipe where
  show (GeneratedScaffoldRecipe path name _ line interfaces _ _ role) =
    "GeneratedScaffoldRecipe " ++ show (path,name,line,interfaces,role)

generatedScaffoldRecipe :: CheckedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipe = generatedScaffoldRecipeFor NativeTurnTemplate

generatedCheckingTemplateRecipe :: CheckedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedCheckingTemplateRecipe = generatedScaffoldRecipeFor CheckingCellTemplate

generatedScaffoldRecipeFor :: GeneratedScaffoldPurpose -> CheckedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipeFor purpose (CheckedTemplateImports roots interfaces) protectedTemplate rendered path name = do
  canonical <- canonicalizePath path
  let compilerImport = "import qualified Tidepool.Internal.Resume as TidepoolResume"
      occurrences text' = [line | (line,textLine) <- zip [1..] (lines text'), textLine == compilerImport]
      bytes = TextEncoding.encodeUtf8 (Text.pack rendered)
      templateOccurrence interface =
        let owner = templateInterfaceModule interface
            importShape textLine | textLine == compilerImport = Nothing
            importShape textLine = case words textLine of
              ["import", name] | name == owner -> Just Nothing
              ["import", "qualified", name, "as", alias] | name == owner -> Just (Just alias)
              _ -> Nothing
            matching text' = [(line,shape) | (line,textLine) <- zip [1..] (lines text')
              , Just shape <- [importShape textLine]]
        in case (matching protectedTemplate,matching rendered) of
          ([],_) -> Right Nothing
          ([(_,shape)],[(line,renderedShape)]) | shape == renderedShape -> Right (Just (interface,line,shape))
          _ -> Left "checked template interface import is missing, duplicated, or changed"
  pure $ do
    let rootSet = Set.fromList roots
        graphOwners = [(templateInterfaceUnit interface,templateInterfaceModule interface) | interface <- interfaces]
    unless (length roots == Set.size rootSet && all (`elem` graphOwners) roots)
      (Left "checked template roots are duplicated or leave their sealed graph")
    selected <- catMaybes <$> mapM templateOccurrence
      [interface | interface <- interfaces
      , (templateInterfaceUnit interface,templateInterfaceModule interface) `Set.member` rootSet]
    let key interface = (templateInterfaceUnit interface,templateInterfaceModule interface)
        graph = Map.fromList [(key interface,interface) | interface <- interfaces]
        reachable seen [] = Right (Map.elems seen)
        reachable seen (owner:rest)
          | Map.member owner seen = reachable seen rest
          | otherwise = do
              interface <- maybe (Left "checked template graph is incomplete") Right (Map.lookup owner graph)
              reachable (Map.insert owner interface seen) (templateInterfaceImports interface ++ rest)
    unless (Map.size graph == length interfaces) (Left "duplicate checked template interface")
    closure <- reachable Map.empty [key interface | (interface,_,_) <- selected]
    case (purpose,occurrences protectedTemplate,occurrences rendered) of
      (NativeTurnTemplate,[_],[line]) -> Right (GeneratedScaffoldRecipe canonical name bytes line selected closure ImportedTemplateInstances purpose)
      (CheckingCellTemplate,[],[]) -> Right (GeneratedScaffoldRecipe canonical name bytes 0 selected closure ImportedTemplateInstances purpose)
      _ -> Left "generated scaffold support import differs from its template role"

-- Only the activation admission supplies this complete original instance
-- graph. Its synthetic edges affect GHC's instance traversal without exposing
-- any original lexical name or permitting an authored source import.
generatedActivationPreviewRecipe :: [CheckedTemplateInterface] -> (String,String) -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedActivationPreviewRecipe interfaces originalOwner protectedTemplate rendered path name = do
  let roots = [(templateInterfaceUnit interface,templateInterfaceModule interface) | interface <- interfaces]
      imports = CheckedTemplateImports roots interfaces
  recipe <- generatedScaffoldRecipe imports protectedTemplate rendered path name
  pure $ do
    original <- recipe
    let GeneratedScaffoldRecipe canonical target bytes line selected _ _ role = original
        owners = Set.fromList [(templateInterfaceUnit interface,templateInterfaceModule interface)
          | interface <- interfaces]
    unless (all (all (`Set.member` owners) . templateInterfaceImports) interfaces)
      (Left "activation preview edge leaves its original instance graph")
    originalTarget <- case [interface | interface <- interfaces
        , (templateInterfaceUnit interface,templateInterfaceModule interface) == originalOwner] of
      [interface] -> Right interface
      _ -> Left "activation preview lacks its sealed original target"
    pure (GeneratedScaffoldRecipe canonical target bytes line selected interfaces (OriginalPreviewInstances originalTarget) role)

captureGeneratedScaffoldTarget :: GeneratedScaffoldRecipe -> FilePath -> IO (Either String BS.ByteString)
captureGeneratedScaffoldTarget (GeneratedScaffoldRecipe path _ expected _ _ _ _ _) requested = do
  canonical <- canonicalizePath requested
  actual <- BS.readFile canonical
  pure $ if canonical == path && actual == expected then Right expected
    else Left "generated scaffold target differs from its protected recipe"

data GeneratedScaffoldOwner
  = NativeScaffoldOwner ExecutionSourceIdentity
  | TemplateInterfaceOwner

-- Interface-only template edges never enter the native/source replay inventory.
data GeneratedScaffoldImportAuthority = GeneratedScaffoldImportAuthority
  [(Module,Fingerprint,GeneratedScaffoldOwner,SrcSpan,ExactIfaceArtifact)]
  [(ExactIfaceArtifact,[(String,String)])]
  [(Module,Fingerprint,[(String,String)])]
  [OriginalPreviewOrphans]

data OriginalPreviewOrphans = OriginalPreviewOrphans Module Fingerprint [Module]

noGeneratedScaffoldImports :: GeneratedScaffoldImportAuthority
noGeneratedScaffoldImports = GeneratedScaffoldImportAuthority [] [] [] []

permitsGeneratedScaffoldImport
  :: GeneratedScaffoldImportAuthority -> ModSummary -> (String,String)
  -> (PkgQual, Located ModuleName) -> Bool
permitsGeneratedScaffoldImport (GeneratedScaffoldImportAuthority scaffold _ _ _) summary requested (qualifier,imported) =
  qualifier == NoPkgQual && any
    (\(target,fingerprint,owner,span',artifact) -> ms_mod summary == target && ms_hs_hash summary == fingerprint
      && requested == (case owner of
          NativeScaffoldOwner native -> (executionUnit native,executionModule native)
          TemplateInterfaceOwner -> (exactUnit artifact,exactModule artifact))
      && getLoc imported == span') scaffold

readGeneratedScaffoldImportAuthority :: VerifiedExactIfaceClosure -> [ExecutionSourceIdentity]
  -> Maybe ((String,String),String) -> GeneratedScaffoldRecipe -> ParsedModule -> ModuleGraph -> HscEnv
  -> IO (Either String GeneratedScaffoldImportAuthority)
readGeneratedScaffoldImportAuthority (VerifiedExactIfaceClosure captured) nativeOwners planned
    recipe@(GeneratedScaffoldRecipe path target expected line templateInterfaces templateGraph instanceScope purpose) parsed sourceGraph env = do
  checked <- captureGeneratedScaffoldTarget recipe path
  (source,fingerprint) <- sourceEvidenceWithFingerprint path
  targetPaths <- forM [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph
      , moduleNameString (moduleName (ms_mod summary)) == target] $ \summary ->
    traverse canonicalizePath (ml_hs_file (ms_location summary))
  pure $ do
    _ <- checked
    unless (dependencySourceSha256 source == hexBytes (SHA256.hash expected))
      (Left "generated scaffold source changed during admission")
    summary <- case [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph
        , moduleNameString (moduleName (ms_mod summary)) == target] of
      [summary] | ms_hs_hash summary == fingerprint && targetPaths == [Just path]
          && ms_mod summary == mkModule (stringToUnit (unitString (homeUnitId (hsc_home_unit env)))) (mkModuleName target)
          && ms_mod (pm_mod_summary parsed) == ms_mod summary
          && ms_hs_hash (pm_mod_summary parsed) == fingerprint -> Right summary
      _ -> Left "generated scaffold target summary differs from its protected source"
    let support = mkModule (stringToUnit (unitString (homeUnitId (hsc_home_unit env))))
          (mkModuleName "Tidepool.Internal.Resume")
        key = (unitString (moduleUnit support),moduleNameString (moduleName support))
    resume <- if purpose == CheckingCellTemplate
      then Right noGeneratedScaffoldImports
      else if any (\case ModuleNode _ loaded -> ms_mod loaded == support; _ -> False)
        (mgModSummaries' sourceGraph)
      then Right noGeneratedScaffoldImports
      else do
        (artifact,iface,_) <- maybe (Left "generated scaffold lacks its verified support interface") Right
          (Map.lookup key captured)
        native <- case [owner | owner <- nativeOwners,
            (executionUnit owner,executionModule owner) == key
            && executionIfaceSha256 owner == exactSha256 artifact] of
          [owner] -> Right owner
          _ -> Left "generated scaffold lacks one paired original native owner"
        unless (mi_module iface == support)
          (Left "generated scaffold lacks a paired original native owner")
        let hiddenHomeWitnesses = filter (isHomeUnit (hsc_home_unit env) . moduleUnit)
              (dep_orphs (mi_deps iface) ++ dep_finsts (mi_deps iface))
        -- Same-owner requirements preserve custody of native references between
        -- groups. They introduce no additional interface or lexical owner.
        unless (all (== key) (exactRequirements artifact))
          (Left "generated scaffold support requires another home implementation owner")
        unless (null (mi_insts iface))
          (Left "generated scaffold support defines class instances")
        unless (null (mi_fam_insts iface))
          (Left "generated scaffold support defines family instances")
        unless (null hiddenHomeWitnesses)
          (Left "generated scaffold support imports home orphan or family witnesses")
        let exports = concatMap availNames (mi_exports iface)
        unless (all (\occurrence -> any (\name -> nameModule_maybe name == Just support
            && occNameString (nameOccName name) == occurrence) exports) ["settle","resumeLifted"])
          (Left "generated scaffold support has another export owner")
        imported <- case [name | (NoPkgQual,name) <- ms_textual_imps summary
            , unLoc name == moduleName support
            , case getLoc name of RealSrcSpan span' _ -> srcSpanStartLine span' == line; _ -> False] of
          [name] -> Right name
          _ -> Left "generated scaffold import occurrence differs from its protected recipe"
        declaration <- case [unLoc located | located <- hsmodImports (unLoc (pm_parsed_source parsed))
            , getLocA (ideclName (unLoc located)) == getLoc imported] of
          [declaration] -> Right declaration
          _ -> Left "generated scaffold parsed import differs from its captured occurrence"
        unless (ideclQualified declaration == QualifiedPre
            && fmap unLoc (ideclAs declaration) == Just (mkModuleName "TidepoolResume")
            && ideclSource declaration == NotBoot && ideclImportList declaration == Nothing
            && case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
          (Left "generated scaffold parsed import differs from its protected shape")
        pure (GeneratedScaffoldImportAuthority [(ms_mod summary,fingerprint,NativeScaffoldOwner native,getLoc imported,artifact)] [(artifact,[])] [] [])
    originals <- case planned of
      Nothing -> Right noGeneratedScaffoldImports
      Just (owner,expectedFingerprint) -> do
        let original = mkModule (stringToUnit (fst owner)) (mkModuleName (snd owner))
            importLine = "import " ++ snd owner
            occurrences = [number | (number,textLine) <- zip [1..] (lines (Text.unpack (TextEncoding.decodeUtf8 expected)))
              , textLine == importLine]
        unless (moduleUnit original == homeUnitAsUnit (hsc_home_unit env))
          (Left "checked recipe original belongs to another home unit")
        unless (not (any (\case ModuleNode _ loaded -> ms_mod loaded == original; _ -> False)
          (mgModSummaries' sourceGraph)))
          (Left "checked recipe original collides with fresh source")
        (artifact,iface,_) <- maybe (Left "checked recipe lacks its verified original interface") Right
          (Map.lookup owner captured)
        unless (mi_module iface == original
          && show (mi_iface_hash (mi_final_exts iface)) == expectedFingerprint)
          (Left "checked recipe original fingerprint differs")
        native <- case [candidate | candidate <- nativeOwners
            , (executionUnit candidate,executionModule candidate) == owner
            && executionIfaceSha256 candidate == exactSha256 artifact] of
          [candidate] -> Right candidate
          _ -> Left "checked recipe lacks its paired original native owner"
        originalLine <- case occurrences of
          [number] -> Right number
          _ -> Left "checked recipe original import is missing or duplicated"
        imported <- case [name | (NoPkgQual,name) <- ms_textual_imps summary
            , unLoc name == moduleName original
            , case getLoc name of RealSrcSpan span' _ -> srcSpanStartLine span' == originalLine; _ -> False] of
          [name] -> Right name
          _ -> Left "checked recipe original import occurrence differs"
        declaration <- case [unLoc located | located <- hsmodImports (unLoc (pm_parsed_source parsed))
            , getLocA (ideclName (unLoc located)) == getLoc imported] of
          [declaration] -> Right declaration
          _ -> Left "checked recipe original parsed import differs"
        unless (ideclQualified declaration == NotQualified && isNothing (ideclAs declaration)
          && ideclSource declaration == NotBoot && ideclImportList declaration == Nothing
          && case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
          (Left "checked recipe original import has another shape")
        -- Original instances and families belong to this sealed declaration;
        -- only the protected recipe edge receives visibility.
        pure (GeneratedScaffoldImportAuthority [(ms_mod summary,fingerprint,NativeScaffoldOwner native,getLoc imported,artifact)] [(artifact,[])] [] [])
    templateNodes <- forM templateGraph $ \selected -> do
      let owner = (templateInterfaceUnit selected,templateInterfaceModule selected)
      (artifact,_,_) <- maybe (Left "checked template graph interface is not retained") Right (Map.lookup owner captured)
      unless (exactSha256 artifact == templateInterfaceSha256 selected)
        (Left "checked template graph interface seal changed")
      pure (artifact,templateInterfaceImports selected)
    templateEdges <- forM templateInterfaces $ \(selected,importLine,qualification) -> do
      let owner = (templateInterfaceUnit selected,templateInterfaceModule selected)
          original = mkModule (stringToUnit (fst owner)) (mkModuleName (snd owner))
      unless (moduleUnit original == homeUnitAsUnit (hsc_home_unit env)
          && not (any (\case ModuleNode _ loaded -> ms_mod loaded == original; _ -> False)
            (mgModSummaries' sourceGraph)))
        (Left "checked template interface conflicts with current source")
      (artifact,iface,_) <- maybe (Left "checked template interface is not retained") Right
        (Map.lookup owner captured)
      unless (exactSha256 artifact == templateInterfaceSha256 selected && mi_module iface == original)
        (Left "checked template interface seal changed")
      imported <- case [name | (NoPkgQual,name) <- ms_textual_imps summary
          , unLoc name == moduleName original
          , case getLoc name of RealSrcSpan span' _ -> srcSpanStartLine span' == importLine; _ -> False] of
        [name] -> Right name
        _ -> Left "checked template interface import occurrence differs"
      declaration <- case [unLoc located | located <- hsmodImports (unLoc (pm_parsed_source parsed))
          , getLocA (ideclName (unLoc located)) == getLoc imported] of
        [declaration] -> Right declaration
        _ -> Left "checked template parsed import differs"
      let sameQualification = case qualification of
            Nothing -> ideclQualified declaration == NotQualified && isNothing (ideclAs declaration)
            Just alias -> ideclQualified declaration == QualifiedPre
              && fmap unLoc (ideclAs declaration) == Just (mkModuleName alias)
      unless (sameQualification && ideclSource declaration == NotBoot && ideclImportList declaration == Nothing
          && case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
        (Left "checked template interface import has another shape")
      pure (ms_mod summary,fingerprint,TemplateInterfaceOwner,getLoc imported,artifact)
    orphanScopes <- case instanceScope of
      ImportedTemplateInstances -> pure []
      OriginalPreviewInstances originalTarget -> do
        let owner = (templateInterfaceUnit originalTarget,templateInterfaceModule originalTarget)
        (artifact,iface,_) <- maybe (Left "activation preview original target interface is not retained") Right
          (Map.lookup owner captured)
        unless (exactSha256 artifact == templateInterfaceSha256 originalTarget
            && (unitString (moduleUnit (mi_module iface)),moduleNameString (moduleName (mi_module iface))) == owner)
          (Left "activation preview original target interface seal changed")
        let orphans = Set.toAscList (Set.fromList
              ([mi_module iface | mi_orphan (mi_final_exts iface)] ++ dep_orphs (mi_deps iface)))
        forM_ (filter (isHomeUnit (hsc_home_unit env) . moduleUnit) orphans) $ \orphan ->
          unless ((unitString (moduleUnit orphan),moduleNameString (moduleName orphan))
              `Map.member` captured)
            (Left "activation preview original orphan interface is not retained")
        pure [OriginalPreviewOrphans (ms_mod summary) fingerprint orphans]
    let GeneratedScaffoldImportAuthority resumeEdges resumeNodes _ _ = resume
        GeneratedScaffoldImportAuthority originalEdges originalNodes _ _ = originals
        instanceEdges = case instanceScope of
          ImportedTemplateInstances -> []
          OriginalPreviewInstances _ -> [(ms_mod summary,fingerprint,
            [(exactUnit artifact,exactModule artifact) | (artifact,_) <- templateNodes])]
    pure (GeneratedScaffoldImportAuthority (resumeEdges ++ originalEdges ++ templateEdges)
      (resumeNodes ++ originalNodes ++ templateNodes) instanceEdges orphanScopes)

readCheckedValueImportAuthority
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String CheckedValueImportAuthority)
readCheckedValueImportAuthority env artifacts
  | any (not . checkedValueOwner) artifacts = pure (Left "checked value import has another owner")
  | otherwise = do
      verified <- readVerifiedExactIfaceClosure env artifacts
      pure (verified >>= (`checkedValueImportAuthorityFromVerified` artifacts))

-- A legacy standalone reset also retires executable home symbols. Resident
-- context selection keeps interpreter transition under its single owner.
freshExactState :: HscEnv -> IO HscEnv
freshExactState env = do
  forM_ (hsc_interp env) $ \interp -> Linker.unload interp env []
  freshExactContext env

-- | Allocate resolution cells independently of every retained context.
-- Name interning and the interpreter remain worker-owned.
freshExactContext :: HscEnv -> IO HscEnv
freshExactContext env = do
  timing <- readTimingEnabled
  timeDetailPhase timing "exact_scope" "fresh_state" $ do
    eps <- initExternalUnitCache
    finder <- initFinderCache
    let cleared = discardIC (withoutPreviewOrphanPlugin env)
        units = hsc_unit_env cleared
        homes = fmap (\home -> home { homeUnitEnv_hpt = emptyHomePackageTable })
          (ue_home_unit_graph units)
    pure cleared
      { hsc_FC = finder
      , hsc_targets = []
      , hsc_mod_graph = mkModuleGraph []
      , hsc_type_env_vars = emptyKnotVars
      , hsc_unit_env = units { ue_eps = eps, ue_home_unit_graph = homes }
      }

-- One compiler universe owns package locations, including unsuccessful searches.
-- This cache is independent of every attempt's finder, so a miss never walks
-- completed environments. Its home inventory cannot become package authority.
data PackageFinderFacts = PackageFinderFacts (Set.Set UnitId) FinderCache

newPackageFinderFacts :: HscEnv -> IO PackageFinderFacts
newPackageFinderFacts env = PackageFinderFacts (hsc_all_home_unit_ids env)
  <$> initFinderCache

-- Standalone callers have no retained universe and share no finder facts.
forkExactContext :: HscEnv -> IO HscEnv
forkExactContext env = do
  packages <- newPackageFinderFacts env
  forkExactContextWithPackageFacts packages env

data ExactContextForkFailure
  = ExactContextRequiresCompilationManager
  | ExactContextRequiresSingleDefiniteHomeUnit
  | ExactContextContainsHomePackageInterface
  deriving (Eq, Show)

instance Exception ExactContextForkFailure

-- Lazy HMI details capture their original HscEnv. Copying its EPS cell would
-- let those details load package declarations/instances into the old cell,
-- leaving the selected attempt with types but without their instances. Keep
-- that package owner for the fixed compiler universe; HPT, home locations and
-- file hashes still belong to the selected attempt.
--
-- GHC's CompManager loader rejects successful reads of the active home unit
-- into EPS. Its failed reads nevertheless cache empty home interfaces in PIT.
-- Remove only these negative rows before a new attempt. OneShot, multiple home
-- units and Backpack require different ownership and cannot use this boundary.
forkExactContextWithPackageFacts :: PackageFinderFacts -> HscEnv -> IO HscEnv
forkExactContextWithPackageFacts (PackageFinderFacts packageHomes packages) env = do
  unless (ghcMode (hsc_dflags env) == CompManager) $
    throwIO ExactContextRequiresCompilationManager
  let units = hsc_unit_env env
      actualHomes = hsc_all_home_unit_ids env
      homeUnits = Set.union packageHomes actualHomes
      activeHome = ue_current_unit units
      eps = ue_eps units
  -- hsc_home_unit_maybe still requires the active HUG entry to exist.
  unless (actualHomes == Set.singleton activeHome && homeUnits == actualHomes) $
    throwIO ExactContextRequiresSingleDefiniteHomeUnit
  unless (case hsc_home_unit_maybe env of
      Just home -> isHomeUnitDefinite home && homeUnitId home == activeHome
      Nothing -> False) $
    throwIO ExactContextRequiresSingleDefiniteHomeUnit
  cleared <- atomicModifyIORef' (euc_eps eps) $ \external ->
    let homeOwner owner = toUnitId (moduleUnit owner) `Set.member` homeUnits
        negative iface = mi_iface_hash (mi_final_exts iface) == fingerprint0
          && null (mi_decls iface) && null (mi_exports iface)
          && null (mi_insts iface) && null (mi_fam_insts iface)
        unexpected = any (\(owner,iface) -> homeOwner owner && not (negative iface))
          (moduleEnvToList (eps_PIT external))
    in if unexpected then (external,False)
       else (external {eps_PIT = filterModuleEnv (\owner _ -> not (homeOwner owner))
                     (eps_PIT external)},True)
  unless cleared (throwIO ExactContextContainsHomePackageInterface)
  localFinder <- initFinderCache
  let selected :: InstalledModuleWithIsBoot -> FinderCache
      selected (GWIB owner _)
        | moduleUnit owner `Set.member` homeUnits = localFinder
        | otherwise = packages
      finder = localFinder
        { lookupFinderCache = \key -> lookupFinderCache (selected key) key
        , addToFinderCache = \key -> addToFinderCache (selected key) key
        }
  pure (discardIC (withoutPreviewOrphanPlugin env))
    { hsc_FC = finder, hsc_targets = [], hsc_type_env_vars = emptyKnotVars
    , hsc_unit_env = (hsc_unit_env env) { ue_eps = eps }
    }

-- Reading every interface before installing any of them prevents a corrupt
-- member of an implementation SCC from partially mutating the HPT.
readExactIfaceArtifacts
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String [(ExactIfaceArtifact, ModIface)])
readExactIfaceArtifacts env artifacts
  | Set.size moduleNames /= length artifacts = pure (Left "duplicate exact interface owner")
  | any (\artifact -> length (exactSha256 artifact) /= 64
      || not (all isHexDigit (exactSha256 artifact))) artifacts =
      pure (Left "invalid exact interface digest")
  | any (\artifact -> any (`Set.notMember` owners) (exactRequirements artifact)) artifacts =
      pure (Left "incomplete exact interface dependency closure")
  | otherwise = do
      timing <- readTimingEnabled
      timeDetailPhase timing "exact_scope" "verify_interfaces" $
        sequence <$> forM artifacts (readOne env)
  where
    moduleNames = Set.fromList (map exactModule artifacts)
    owners = Set.fromList [(exactUnit artifact, exactModule artifact) | artifact <- artifacts]

readOne :: HscEnv -> ExactIfaceArtifact -> IO (Either String (ExactIfaceArtifact, ModIface))
readOne env artifact = do
  timing <- readTimingEnabled
  emitCount timing ("exact_iface_read_calls." ++ exactModule artifact) 1
  readResult <- try (BS.readFile (exactPath artifact)) :: IO (Either IOException BS.ByteString)
  case readResult of
    Left _ -> pure (Left ("interface unavailable: " ++ exactModule artifact))
    Right bytes -> do
      emitCount timing ("exact_iface_read_bytes." ++ exactModule artifact) (fromIntegral (BS.length bytes))
      readVerified timing bytes
  where
   readVerified timing bytes =
    if hexBytes (SHA256.hash bytes) /= map toLower (exactSha256 artifact)
    then pure (Left ("interface digest mismatch: " ++ exactModule artifact))
    else do
      -- Decode the exact bytes that passed the digest check. Reading the
      -- candidate path again would admit a different interface between the
      -- hash and GHC's decoder, even if a later revalidation observed the
      -- original bytes restored.
      let owner = mkModule (stringToUnit (exactUnit artifact))
            (mkModuleName (exactModule artifact))
      decoded <- try @SomeException (withCapturedIface timing (exactModule artifact) bytes $ \path -> do
        emitCount timing ("exact_iface_decode_reads." ++ exactModule artifact) 1
        readIface (hsc_dflags env) (hsc_NC env) owner path)
      case decoded of
        Left failure -> case fromException failure :: Maybe SomeAsyncException of
          Just async -> throwIO async
          Nothing -> pure (Left ("interface read failed: " ++ exactModule artifact))
        Right result -> pure $ case result of
          MErr.Failed _ -> Left ("interface read failed: " ++ exactModule artifact)
          MErr.Succeeded iface
            | unitString (moduleUnit (mi_module iface)) /= exactUnit artifact
                || moduleNameString (moduleName (mi_module iface)) /= exactModule artifact ->
                Left ("interface owner mismatch: " ++ exactModule artifact)
            | isJust (mi_extra_decls iface) ->
                Left ("interface contains defining Core: " ++ exactModule artifact)
            | otherwise -> Right (artifact, iface)

data OriginalInterfaceCaptureFailure
  = OriginalInterfaceChanged String String
  | ConflictingOriginalInterfaces
  | FinalizedOriginalConflict String String
  deriving Show

instance Exception OriginalInterfaceCaptureFailure

-- The one serializer used for finalization certification and candidate reuse.
-- The temporary handle must be closed before GHC opens the same path.
serializeOriginalInterface :: HscEnv -> FilePath -> ModIface -> IO BS.ByteString
serializeOriginalInterface env directory interface =
  bracket (openBinaryTempFile directory "module-product.hi")
    (\(path, handle) -> do
      closed <- hIsClosed handle
      unless closed (hClose handle)
      removeFile path)
    (\(path, handle) -> do
      hClose handle
      writeBinIface (targetProfile (hsc_dflags env)) QuietBinIFace NormalCompression path interface
      BS.readFile path)

-- Original product publication and type witnesses share the same serializer
-- and immutable bytes. This cache belongs only to the completed transaction;
-- it neither consults source nor survives in a worker-global map.
data OriginalInterfaceArtifacts = OriginalInterfaceArtifacts
  HscEnv (Map.Map Module FinalizedModule) (Map.Map Module BS.ByteString) [CapturedSessionInterface] FilePath
  (IORef (Map.Map Module (Maybe (BS.ByteString, String))))

-- Only finalization products and explicit admitted captures can supply a home
-- owner. A checking HPT entry is provisional and cannot issue original bytes.
newOriginalInterfaceArtifacts :: HscEnv -> Map.Map ModuleName FinalizedModule
  -> [ExactIfaceArtifact] -> FilePath -> IO OriginalInterfaceArtifacts
newOriginalInterfaceArtifacts env finalized retained =
  newOriginalInterfaceArtifactsWithSessionCaptures env finalized retained []

-- | Selected session snapshots supply type-dependency seals alongside exact
-- captures. They do not enter the finalized-source or executable inventory.
newOriginalInterfaceArtifactsWithSessionCaptures :: HscEnv -> Map.Map ModuleName FinalizedModule
  -> [ExactIfaceArtifact] -> [CapturedSessionInterface] -> FilePath -> IO OriginalInterfaceArtifacts
newOriginalInterfaceArtifactsWithSessionCaptures env finalized retained injected directory = do
  captures <- forM retained $ \artifact -> do
    bytes <- BS.readFile (exactPath artifact)
    unless (hexBytes (SHA256.hash bytes) == exactSha256 artifact) $
      throwIO (OriginalInterfaceChanged (exactUnit artifact) (exactModule artifact))
    let owner = mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
    pure (owner, bytes)
  let originals = Map.fromList
        [(mi_module (hm_iface (finalizedHomeModInfo original)), original)
        | original <- Map.elems finalized]
      selected = captures ++ map capturedSessionInterface injected
      admitted = Map.fromList selected
  unless (all (\(owner,bytes) -> Map.lookup owner admitted == Just bytes) selected) $
    throwIO ConflictingOriginalInterfaces
  let snapshots = Map.fromList [(owner,snapshot) | snapshot <- injected
        , let (owner,_) = capturedSessionInterface snapshot]
  unless (all (\snapshot -> let (owner,bytes) = capturedSessionInterface snapshot
          in case Map.lookup owner snapshots of
            Just selectedSnapshot -> capturedSessionInterface selectedSnapshot == (owner,bytes)
              && capturedSessionInterfaceEvidence selectedSnapshot == capturedSessionInterfaceEvidence snapshot
            Nothing -> False) injected) $ throwIO ConflictingOriginalInterfaces
  OriginalInterfaceArtifacts env originals admitted (Map.elems snapshots) directory <$> newIORef Map.empty

originalSessionInterfaces :: OriginalInterfaceArtifacts -> [CapturedSessionInterface]
originalSessionInterfaces (OriginalInterfaceArtifacts _ _ _ selected _ _) = selected

originalInterfaceBytes :: OriginalInterfaceArtifacts -> Module -> IO (Maybe BS.ByteString)
originalInterfaceBytes artifacts owner = fmap fst <$> originalInterfaceArtifact artifacts owner

originalInterfaceSha256 :: OriginalInterfaceArtifacts -> Module -> IO (Maybe String)
originalInterfaceSha256 artifacts owner = fmap snd <$> originalInterfaceArtifact artifacts owner

originalInterfaceArtifact :: OriginalInterfaceArtifacts -> Module
  -> IO (Maybe (BS.ByteString, String))
originalInterfaceArtifact (OriginalInterfaceArtifacts env originals retained _ directory captured) owner = do
  known <- Map.lookup owner <$> readIORef captured
  case known of
    Just artifact -> pure artifact
    Nothing -> do
      external <- hscEPS env
      let matches interface = if mi_module interface == owner then Just interface else Nothing
          productInterface = Map.lookup owner originals >>= matches . hm_iface . finalizedHomeModInfo
          packageInterface = lookupModuleEnv (eps_PIT external) owner >>= matches
      artifact <- case productInterface of
        Just interface -> do
          finalized <- serialize interface
          case (Map.lookup owner retained, finalized) of
            (Just admitted, Just (bytes,_)) | admitted /= bytes ->
              throwIO (FinalizedOriginalConflict (unitString (moduleUnit owner)) (moduleNameString (moduleName owner)))
            _ -> pure finalized
        Nothing -> case Map.lookup owner retained of
          Just bytes -> pure (seal bytes)
          Nothing | toUnitId (moduleUnit owner) `Set.member` hsc_all_home_unit_ids env -> pure Nothing
          Nothing -> packageArtifact packageInterface
      modifyIORef' captured (Map.insert owner artifact)
      pure artifact
  where
    seal bytes = Just (bytes, hexBytes (SHA256.hash bytes))
    serialize interface = seal <$> serializeOriginalInterface env directory interface
    sameOriginal selected actual = mi_module actual == owner
      && mi_iface_hash (mi_final_exts actual) == mi_iface_hash (mi_final_exts selected)
    packageArtifact selected
      | owner /= gHC_PRIM, Nothing <- selected = pure Nothing
      | otherwise = do
          full <- readExactInterface env owner
          case full of
            Right (interface, location)
              | mi_module interface == owner
              , maybe (owner == gHC_PRIM) (`sameOriginal` interface) selected ->
                  if owner == gHC_PRIM then serialize interface else do
                    -- EPS interfaces contain panic-elided declarations. Read
                    -- the installed artifact through the original owner and
                    -- decode the same captured bytes before sealing them.
                    capturedBytes <- try @IOException (BS.readFile (ml_hi_file location))
                    case capturedBytes of
                      Left _ -> pure Nothing
                      Right bytes -> do
                        decoded <- withCapturedIface False (moduleNameString (moduleName owner)) bytes $
                          readIface (hsc_dflags env) (hsc_NC env) owner
                        pure $ case decoded of
                          MErr.Succeeded original
                            | sameOriginal interface original -> seal bytes
                          _ -> Nothing
            _ -> pure Nothing

withCapturedIface :: Bool -> String -> BS.ByteString -> (FilePath -> IO a) -> IO a
withCapturedIface timing owner bytes consume = do
  directory <- getTemporaryDirectory
  bracket (openBinaryTempFile directory "tidepool-exact-iface.hi")
    (\(path, handle) -> do
      closed <- hIsClosed handle
      unless closed (hClose handle)
      removeFile path)
    (\(path, handle) -> do
      BS.hPut handle bytes
      emitCount timing ("exact_iface_capture_write_bytes." ++ owner) (fromIntegral (BS.length bytes))
      hClose handle
      consume path)

-- GHC's home-interface knot allows mutually recursive source/boot modules to
-- resolve each other's original Names while typechecking their details.
hydrateExactScope
  :: HscEnv -> [(ExactIfaceArtifact, ModIface)] -> IO HscEnv
hydrateExactScope env loaded = hydrateOriginalInterfaces env (map snd loaded)

-- Typechecking original compiler interfaces is shared by exact hydration and
-- producer semantic checks. This issues GHC details, not admitted authority.
hydrateOriginalInterfaces :: HscEnv -> [ModIface] -> IO HscEnv
hydrateOriginalInterfaces env loaded = do
  timing <- readTimingEnabled
  timeDetailPhase timing "exact_scope" "hydrate" $ do
    details <- fixIO $ \recursiveDetails -> do
      let knotted = withDetails recursiveDetails
      forM pending $ \iface ->
        initIfaceCheck (text "tidepool exact hydration") knotted (typecheckIface iface)
    pure (withDetails details)
  where
    pending = filter (not . alreadyHydrated) loaded
    alreadyHydrated iface = case lookupHpt (hsc_HPT env) (moduleName (mi_module iface)) of
      -- Thin value interfaces have no content fingerprint. Their verified
      -- captured object must be installed; an older zero-fingerprint HMI
      -- cannot supply its details or the completed import's allocation witness.
      Just home -> mi_iface_hash (mi_final_exts iface) /= fingerprint0
        && mi_module (hm_iface home) == mi_module iface
        && mi_iface_hash (mi_final_exts (hm_iface home)) == mi_iface_hash (mi_final_exts iface)
      Nothing -> False
    withDetails details = hscUpdateHPT_lazy (\hpt -> foldr
      (\(iface, detail) table -> addToHpt table (moduleName (mi_module iface))
        (HomeModInfo iface detail emptyHomeModInfoLinkable))
      hpt (zipDetails pending details)) env
    -- The loaded interface spine is available before fixIO returns. Ordinary
    -- zip would demand the recursive detail spine while building the HPT;
    -- sharing a deferred head/tail split keeps the knot lazy and traversal linear.
    zipDetails [] _ = []
    zipDetails (iface : rest) remaining =
      let ~(detail, tailDetails) = splitDetails remaining
      in (iface, detail) : zipDetails rest tailDetails
    splitDetails (detail : rest) = (detail, rest)
    splitDetails [] = error "exact hydration detail arity mismatch"

-- A source-less interface borrows original dfun/axiom Names. GHC unions
-- reachable HMI tables, so two lexical views can otherwise insert the same
-- original instance twice. Select each identity once in this target's temporary
-- environment, never in the retained interfaces or the complete consistency
-- closure. Authenticated joins copy these records from their original owners;
-- equal Names therefore carry the same original instance metadata.
exactHomeInstancesFor :: ModSummary -> HscEnv -> HscEnv
exactHomeInstancesFor summary env = hscUpdateHPT_lazy install env
  where
    reachable = hptSomeThingsBelowUs (\hmi -> [hmi]) True env
      (homeUnitId (hsc_home_unit env))
      (GWIB (moduleName (ms_mod summary)) (if ms_hsc_src summary == HsBootFile then IsBoot else NotBoot))
    active = filter ((== homeUnitAsUnit (hsc_home_unit env)) . moduleUnit . mi_module . hm_iface) reachable
    (_, projected) = mapAccumL project (Set.empty, Set.empty) active
    project (classes, families) hmi =
      let details = hm_details hmi
          (classes', instances) = unique is_dfun_name classes (instEnvElts (md_insts details))
          (families', equations) = unique (coAxiomName . fi_axiom) families (md_fam_insts details)
       in ((classes', families'), hmi { hm_details = details
            { md_insts = mkInstEnv instances, md_fam_insts = equations } })
    install table = foldr (\hmi hpt -> addToHpt hpt (moduleName (mi_module (hm_iface hmi))) hmi) table projected
    unique key seen values =
      let step retained value
            | key value `Set.member` retained = (retained, Nothing)
            | otherwise = (Set.insert (key value) retained, Just value)
          (final, chosen) = mapAccumL step seen values
       in (final, [value | Just value <- chosen])

-- The facade typecheck path restores its exact HPT on both SourceError and
-- asynchronous cancellation; the diagnostics path borrows the same projection
-- directly without changing its caller's Session.
withExactHomeInstances :: ModSummary -> Ghc a -> Ghc a
withExactHomeInstances summary action = reifyGhc $ \session -> bracket
  (reflectGhc getSession session)
  (\original -> reflectGhc (do
    current <- getSession
    setSession ((hscUpdateHPT_lazy (const (hsc_HPT original)) current)
      { hsc_plugins = hsc_plugins original })) session)
  (\original -> reflectGhc (setSession (exactHomeInstancesFor summary original) >> action) session)

previewOrphanPluginMarker :: String
previewOrphanPluginMarker = "tidepool-original-preview-orphans"

-- The callback belongs to one verified target and is replaced on every graph
-- installation. Resetting a transaction removes it, including after failure.
withoutPreviewOrphanPlugin :: HscEnv -> HscEnv
withoutPreviewOrphanPlugin env = env
  { hsc_plugins = plugins { staticPlugins = filter retained (staticPlugins plugins) } }
  where
    plugins = hsc_plugins env
    retained plugin = paArguments (spPlugin plugin) /= [previewOrphanPluginMarker]

withOriginalPreviewOrphans :: [OriginalPreviewOrphans] -> HscEnv -> HscEnv
withOriginalPreviewOrphans scopes original = env
  { hsc_plugins = plugins { staticPlugins = map scopedPlugin scopes ++ staticPlugins plugins } }
  where
    env = withoutPreviewOrphanPlugin original
    plugins = hsc_plugins env
    scopedPlugin (OriginalPreviewOrphans target fingerprint orphans) = StaticPlugin
      { spPlugin = PluginWithArgs
          { paPlugin = defaultPlugin
              { renamedResultAction = \_ environment declarations ->
                  if tcg_mod environment /= target then pure (environment,declarations) else do
                    current <- getTopEnv
                    unless (any (\case
                        ModuleNode _ summary -> ms_mod summary == target && ms_hs_hash summary == fingerprint
                        _ -> False) (mgModSummaries' (hsc_mod_graph current)))
                      (liftIO (fail "activation preview orphan callback target fingerprint changed"))
                    let imports = tcg_imports environment
                        restored = environment { tcg_imports = imports { imp_orphs = Set.toAscList
                          (Set.fromList (orphans ++ imp_orphs imports)) } }
                    pure (restored,declarations)
              , pluginRecompile = \_ -> pure (MaybeRecompile
                  (fingerprintString ("original-preview-orphans-v1:"
                    ++ show (fingerprint,map moduleKey (target:orphans)))))
              }
          , paArguments = [previewOrphanPluginMarker]
          }
      , spInitialised = True
      }
    moduleKey owner = (unitString (moduleUnit owner),moduleNameString (moduleName owner))

-- A lexical interface contributes its chosen instance/family environment to
-- GHC's graph traversal. Implementation-only HMIs remain installed but never
-- appear in this graph; their original Names can still resolve through HPT.
-- The caller supplies virtual-to-virtual edges, never implementation edges.
installExactLexicalGraph
  :: ModuleGraph -> [(ExactIfaceArtifact, [(String, String)])]
  -> CheckedValueImportAuthority -> HscEnv
  -> IO (Either String HscEnv)
installExactLexicalGraph sourceGraph lexical checkedValues =
  installExactLexicalGraphWithScaffold sourceGraph lexical checkedValues noGeneratedScaffoldImports

installExactLexicalGraphWithScaffold
  :: ModuleGraph -> [(ExactIfaceArtifact, [(String,String)])]
  -> CheckedValueImportAuthority -> GeneratedScaffoldImportAuthority -> HscEnv
  -> IO (Either String HscEnv)
installExactLexicalGraphWithScaffold sourceGraph lexical (CheckedValueImportAuthority checkedValues)
    authority@(GeneratedScaffoldImportAuthority _ scaffoldGraph instanceEdges orphanScopes) env
  | not (null conflictingRows) = pure (Left "checked template graph conflicts with current exact lexical graph")
  | Set.size lexicalModuleNames /= length lexical = pure (Left "duplicate virtual lexical owner")
  | Set.size virtualModuleNames /= length virtualRows = pure (Left "duplicate virtual graph module name")
  | any (\(artifact, _) -> exactUnit artifact /= unitString home) virtualRows =
      pure (Left "virtual graph owner is outside active home unit")
  | any (\(_, deps) -> any (`Set.notMember` virtualOwners) deps) virtualRows =
      pure (Left "virtual lexical edge leaves admitted graph")
  | any (\(_,_,deps) -> any (`Set.notMember` virtualOwners) deps) instanceEdges =
      pure (Left "activation preview instance edge leaves admitted graph")
  | any (\(target,fingerprint,_) -> length [() | ModuleNode _ summary <- mgModSummaries' sourceGraph
      , ms_mod summary == target && ms_hs_hash summary == fingerprint] /= 1) instanceEdges =
      pure (Left "activation preview instance target differs from its protected recipe")
  | any (\(OriginalPreviewOrphans target fingerprint _) ->
      not (any (\(owner,seal,_) -> owner == target && seal == fingerprint) instanceEdges)) orphanScopes =
      pure (Left "activation preview orphan scope differs from its protected instance target")
  | any (\node -> case node of
      ModuleNode _ summary -> keyOf summary `Set.member` Set.union virtualOwners checkedValues
      _ -> False) (mgModSummaries' sourceGraph) =
      pure (Left "virtual lexical owner collides with source graph")
  | any (\(artifact, _) -> case lookupHpt (hsc_HPT env)
        (mkModuleName (exactModule artifact)) of
      Nothing -> True
      Just hmi -> mi_module (hm_iface hmi) /=
        mkModule (stringToUnit (exactUnit artifact))
          (mkModuleName (exactModule artifact))) virtualRows =
      pure (Left "virtual lexical interface missing from exact HPT")
  | not (null unadmittedHomeEdges) =
      pure (Left ("source graph imports unadmitted home implementation: "
        ++ show unadmittedHomeEdges))
  | otherwise = do
      forM_ virtualRows $ \(artifact, _) ->
        addHomeModuleToFinder (hsc_FC env) (hsc_home_unit env)
          (GWIB (mkModuleName (exactModule artifact)) NotBoot)
          (ms_location (exactInterfaceSummary env artifact))
      pure (Right (withOriginalPreviewOrphans orphanScopes env)
        { hsc_mod_graph = mkModuleGraph (sourceNodes ++ virtualNodes) })
  where
    keyOfArtifact artifact = (exactUnit artifact,exactModule artifact)
    graphRows = lexical ++ scaffoldGraph
    rowGroups = Map.fromListWith (++) [(keyOfArtifact artifact,[(artifact,deps)]) | (artifact,deps) <- graphRows]
    conflictingRows = [owner | (owner,first:rest) <- Map.toList rowGroups, any (/= first) rest]
    virtualRows = Map.elems (Map.fromList [(keyOfArtifact artifact,(artifact,deps)) | (artifact,deps) <- graphRows])
    lexicalModuleNames = Set.fromList [exactModule artifact | (artifact, _) <- lexical]
    virtualModuleNames = Set.fromList [exactModule artifact | (artifact, _) <- virtualRows]
    virtualOwners = Set.fromList (map (keyOfArtifact . fst) virtualRows)
    -- Graph validation includes protected template dependencies. General authored
    -- import permission still belongs only to the current lexical selection.
    owners = Set.fromList
      [(exactUnit artifact, exactModule artifact) | (artifact, _) <- lexical]
    home = homeUnitId (hsc_home_unit env)
    known = Set.union (Set.union owners checkedValues) (Set.fromList
      [keyOf summary | ModuleNode _ summary <- mgModSummaries' sourceGraph])
    -- Downsweep intentionally excludes admitted exact owners. It may therefore
    -- omit the corresponding edge; textual imports still cannot expose an
    -- implementation-only HPT entry outside the selected lexical graph.
    unadmittedHomeEdges =
      [(keyOf summary, requested)
      | ModuleNode edges summary <- sourceNodes
      , requested <- Set.toAscList (Set.fromList
          ([owner | edge <- edges, Just owner <- [missing summary edge]]
           ++ [owner | imported <- ms_textual_imps summary ++ ms_srcimps summary
              , Just owner <- [hiddenImport summary imported]]))]
    permitted summary requested qualifier imported = permitsGeneratedScaffoldImport
      authority summary requested (qualifier,imported)
    hiddenImport summary (qualifier, imported) =
      let name = unLoc imported
          local = case qualifier of
            NoPkgQual -> True
            ThisPkg unit -> unit == home
            OtherPkg _ -> False
      in if not local then Nothing else case lookupHpt (hsc_HPT env) name of
        Nothing -> Nothing
        Just hmi ->
          let owner = (unitString (moduleUnit (mi_module (hm_iface hmi))), moduleNameString name)
          in if owner `Set.member` known || permitted summary owner qualifier imported then Nothing else Just owner
    missing summary (NodeKey_Module (ModNodeKeyWithUid (GWIB name _) unit))
      | unit == home, (unitString unit, moduleNameString name) `Set.notMember` known =
          let requested = (unitString unit,moduleNameString name) in
          if any (\(qualifier, imported) -> unLoc imported == name && permitted summary requested qualifier imported)
              (ms_textual_imps summary) then Nothing else Just requested
    missing _ _ = Nothing
    keyOf summary =
      (unitString (moduleUnit (ms_mod summary)), moduleNameString (moduleName (ms_mod summary)))
    nodeKey (_, moduleName') = NodeKey_Module
      (ModNodeKeyWithUid (GWIB (mkModuleName moduleName') NotBoot) home)
    sourceNodes = [case node of
      ModuleNode edges summary ->
        let imports =
              [ (unitString home, moduleNameString (unLoc imported))
              | (qualifier, imported) <- ms_textual_imps summary
              , case qualifier of
                  NoPkgQual -> True
                  ThisPkg unit -> unit == home
                  OtherPkg _ -> False
              ]
            added = [nodeKey owner | owner <- imports, owner `Set.member` owners]
              ++ [nodeKey owner | (qualifier,name) <- ms_textual_imps summary
                  , let owner = (unitString home,moduleNameString (unLoc name))
                  , permitted summary owner qualifier name]
              ++ [nodeKey owner | (target,fingerprint,roots) <- instanceEdges
                  , ms_mod summary == target && ms_hs_hash summary == fingerprint
                  , owner <- roots]
        in ModuleNode (Set.toList (Set.fromList (edges ++ added))) summary
      other -> other
      | node <- mgModSummaries' sourceGraph]
    virtualNodes =
      [ ModuleNode (map nodeKey deps) (exactInterfaceSummary env artifact)
      | (artifact, deps) <- virtualRows ]

-- This node describes admitted interface dependencies for scope/linker graphs.
-- It is not a source summary and must never be sent through GHC make.
exactInterfaceSummary :: HscEnv -> ExactIfaceArtifact -> ModSummary
exactInterfaceSummary env artifact = ModSummary
  { ms_mod = mkModule (stringToUnit (exactUnit artifact))
      (mkModuleName (exactModule artifact))
  , ms_hsc_src = HsSrcFile
  , ms_location = location
  , ms_hs_hash = fingerprintByteString BS.empty
  , ms_obj_date = Nothing
  , ms_dyn_obj_date = Nothing
  , ms_iface_date = Nothing
  , ms_hie_date = Nothing
  , ms_srcimps = []
  , ms_textual_imps = []
  , ms_ghc_prim_import = False
  , ms_parsed_mod = Nothing
  , ms_hspp_file = exactPath artifact
  , ms_hspp_opts = hsc_dflags env
  , ms_hspp_buf = Nothing
  }
  where location = ModLocation
          { ml_hs_file = Nothing
          , ml_hi_file = exactPath artifact
          , ml_dyn_hi_file = exactPath artifact
          , ml_obj_file = exactPath artifact
          , ml_dyn_obj_file = exactPath artifact
          , ml_hie_file = exactPath artifact
          }

hexBytes :: BS.ByteString -> String
hexBytes = concatMap (\byte -> let s = showHex byte "" in replicate (2 - length s) '0' ++ s)
  . BS.unpack
