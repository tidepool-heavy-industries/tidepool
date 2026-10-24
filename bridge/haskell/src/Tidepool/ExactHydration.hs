{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}

module Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, newOriginalInterfaceArtifacts, newOriginalInterfaceArtifactsWithSessionCaptures
  , newOriginalInterfaceArtifactsWithSessionOutputs, newOriginalInterfaceArtifactsWithReader, newOriginalInterfaceArtifactsWithLookup
  , originalInterfaceBody, originalInterfaceBytes, originalInterfaceSha256, originalRetainedInterface, originalRetainedInterfaceCaptures, sameExactInterfaceIdentity
  , originalSessionInterfaces, originalProducedSessionInterfaces, serializeOriginalInterface
  , ExactIfaceArtifact(..)
  , ExactInterfaceOperations, exactInterfaceOperations, runExactInterfaceOperation
  , depanalSourceModules, freshExactState, freshExactContext, forkExactContext
  , PackageFinderFacts, newPackageFinderFacts, forkExactContextWithPackageFacts, retainClosedHomeContext
  , ExactContextForkFailure(..)
  , readExactIfaceArtifacts, readExactIfaceArtifactsWith
  , RequestIfaceDecoder, newRequestIfaceDecoder, pruneRequestIfaceDecoder, readCapturedExactIfaceArtifacts
  , readCapturedExactIfaceClosureWithCheckedValues
  , hydrateExactScope, hydrateOriginalInterfaces, exactInterfaceSummary
  , HydratedExactInterfaces, hydrateInterfaceInputs, hydratedExactEnvironment, hydratedExactInputs, hydratedExactInterfaces, restoreHydratedInterfaceEnvironment, hydratedInterfaceProduct
  , exactHomeInstancesFor, withExactHomeInstances
  , CheckedValueImportAuthority
  , checkedValueOwner, noCheckedValueImports
  , checkedValueImportAuthorityFromCaptures, unionCheckedValueImports, admitsCheckedValueImport
  , readCheckedValueImportAuthority
  , CapturedInterfaceInputs, capturedInterfaceInputs, captureCompletedInterfaceInputs
  , selectCapturedInterfaceInputs, selectCapturedInterfaceOwners, mergeCapturedInterfaceInputs
  , capturedSessionInterfaceFor, checkCapturedSessionInterface
  , InterfaceInputFailure(..)
  , readVerifiedExactIfaceClosure
  , readVerifiedExactIfaceClosureWithCheckedValues, readVerifiedExactIfaceClosureWithCheckedValuesReader
  , selectVerifiedExactInterfaces
  , VerifiedExactInterface, verifiedInterfaceProducts, selectVerifiedExactInterfaceProducts, verifiedExactInterface, verifiedExactInterfaceBody
  , selectVerifiedValueInterfaces
  , checkedValueImportAuthorityFromVerified
  , CheckedTemplateInterface(..)
  , CheckedTemplateImports(..)
  , ProtectedTemplateImports, RenderedProtectedTemplateImports
  , captureProtectedTemplateImports, captureProtectedTemplateImportsAt, renderProtectedTemplateImports
  , GeneratedScaffoldRecipe, generatedScaffoldRecipe, generatedCheckingTemplateRecipe
  , generatedScaffoldRecipeWithProtectedImports, generatedCheckingTemplateRecipeWithProtectedImports
  , generatedTypedSegmentRecipeWithProtectedImports
  , generatedCheckingTemplateRecipeWithImportRendering, generatedTypedSegmentRecipeWithImportRendering
  , generatedScaffoldRecipeWithImportRendering
  , generatedActivationPreviewRecipe, captureGeneratedScaffoldTarget
  , CapturedGeneratedScaffoldSource, captureGeneratedScaffoldSource
  , capturedGeneratedScaffoldBytes, capturedGeneratedScaffoldText, capturedGeneratedScaffoldPath
  , capturedSourceImportOrigins
  , CapturedGeneratedImports, parseCapturedGeneratedImports
  , capturedGeneratedImportSource, capturedGeneratedImportFlags, capturedGeneratedImportSyntax
  , CapturedGeneratedImportOccurrence, capturedGeneratedImportOccurrences
  , capturedGeneratedImportOccurrenceSyntax, capturedGeneratedImportOccurrenceOrigin
  , permitsGeneratedScaffoldImport
  , GeneratedScaffoldImportAuthority, noGeneratedScaffoldImports, readGeneratedScaffoldImportAuthority
  , projectExactLexicalGraph
  ) where

import Tidepool.ArtifactBytes (ArtifactBytes, captureArtifactBytes, artifactBytes, artifactSha256)
import Tidepool.Timing (readTimingEnabled, emitCount, timeDetailPhase)
import Tidepool.TypedSegment.Types (GeneratedSegmentOperations, generatedSegmentQualifier)
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString, CapturedSessionInterface, capturedSessionInterfaceBody, capturedSessionInterface, capturedSessionInterfaceEvidence, capturedSessionInterfaceEvidenceBodies, capturedSessionCompilerProduct, sameCapturedSessionInterfaceOwner)
import Control.Monad (forM, forM_, unless, foldM)
import Control.Concurrent.MVar (MVar, newMVar, modifyMVar, modifyMVarMasked)
import GHC.Types.Name.Cache (NameCache(nsNames,nsUniqChar), OrigNameCache)
import Control.Monad.IO.Class (liftIO)
import Data.List (mapAccumL)
import Control.Exception
  ( Exception, IOException, SomeException, SomeAsyncException, bracket, try, fromException, throwIO, evaluate )
import Data.Char (isHexDigit, toLower)
import Data.Maybe (isJust)
import Data.List (nub, sortOn)
import GHC.Data.Strict qualified as Strict
import qualified Data.ByteString as BS
import qualified Crypto.Hash.SHA256 as SHA256
import GHC.Driver.Env
  ( HscEnv(..), hscUpdateHPT, hscUpdateHPT_lazy, hscUpdateFlags, hptSomeThingsBelowUs, hsc_home_unit, hsc_home_unit_maybe, hsc_HPT, hscEPS, discardIC, hsc_all_home_unit_ids )
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
import GHC.Driver.Env.KnotVars (emptyKnotVars)
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), emptyHomeModInfoLinkable, emptyHomePackageTable, addToHpt
  , lookupHpt, eltsHpt, filterHpt )
import GHC.Iface.Load (readIface, loadInterface, WhereFrom(..))
import GHC.Driver.Session (DynFlags, importPaths, ghcMode, GhcMode(CompManager), xopt)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Driver.Ppr (showPpr)
import qualified GHC.Parser as Parser (parseHeader, parseModule)
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP, getPsErrorMessages)
import GHC.Types.Error (isEmptyMessages)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Data.FastString (mkFastString, fsLit)
import Data.IORef (IORef, newIORef, readIORef, modifyIORef', atomicModifyIORef')
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLoad)
import GHC.Unit.Module (Module, ModuleName, moduleName, moduleUnit, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.Graph
  ( ModuleGraph, ModuleGraphNode(..), NodeKey(..), ModNodeKeyWithUid(..)
  , mgModSummaries', mkModuleGraph )
import GHC.Unit.Module.Location
  ( pattern ModLocation
  , ml_hs_file, ml_hi_file, ml_dyn_hi_file, ml_obj_file, ml_dyn_obj_file, ml_hie_file )
import GHC.Unit.Module.ModSummary (ModSummary(..))
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.SourceText (sl_fs)
import GHC.Types.PkgQual (PkgQual(..), RawPkgQual(..))
import GHC.Types.SrcLoc (Located, unLoc, getLoc, SrcSpan(..), srcSpanStartLine, mkRealSrcLoc, getBufSpan, bufSpanStart, bufSpanEnd, bufPos)
import GHC.Types.Avail (availNames)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import Tidepool.DependencyEvidence (sourceEvidenceWithFingerprint, dependencySourceSha256)
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import GHC (Ghc, GhcPs, ParsedModule(..), getSession, setSession, depanal)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Driver.Monad (reifyGhc, reflectGhc)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Core.InstEnv (instEnvElts, is_dfun_name, mkInstEnv)
import GHC.Core.FamInstEnv (fi_axiom)
import GHC.Core.Coercion.Axiom (coAxiomName)
import GHC.Parser.Annotation (getLocA)
import Language.Haskell.Syntax (HsModule(..))
import GHC.Hs (ImportDecl(..), ImportDeclQualifiedStyle(..))
import GHC.Unit.Module.Deps (dep_orphs, dep_finsts, dep_direct_mods)
import GHC.Unit.Home (homeUnitAsUnit, homeUnitId, isHomeUnit, isHomeUnitDefinite)
import GHC.Unit.Types (GenWithIsBoot(..), InstalledModuleWithIsBoot, UnitId)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Utils.Fingerprint (fingerprint0, fingerprintByteString, fingerprintString)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_extra_decls, mi_exports, mi_insts, mi_fam_insts, mi_deps, mi_iface_hash, mi_orphan, mi_final_exts, mi_decls, mi_hsc_src)
import GHC.Builtin.Names (gHC_PRIM)
import Tidepool.FatIface (readPackageInterface)
import Tidepool.FatIface.Internal (withCapturedIface)
import Tidepool.ResumePackage (resolveResumeInterface)
import GHC.Unit.Types (unitString, unitIdString, stringToUnit, toUnitId, GenUnit(RealUnit), Definite(Definite))
import Tidepool.FinalizedModule (FinalizedModule, finalizedHomeModInfo, CompletedModuleInterface, completedModuleInterfaceFinalized, completedModuleInterfaceHome, sameCompletedModuleInterfaceOwner, captureFinalizedInterface, finalizedInterfaceNameCache, homeInterfaceUsageOwners)
import Tidepool.FinalizedModule.Internal (serializeOriginalInterface)
import qualified GHC.Data.Maybe as MErr
import GHC.Utils.Outputable (text, ppr, showSDocOneLine, defaultSDocContext)
import Numeric (showHex)
import System.Directory (canonicalizePath)
import GHC.Fingerprint.Type (Fingerprint)
import System.IO (fixIO)
import Tidepool.TurnSource (preambleImportMarker, ImportOrigin(..), ImportRendering, importRenderingSource, importRenderingOrigins)
import Tidepool.ImportSyntax
  ( ImportDescriptor, describeImport, CapturedImportSyntax, captureImportSyntax
  , ImportOccurrence, capturedImportOccurrences, importOccurrenceId, importOccurrenceDescriptor
  , DriverImportSyntax, driverParsedModule, driverImportOccurrences )
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

sameExactInterfaceIdentity :: ExactIfaceArtifact -> ExactIfaceArtifact -> Bool
sameExactInterfaceIdentity left right =
  (exactUnit left,exactModule left,exactSha256 left,exactRequirements left)
    == (exactUnit right,exactModule right,exactSha256 right,exactRequirements right)

-- The compiler owner supplies its guarded runner, rather than an ambient
-- HscEnv. Every operation borrows that owner's fixed package facts with empty
-- home visibility. The runner also rejects use after release or during another
-- operation; this capability creates no separate lifetime or interpreter owner.
newtype ExactInterfaceOperations = ExactInterfaceOperations
  (forall result. (HscEnv -> IO result) -> IO result)

exactInterfaceOperations
  :: (forall result. Ghc result -> IO result) -> [FilePath] -> ExactInterfaceOperations
exactInterfaceOperations run includes = ExactInterfaceOperations $ \operation -> run $ do
  current <- getSession
  let selected = hscUpdateHPT_lazy (const emptyHomePackageTable) current
        { hsc_mod_graph = mkModuleGraph []
        , hsc_targets = []
        , hsc_type_env_vars = emptyKnotVars
        }
      configured = hscUpdateFlags (\flags -> flags { importPaths = includes }) selected
  setSession configured
  liftIO (operation configured)

runExactInterfaceOperation :: ExactInterfaceOperations -> (HscEnv -> IO result) -> IO result
runExactInterfaceOperation (ExactInterfaceOperations run) = run

-- Checked value modules are injected in dependency order after the lexical
-- graph is installed. Their verified identities authorize source imports at
-- that earlier boundary without exposing implementation-only originals.
newtype CheckedValueImportAuthority = CheckedValueImportAuthority (Set.Set (String, String))

newtype CapturedInterfaceInputs = CapturedInterfaceInputs
  (Map.Map (String, String) (VerifiedExactInterface, [ExactIfaceArtifact]))

data InterfaceInputFailure
  = InterfaceInputConflict (String,String)
  | InterfaceInputMissing (String,String)
  | InterfaceInputSelectionMismatch (String,String)
  | InterfaceInputDependenciesMissing (String,String) [(String,String)]
  | InterfaceSessionAliasMismatch (String,String)
  | InterfaceNameCacheMismatch
  | InterfaceHydratedHomeMissing (String,String)
  | InterfaceHydratedHomeMismatch (String,String)
  | InterfacePhaseHomeConflict (String,String)
  | InterfaceCompletedOwnerConflict (String,String)
  deriving (Eq,Show)
instance Exception InterfaceInputFailure

-- Assembly selects already admitted products, rather than decoding another
-- inventory or reconstructing ownership from a digest. Conflicts apply to this
-- selected namespace; unrelated retained alternatives are not arguments here.
capturedInterfaceInputs :: [VerifiedExactInterface] -> Either InterfaceInputFailure CapturedInterfaceInputs
capturedInterfaceInputs selected = do
  case selected of
    [] -> pure ()
    VerifiedExactInterface _ _ _ issuer:rest ->
      unless (all (\(VerifiedExactInterface _ _ _ actual) -> actual == issuer) rest)
        (Left InterfaceNameCacheMismatch)
  let groups = Map.fromListWith (++)
        [((exactUnit artifact,exactModule artifact),[verified]) | verified <- selected
          , let (artifact,_) = verifiedExactInterface verified]
      owners = Map.keysSet groups
  rows <- forM (Map.toAscList groups) $ \(key,products) -> case products of
    [] -> Left (InterfaceInputMissing key)
    first:rest -> do
      let (artifact,_) = verifiedExactInterface first
      unless (all (\other -> sameExactInterfaceIdentity artifact (fst (verifiedExactInterface other))) rest)
        (Left (InterfaceInputConflict key))
      let missing = filter (`Set.notMember` owners) (exactRequirements artifact)
      unless (null missing) (Left (InterfaceInputDependenciesMissing key missing))
      chosen <- foldM (preferCompilerProduct key) first rest
      pure (key,(chosen,nub (map (fst . verifiedExactInterface) products)))
  pure (CapturedInterfaceInputs (Map.fromList rows))

preferCompilerProduct :: (String,String) -> VerifiedExactInterface -> VerifiedExactInterface
  -> Either InterfaceInputFailure VerifiedExactInterface
preferCompilerProduct key left@(VerifiedExactInterface _ leftProduct _ leftIssuer)
    right@(VerifiedExactInterface _ rightProduct _ rightIssuer) = do
  unless (leftIssuer == rightIssuer) (Left InterfaceNameCacheMismatch)
  case (leftProduct,rightProduct) of
    (DecodedTypeInterface _,DecodedTypeInterface _) -> Right left
    (CompletedHomeInterface _,DecodedTypeInterface _) -> Right left
    (DecodedTypeInterface _,CompletedHomeInterface _) -> Right right
    (CompletedHomeInterface first,CompletedHomeInterface second)
      | sameCompletedInterfaceOwner first second -> Right left
      | otherwise -> Left (InterfaceCompletedOwnerConflict key)

-- The session injection owner carries authenticated bytes and its actual
-- decoded HMI/NameCache together. A receiving alias can change no identity.
capturedSessionProducts :: [(ExactIfaceArtifact,CapturedSessionInterface)] -> Either InterfaceInputFailure [VerifiedExactInterface]
capturedSessionProducts captures = forM captures (\(artifact,capture) -> do
    let (owner,body) = capturedSessionInterfaceBody capture
        (issuer,home) = capturedSessionCompilerProduct capture
    unless (owner == mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
        && artifactSha256 body == exactSha256 artifact && mi_module (hm_iface home) == owner)
      (Left (InterfaceSessionAliasMismatch (exactUnit artifact,exactModule artifact)))
    pure (VerifiedExactInterface artifact (CompletedHomeInterface (SessionHomeInterface capture)) body issuer))

captureCompletedInterfaceInputs :: HscEnv -> CapturedInterfaceInputs
  -> [(ExactIfaceArtifact,CapturedSessionInterface)] -> [CompletedModuleInterface]
  -> IO CapturedInterfaceInputs
captureCompletedInterfaceInputs environment previous captures finalized = do
  sessions <- either throwIO pure (capturedSessionProducts captures)
  produced <- forM finalized $ \completed -> do
    let original = completedModuleInterfaceFinalized completed
        home = completedModuleInterfaceHome completed
        owner = mi_module (hm_iface home)
        issuer = finalizedInterfaceNameCache original
    unless (issuer == (nsUniqChar (hsc_NC environment),nsNames (hsc_NC environment)))
      (throwIO InterfaceNameCacheMismatch)
    body <- captureFinalizedInterface original
    let artifact = ExactIfaceArtifact (unitString (moduleUnit owner)) (moduleNameString (moduleName owner))
          ("<completed-type>/" ++ moduleNameString (moduleName owner)) (artifactSha256 body)
          (homeInterfaceUsageOwners environment (hm_iface home))
    pure (VerifiedExactInterface artifact (CompletedHomeInterface (FinalizedHomeInterface completed)) body issuer)
  -- Preserve every authenticated receiving alias while choosing the compiler
  -- product once. Reassembly from just the primary rows would lose aliases
  -- admitted by earlier operations.
  CapturedInterfaceInputs selected <- either throwIO pure
    (capturedInterfaceInputs (verifiedInterfaceProducts previous ++ sessions ++ produced))
  let CapturedInterfaceInputs retained = previous
  pure (CapturedInterfaceInputs (Map.mapWithKey
    (\key (verified,known) -> (verified,nub (known ++ maybe [] snd (Map.lookup key retained)))) selected))

selectCapturedInterfaceInputs :: CapturedInterfaceInputs -> [ExactIfaceArtifact]
  -> Either InterfaceInputFailure CapturedInterfaceInputs
selectCapturedInterfaceInputs allInputs@(CapturedInterfaceInputs captured) roots = do
  forM_ roots $ \artifact -> do
    if checkedValueOwner artifact
      then either (const (Left (InterfaceInputSelectionMismatch (exactUnit artifact,exactModule artifact))))
        (const (Right ())) (selectVerifiedValueInterfaces allInputs [artifact])
      else either (const (Left (InterfaceInputSelectionMismatch (exactUnit artifact,exactModule artifact))))
        (const (Right ())) (selectVerifiedExactInterfaceProducts allInputs [artifact])
  selected <- close Map.empty [(exactUnit root,exactModule root) | root <- roots]
  pure (CapturedInterfaceInputs selected)
  where
    close selected [] = Right selected
    close selected (key:pending)
      | Map.member key selected = close selected pending
      | otherwise = case Map.lookup key captured of
          Nothing -> Left (InterfaceInputMissing key)
          Just row@(verified,_) -> let (artifact,_) = verifiedExactInterface verified
            in close (Map.insert key row selected) (exactRequirements artifact ++ pending)

selectCapturedInterfaceOwners :: CapturedInterfaceInputs -> [(String,String)]
  -> Either InterfaceInputFailure CapturedInterfaceInputs
selectCapturedInterfaceOwners inputs@(CapturedInterfaceInputs captured) owners = do
  roots <- forM owners $ \owner -> case Map.lookup owner captured of
    Just (verified,_) -> Right (fst (verifiedExactInterface verified))
    Nothing -> Left (InterfaceInputMissing owner)
  selectCapturedInterfaceInputs inputs roots

-- Borrow the actual successful session issuer when another selected role
-- refers to the same value. A decoded copy cannot issue this association.
capturedSessionInterfaceFor :: CapturedInterfaceInputs -> ExactIfaceArtifact -> ArtifactBytes -> ArtifactBytes
  -> Either InterfaceInputFailure (Maybe CapturedSessionInterface)
capturedSessionInterfaceFor inputs artifact body packages = do
  CapturedInterfaceInputs selected <- selectCapturedInterfaceInputs inputs [artifact]
  case Map.lookup (exactUnit artifact,exactModule artifact) selected of
    Just (VerifiedExactInterface _ (CompletedHomeInterface (SessionHomeInterface capture)) _ _,_) ->
      Just <$> checkCapturedSessionInterface artifact body packages capture
    Just _ -> Right Nothing
    Nothing -> Left (InterfaceInputMissing (exactUnit artifact,exactModule artifact))

-- Borrowing preserves the actual issuer while checking every selected value
-- input fact. Equal interface bytes cannot replace another package obligation.
checkCapturedSessionInterface :: ExactIfaceArtifact -> ArtifactBytes -> ArtifactBytes
  -> CapturedSessionInterface -> Either InterfaceInputFailure CapturedSessionInterface
checkCapturedSessionInterface artifact body packages capture = do
  let key = (exactUnit artifact,exactModule artifact)
      (owner,actualBody) = capturedSessionInterfaceBody capture
  unless (owner == mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
      && actualBody == body && artifactSha256 body == exactSha256 artifact)
    (Left (InterfaceSessionAliasMismatch key))
  case capturedSessionInterfaceEvidenceBodies capture of
    Just (actualPackages,_,required)
      | actualPackages == packages, required == exactRequirements artifact -> Right capture
    _ -> Left (InterfaceSessionAliasMismatch key)

mergeCapturedInterfaceInputs :: [CapturedInterfaceInputs] -> Either InterfaceInputFailure CapturedInterfaceInputs
mergeCapturedInterfaceInputs inputs = do
  CapturedInterfaceInputs selected <- capturedInterfaceInputs (concatMap verifiedInterfaceProducts inputs)
  let aliases = Map.unionsWith (++) [Map.map snd rows | CapturedInterfaceInputs rows <- inputs]
  pure (CapturedInterfaceInputs (Map.mapWithKey
    (\key (verified,known) -> (verified,nub (known ++ Map.findWithDefault [] key aliases))) selected))

readVerifiedExactIfaceClosure
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String CapturedInterfaceInputs)
readVerifiedExactIfaceClosure = readVerifiedExactIfaceClosureWith (BS.readFile . exactPath)

readVerifiedExactIfaceClosureWith :: (ExactIfaceArtifact -> IO BS.ByteString)
  -> HscEnv -> [ExactIfaceArtifact] -> IO (Either String CapturedInterfaceInputs)
readVerifiedExactIfaceClosureWith readInput = readVerifiedExactIfaceClosureUsing (readVerifiedOneWith readInput)

readVerifiedExactIfaceClosureUsing :: (HscEnv -> ExactIfaceArtifact -> IO (Either String VerifiedExactInterface))
  -> HscEnv -> [ExactIfaceArtifact] -> IO (Either String CapturedInterfaceInputs)
readVerifiedExactIfaceClosureUsing readInterface env artifacts = fmap (CapturedInterfaceInputs . Map.fromList
  . map (\verified -> let (artifact,_) = verifiedExactInterface verified
      in ((exactUnit artifact,exactModule artifact),(verified,[artifact]))))
  <$> readExactIfaceArtifactsUsing readInterface env artifacts

readVerifiedExactIfaceClosureWithCheckedValues
  :: HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String CapturedInterfaceInputs)
readVerifiedExactIfaceClosureWithCheckedValues = readVerifiedExactIfaceClosureWithCheckedValuesReader (BS.readFile . exactPath)

readVerifiedExactIfaceClosureWithCheckedValuesReader :: (ExactIfaceArtifact -> IO BS.ByteString)
  -> HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String CapturedInterfaceInputs)
readVerifiedExactIfaceClosureWithCheckedValuesReader readInput = readVerifiedExactIfaceClosureWithCheckedValuesUsing (readVerifiedOneWith readInput)

readVerifiedExactIfaceClosureWithCheckedValuesUsing :: (HscEnv -> ExactIfaceArtifact -> IO (Either String VerifiedExactInterface))
  -> HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String CapturedInterfaceInputs)
readVerifiedExactIfaceClosureWithCheckedValuesUsing readInterface env originals values
  | any (not . checkedValueOwner) values = pure (Left "checked value import has another owner")
  | length (map key values) /= Set.size (Set.fromList (map key values)) =
      pure (Left "duplicate checked value owner")
  | otherwise = do
      let originalKeys = Set.fromList (map key originals)
          inputs = originals ++ [value | value <- values, key value `Set.notMember` originalKeys]
      verified <- readVerifiedExactIfaceClosureUsing readInterface env inputs
      case verified of
        Left reason -> pure (Left reason)
        Right (CapturedInterfaceInputs captured) -> do
          aliases <- forM values $ \value -> case Map.lookup (key value) captured of
            Just (verified,known)
              | value `elem` known -> pure (Right (key value,(verified,known)))
              | let (original,_) = verifiedExactInterface verified
              , exactSha256 value == exactSha256 original
              , exactRequirements value == [] || exactRequirements value == exactRequirements original -> do
                  -- The closed original supplies the type dependency proof.
                  -- Verify this protected value's separate capture before its
                  -- path becomes an alias for those exact original bytes.
                  alias <- readInterface env value
                  pure ((\_ -> (key value,(verified,value:known))) <$> alias)
            _ -> pure (Left "checked value alias conflicts with its captured original")
          pure $ do
            additions <- sequence aliases
            Right (CapturedInterfaceInputs (Map.union (Map.fromList additions) captured))
  where key artifact = (exactUnit artifact,exactModule artifact)

-- Requirements are validated in the complete captured closure. A later
-- dependency-ordered injection may select one value whose type mentions
-- another original without rereading or weakening that original proof.
selectVerifiedExactInterfaces
  :: CapturedInterfaceInputs -> [ExactIfaceArtifact]
  -> Either String [(ExactIfaceArtifact, ModIface)]
selectVerifiedExactInterfaces (CapturedInterfaceInputs captured) artifacts =
  mapM select artifacts
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (verified,aliases) | artifact `elem` aliases -> Right (artifact,snd (verifiedExactInterface verified))
      _ -> Left "requested exact interface differs from its verified closure"

-- The admitted TYPE product keeps authenticated bytes and decoded Names with
-- their actual issuer. Its logical input alias is never a filesystem grant.
data CompletedInterfaceOwner = SessionHomeInterface CapturedSessionInterface | FinalizedHomeInterface CompletedModuleInterface

completedInterfaceHome :: CompletedInterfaceOwner -> HomeModInfo
completedInterfaceHome (SessionHomeInterface capture) = snd (capturedSessionCompilerProduct capture)
completedInterfaceHome (FinalizedHomeInterface original) = completedModuleInterfaceHome original

sameCompletedInterfaceOwner :: CompletedInterfaceOwner -> CompletedInterfaceOwner -> Bool
sameCompletedInterfaceOwner (SessionHomeInterface left) (SessionHomeInterface right) =
  sameCapturedSessionInterfaceOwner left right
sameCompletedInterfaceOwner (FinalizedHomeInterface left) (FinalizedHomeInterface right) =
  sameCompletedModuleInterfaceOwner left right
sameCompletedInterfaceOwner _ _ = False

data CapturedCompilerInterface = DecodedTypeInterface ModIface | CompletedHomeInterface CompletedInterfaceOwner

data VerifiedExactInterface = VerifiedExactInterface !ExactIfaceArtifact CapturedCompilerInterface !ArtifactBytes
  !(Char,MVar OrigNameCache)

verifiedExactInterface :: VerifiedExactInterface -> (ExactIfaceArtifact,ModIface)
verifiedExactInterface (VerifiedExactInterface artifact product' _ _) = (artifact,case product' of
  DecodedTypeInterface iface -> iface
  CompletedHomeInterface owner -> hm_iface (completedInterfaceHome owner))

verifiedInterfaceProducts :: CapturedInterfaceInputs -> [VerifiedExactInterface]
verifiedInterfaceProducts (CapturedInterfaceInputs captured) = map fst (Map.elems captured)

verifiedExactInterfaceBody :: VerifiedExactInterface -> ArtifactBytes
verifiedExactInterfaceBody (VerifiedExactInterface _ _ body _) = body

selectVerifiedExactInterfaceProducts :: CapturedInterfaceInputs -> [ExactIfaceArtifact]
  -> Either String [VerifiedExactInterface]
selectVerifiedExactInterfaceProducts (CapturedInterfaceInputs captured) = mapM select
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (VerifiedExactInterface _ product' body issuer,aliases) | artifact `elem` aliases ->
        Right (VerifiedExactInterface artifact product' body issuer)
      _ -> Left "requested exact interface differs from its verified closure"

-- Completed-value wire inputs bind bytes and selected exported Names. Their
-- type requirements come from this complete verified closure, including
-- worker-issued values created earlier in the same compiled cell.
selectVerifiedValueInterfaces
  :: CapturedInterfaceInputs -> [ExactIfaceArtifact]
  -> Either String [(ExactIfaceArtifact, ModIface)]
selectVerifiedValueInterfaces (CapturedInterfaceInputs captured) artifacts = mapM select artifacts
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (verified,aliases)
        | let (original,iface) = verifiedExactInterface verified
        , checkedValueOwner artifact
        , any (\alias -> exactPath alias == exactPath artifact
            && exactSha256 alias == exactSha256 artifact) aliases
        , exactRequirements artifact == [] || exactRequirements artifact == exactRequirements original ->
            Right (original,iface)
      _ -> Left "completed value input differs from its verified closure"

checkedValueImportAuthorityFromVerified
  :: CapturedInterfaceInputs -> [ExactIfaceArtifact]
  -> Either String CheckedValueImportAuthority
checkedValueImportAuthorityFromVerified closure artifacts
  | any (not . checkedValueOwner) artifacts = Left "checked value import has another owner"
  | otherwise = CheckedValueImportAuthority . Set.fromList
      . map (\(artifact, _) -> (exactUnit artifact, exactModule artifact))
      <$> selectVerifiedValueInterfaces closure artifacts

checkedValueOwner :: ExactIfaceArtifact -> Bool
checkedValueOwner artifact = checkedValueModuleOwner (exactUnit artifact) (exactModule artifact)

checkedValueModuleOwner :: String -> String -> Bool
checkedValueModuleOwner unit name = unit == "main" && case parseSessionModule name of
  Just owner -> smKind owner == ValMod && sessionModuleString owner == name
  Nothing -> False

noCheckedValueImports :: CheckedValueImportAuthority
noCheckedValueImports = CheckedValueImportAuthority Set.empty

-- Captures come from the actual session injection owner. Merely observing a
-- value-shaped name in an interface inventory or HPT cannot issue this proof.
checkedValueImportAuthorityFromCaptures
  :: [CapturedSessionInterface] -> Either String CheckedValueImportAuthority
checkedValueImportAuthorityFromCaptures captures = do
  owners <- mapM checked captures
  pure (CheckedValueImportAuthority (Set.fromList owners))
  where
    checked capture =
      let (owner,_) = capturedSessionInterface capture
          name = moduleNameString (moduleName owner)
          unit = unitString (moduleUnit owner)
      in if checkedValueModuleOwner unit name then Right (unit,name)
        else Left "injected checked value capture has another owner"

unionCheckedValueImports :: CheckedValueImportAuthority -> CheckedValueImportAuthority
  -> CheckedValueImportAuthority
unionCheckedValueImports (CheckedValueImportAuthority left) (CheckedValueImportAuthority right) =
  CheckedValueImportAuthority (Set.union left right)

admitsCheckedValueImport :: CheckedValueImportAuthority -> Module -> Bool
admitsCheckedValueImport (CheckedValueImportAuthority owners) owner =
  (unitString (moduleUnit owner),moduleNameString (moduleName owner)) `Set.member` owners

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

data ProtectedTemplateImport = ProtectedTemplateImport
  { protectedImportModule :: String
  , protectedImportLine :: Int
  , protectedImportOffset :: Int
  , protectedImportSource :: String
  , protectedImportShape :: TemplateImportShape
  }

data ProtectedTemplateImports = ProtectedTemplateImports String Bool [ProtectedTemplateImport]

data RenderedProtectedTemplateImports = RenderedProtectedTemplateImports
  { renderedProtectedSource :: String
  , renderedProtectedOccurrences :: [(String,Int,Int,String,TemplateImportShape)]
  }

data GeneratedScaffoldRecipe = GeneratedScaffoldRecipe FilePath String BS.ByteString Int
  [(CheckedTemplateInterface,Int,TemplateImportShape)] [CheckedTemplateInterface]
  GeneratedScaffoldInstanceScope GeneratedScaffoldPurpose [(Int,Int,ImportOrigin)]
  deriving (Eq)

newtype TemplateImportShape = TemplateImportShape ImportDescriptor
  deriving (Eq)

instance Show TemplateImportShape where
  show _ = "TemplateImportShape"

data GeneratedScaffoldInstanceScope = ImportedTemplateInstances | OriginalPreviewInstances CheckedTemplateInterface
  deriving (Eq)

data GeneratedScaffoldPurpose
  = NativeTurnTemplate
  | CheckingCellTemplate
  | TypedSegmentTemplate GeneratedSegmentOperations
  deriving (Eq, Show)

generatedScaffoldQualifier :: GeneratedScaffoldPurpose -> ModuleName
generatedScaffoldQualifier (TypedSegmentTemplate operations) = generatedSegmentQualifier operations
generatedScaffoldQualifier _ = mkModuleName "TidepoolResume"

isTypedSegmentPurpose :: GeneratedScaffoldPurpose -> Bool
isTypedSegmentPurpose TypedSegmentTemplate{} = True
isTypedSegmentPurpose _ = False

instance Show GeneratedScaffoldRecipe where
  show (GeneratedScaffoldRecipe path name _ line interfaces _ _ role _) =
    "GeneratedScaffoldRecipe " ++ show (path,name,line,interfaces,role)

captureProtectedTemplateImports :: DynFlags -> String -> Either String ProtectedTemplateImports
captureProtectedTemplateImports = captureProtectedTemplateImportsAt preambleImportMarker

captureProtectedTemplateImportsAt :: String -> DynFlags -> String -> Either String ProtectedTemplateImports
captureProtectedTemplateImportsAt marker flags source = do
  markerLine <- case lines marker of
    [line] -> Right line
    _ -> Left "protected template insertion marker must occupy one physical line"
  let sourceLines = zip [1..] (lines source)
      insertionBoundaries = [line | (line,textLine) <- sourceLines, textLine == markerLine]
      -- Only the reserved pragma slot precedes the header. Blanking it leaves
      -- every physical character/newline in place; parseHeader stops before
      -- declaration/body slots and does not inspect generated body syntax.
      headerSource = Text.unpack (Text.replace "{{CELL_PRAGMAS}}"
        (Text.replicate (length ("{{CELL_PRAGMAS}}" :: String)) " ") (Text.pack source))
  parsed <- case unP Parser.parseHeader (initParserState (initParserOpts flags)
      (stringToStringBuffer headerSource) (mkRealSrcLoc (mkFastString "<protected-template>") 1 1)) of
    PFailed _ -> Left "GHC could not parse the protected template header"
    POk _ header -> Right header
  occurrences <- forM (hsmodImports (unLoc parsed)) $ \located -> do
    (start,end) <- case getBufSpan (getLocA located) of
      Strict.Just span' -> Right (bufPos (bufSpanStart span'),bufPos (bufSpanEnd span'))
      Strict.Nothing -> Left "protected template import lacks its physical range"
    unless (0 <= start && start < end && end <= length source)
      (Left "protected template import leaves its captured source")
    let before = take start source
        first = start - length (takeWhile (/= '\n') (reverse before))
        declaration = unLoc located
        owner = moduleNameString (unLoc (ideclName declaration))
        line = 1 + length (filter (== '\n') before)
        imported = take (end-start) (drop start source)
    shape <- TemplateImportShape <$> either (Left . show) Right (describeImport declaration)
    pure (ProtectedTemplateImport owner line (start-first) imported shape)
  case insertionBoundaries of
    [] -> pure (ProtectedTemplateImports source False occurrences)
    [boundary]
      | all (\occurrence -> protectedImportLine occurrence
          + length (filter (== '\n') (protectedImportSource occurrence)) < boundary) occurrences ->
          pure (ProtectedTemplateImports source True occurrences)
      | otherwise -> Left "protected template import follows its mutable insertion marker"
    _ -> Left "protected template repeats its mutable insertion marker"

renderProtectedTemplateImports :: Int -> ProtectedTemplateImports -> Either String RenderedProtectedTemplateImports
renderProtectedTemplateImports linePrefix (ProtectedTemplateImports source hasBoundary occurrences)
  | linePrefix < 0 = Left "protected template line prefix is negative"
  | linePrefix > 0 && not hasBoundary =
      Left "protected template line mapping requires its mutable insertion marker"
  | otherwise = Right (RenderedProtectedTemplateImports source
      [(protectedImportModule occurrence, protectedImportLine occurrence + linePrefix, protectedImportOffset occurrence,
        protectedImportSource occurrence, protectedImportShape occurrence)
      | occurrence <- occurrences])

generatedScaffoldRecipe :: DynFlags -> CheckedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipe = generatedScaffoldRecipeFor NativeTurnTemplate

generatedCheckingTemplateRecipe :: DynFlags -> CheckedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedCheckingTemplateRecipe = generatedScaffoldRecipeFor CheckingCellTemplate

generatedCheckingTemplateRecipeWithProtectedImports :: DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedCheckingTemplateRecipeWithProtectedImports = generatedScaffoldRecipeWithProtectedImportsFor CheckingCellTemplate

generatedTypedSegmentRecipeWithProtectedImports :: GeneratedSegmentOperations -> DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedTypedSegmentRecipeWithProtectedImports operations =
  generatedScaffoldRecipeWithProtectedImportsFor (TypedSegmentTemplate operations)


generatedScaffoldRecipeFor :: GeneratedScaffoldPurpose -> DynFlags -> CheckedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipeFor purpose flags imports protectedTemplate rendered path name =
  case captureProtectedTemplateImports flags protectedTemplate >>= renderProtectedTemplateImports 0 of
    Left message -> pure (Left message)
    Right mapped -> generatedScaffoldRecipeWithProtectedImportsFor purpose flags imports mapped
      protectedTemplate rendered path name

generatedScaffoldRecipeWithProtectedImports :: DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipeWithProtectedImports = generatedScaffoldRecipeWithProtectedImportsFor NativeTurnTemplate

generatedScaffoldRecipeWithProtectedImportsFor :: GeneratedScaffoldPurpose -> DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipeWithProtectedImportsFor purpose flags
    imports mapped protectedTemplate rendered path name =
  generatedScaffoldRecipeWithOriginsFor purpose flags imports mapped protectedTemplate rendered [] path name

generatedScaffoldRecipeWithImportRendering :: DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> ImportRendering -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipeWithImportRendering = generatedScaffoldRecipeWithRenderingFor NativeTurnTemplate

generatedCheckingTemplateRecipeWithImportRendering :: DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> ImportRendering -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedCheckingTemplateRecipeWithImportRendering = generatedScaffoldRecipeWithRenderingFor CheckingCellTemplate

generatedTypedSegmentRecipeWithImportRendering :: GeneratedSegmentOperations -> DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> ImportRendering -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedTypedSegmentRecipeWithImportRendering operations = generatedScaffoldRecipeWithRenderingFor (TypedSegmentTemplate operations)

generatedScaffoldRecipeWithRenderingFor :: GeneratedScaffoldPurpose -> DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> ImportRendering -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipeWithRenderingFor purpose flags imports mapped protected rendering =
  generatedScaffoldRecipeWithOriginsFor purpose flags imports mapped protected
    (importRenderingSource rendering) (importRenderingOrigins rendering)

generatedScaffoldRecipeWithOriginsFor :: GeneratedScaffoldPurpose -> DynFlags -> CheckedTemplateImports
  -> RenderedProtectedTemplateImports -> String -> String -> [(Int,Int,ImportOrigin)] -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipeWithOriginsFor purpose _flags
    (CheckedTemplateImports roots interfaces) mapped protectedTemplate rendered origins path name = do
  canonical <- canonicalizePath path
  let compilerImport = "import qualified \"tidepool-resume\" Tidepool.Internal.Resume as "
        ++ moduleNameString (generatedScaffoldQualifier purpose)
      bytes = TextEncoding.encodeUtf8 (Text.pack rendered)
      mappedSource = renderedProtectedSource mapped
      mappedOccurrences = renderedProtectedOccurrences mapped
      mappedFor interface =
        [occurrence | occurrence@(owner,_,_,source,_) <- mappedOccurrences
        , owner == templateInterfaceModule interface, source /= compilerImport]
      compilerImportLines = [line | (_,line,_,source,_) <- mappedOccurrences, source == compilerImport]
      mappedOccurrence (_,line,offset,source,_) = do
        let first = sum (map ((+1) . length) (take (line-1) (lines rendered))) + offset
            renderedSource = take (length source) (drop first rendered)
        unless (renderedSource == source) (Left "checked template interface import is missing, duplicated, or changed")
      templateOccurrences interface = do
        let selected = mappedFor interface
        mapM_ mappedOccurrence selected
        pure [(interface,line,shape) | (_,line,_,_,shape) <- selected]
  pure $ do
    unless (mappedSource == protectedTemplate)
      (Left "protected template occurrence map belongs to another source")
    mapM_ mappedOccurrence mappedOccurrences
    let lineStart line = sum (map ((+1) . length) (take (line-1) (lines rendered)))
        protectedOrigins = [(lineStart line+offset,lineStart line+offset+length source,ProtectedCompiler)
          | (_,line,offset,source,_) <- mappedOccurrences]
    insertedOrigins <- forM origins $ \(start,end,origin) -> do
      unless (start >= 0 && end > start && end <= length rendered)
        (Left "rendered import occurrence leaves its captured source")
      pure (start,end,origin)
    let census = sortOn (\(start,_,_) -> start) (protectedOrigins ++ insertedOrigins)
    unless (and [end <= next | ((_,end,_),(next,_,_)) <- zip census (drop 1 census)])
      (Left "rendered import occurrences overlap")
    let rootSet = Set.fromList roots
        graphOwners = [(templateInterfaceUnit interface,templateInterfaceModule interface) | interface <- interfaces]
    unless (length roots == Set.size rootSet && all (`elem` graphOwners) roots)
      (Left "checked template roots are duplicated or leave their sealed graph")
    selected <- concat <$> mapM templateOccurrences
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
    case (purpose,compilerImportLines) of
      (TypedSegmentTemplate _,[line]) -> Right (GeneratedScaffoldRecipe canonical name bytes line selected closure ImportedTemplateInstances purpose census)
      (NativeTurnTemplate,[line]) -> Right (GeneratedScaffoldRecipe canonical name bytes line selected closure ImportedTemplateInstances purpose census)
      (CheckingCellTemplate,[]) -> Right (GeneratedScaffoldRecipe canonical name bytes 0 selected closure ImportedTemplateInstances purpose census)
      _ -> Left "generated scaffold support import differs from its template role"

-- Only the activation admission supplies this complete original instance
-- graph. Its synthetic edges affect GHC's instance traversal without exposing
-- any original lexical name or permitting an authored source import.
generatedActivationPreviewRecipe :: DynFlags -> [CheckedTemplateInterface] -> (String,String) -> String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedActivationPreviewRecipe flags interfaces originalOwner protectedTemplate rendered path name = do
  let roots = [(templateInterfaceUnit interface,templateInterfaceModule interface) | interface <- interfaces]
      imports = CheckedTemplateImports roots interfaces
  recipe <- generatedScaffoldRecipe flags imports protectedTemplate rendered path name
  pure $ do
    original <- recipe
    let GeneratedScaffoldRecipe canonical target bytes line selected _ _ role census = original
        owners = Set.fromList [(templateInterfaceUnit interface,templateInterfaceModule interface)
          | interface <- interfaces]
    unless (all (all (`Set.member` owners) . templateInterfaceImports) interfaces)
      (Left "activation preview edge leaves its original instance graph")
    originalTarget <- case [interface | interface <- interfaces
        , (templateInterfaceUnit interface,templateInterfaceModule interface) == originalOwner] of
      [interface] -> Right interface
      _ -> Left "activation preview lacks its sealed original target"
    pure (GeneratedScaffoldRecipe canonical target bytes line selected interfaces (OriginalPreviewInstances originalTarget) role census)

data CapturedGeneratedScaffoldSource = CapturedGeneratedScaffoldSource FilePath BS.ByteString
  [(Int,Int,ImportOrigin)]

capturedGeneratedScaffoldBytes :: CapturedGeneratedScaffoldSource -> BS.ByteString
capturedGeneratedScaffoldBytes (CapturedGeneratedScaffoldSource _ bytes _) = bytes

-- Recipes encode their rendered source as UTF-8; capture issues this owner
-- only after the file matches those exact bytes. Text consumers retain that
-- guarantee without decoding an unrelated raw-byte projection.
capturedGeneratedScaffoldText :: CapturedGeneratedScaffoldSource -> Text.Text
capturedGeneratedScaffoldText (CapturedGeneratedScaffoldSource _ bytes _) = TextEncoding.decodeUtf8 bytes

capturedGeneratedScaffoldPath :: CapturedGeneratedScaffoldSource -> FilePath
capturedGeneratedScaffoldPath (CapturedGeneratedScaffoldSource path _ _) = path

-- Call only after the root's effective flags and compile-purpose policy have
-- been admitted. Parsing does not load interfaces or execute finite TH syntax.
-- The resulting raw AST is retained for demand analysis and driver matching.
data CapturedGeneratedImports = CapturedGeneratedImports
  CapturedGeneratedScaffoldSource DynFlags CapturedImportSyntax [CapturedGeneratedImportOccurrence]

data CapturedGeneratedImportOccurrence = CapturedGeneratedImportOccurrence ImportOccurrence ImportOrigin

parseCapturedGeneratedImports :: DynFlags -> CapturedGeneratedScaffoldSource
  -> Either String CapturedGeneratedImports
parseCapturedGeneratedImports flags captured@(CapturedGeneratedScaffoldSource path _ _) =
  case unP Parser.parseModule parser of
    PFailed state -> Left (parseDiagnostics state)
    POk state parsed -> do
      unless (isEmptyMessages (getPsErrorMessages state))
        (Left (parseDiagnostics state))
      origins <- capturedSourceImportOrigins captured parsed
      syntax <- either (Left . show) Right (captureImportSyntax parsed)
      occurrences <- associate (capturedImportOccurrences syntax) origins
      pure (CapturedGeneratedImports captured flags syntax occurrences)
  where
    parseDiagnostics state = showPpr flags (getPsErrorMessages state)
    parser = initParserState (initParserOpts flags)
      (stringToStringBuffer (Text.unpack (capturedGeneratedScaffoldText captured)))
      (mkRealSrcLoc (mkFastString path) 1 1)
    associate [] [] = Right []
    associate (occurrence:occurrences) (origin:origins) =
      (CapturedGeneratedImportOccurrence occurrence origin :) <$> associate occurrences origins
    associate _ _ = Left "captured generated imports lost their checked origin association"

capturedGeneratedImportSource :: CapturedGeneratedImports -> CapturedGeneratedScaffoldSource
capturedGeneratedImportSource (CapturedGeneratedImports source _ _ _) = source

capturedGeneratedImportFlags :: CapturedGeneratedImports -> DynFlags
capturedGeneratedImportFlags (CapturedGeneratedImports _ flags _ _) = flags

capturedGeneratedImportSyntax :: CapturedGeneratedImports -> CapturedImportSyntax
capturedGeneratedImportSyntax (CapturedGeneratedImports _ _ syntax _) = syntax

capturedGeneratedImportOccurrences :: CapturedGeneratedImports -> [CapturedGeneratedImportOccurrence]
capturedGeneratedImportOccurrences (CapturedGeneratedImports _ _ _ occurrences) = occurrences

capturedGeneratedImportOccurrenceSyntax :: CapturedGeneratedImportOccurrence -> ImportOccurrence
capturedGeneratedImportOccurrenceSyntax (CapturedGeneratedImportOccurrence occurrence _) = occurrence

capturedGeneratedImportOccurrenceOrigin :: CapturedGeneratedImportOccurrence -> ImportOrigin
capturedGeneratedImportOccurrenceOrigin (CapturedGeneratedImportOccurrence _ origin) = origin

capturedSourceImportOrigins :: CapturedGeneratedScaffoldSource -> Located (HsModule GhcPs)
  -> Either String [ImportOrigin]
capturedSourceImportOrigins (CapturedGeneratedScaffoldSource _ _ census) parsed = do
  checked <- forM (hsmodImports (unLoc parsed)) $ \located -> do
    (start,end) <- case getBufSpan (getLocA located) of
      Strict.Just span' -> Right (bufPos (bufSpanStart span'),bufPos (bufSpanEnd span'))
      Strict.Nothing -> Left "parsed source import lacks a physical range"
    case [(origin,index) | (index,(first,last',origin)) <- zip [0::Int ..] census
        , first <= start, end <= last'] of
      [(origin,index)] -> Right (origin,index)
      _ -> Left "parsed source import differs from its rendered origin census"
  unless (sortOn id (map snd checked) == [0..length census-1])
    (Left "rendered source import census has an unused or repeated occurrence")
  pure (map fst checked)

captureGeneratedScaffoldSource :: GeneratedScaffoldRecipe -> FilePath -> IO (Either String CapturedGeneratedScaffoldSource)
captureGeneratedScaffoldSource (GeneratedScaffoldRecipe path _ expected _ _ _ _ _ census) requested = do
  canonical <- canonicalizePath requested
  actual <- BS.readFile canonical
  pure $ if canonical == path && actual == expected
    then Right (CapturedGeneratedScaffoldSource path expected census)
    else Left "generated scaffold target differs from its protected recipe"

captureGeneratedScaffoldTarget :: GeneratedScaffoldRecipe -> FilePath -> IO (Either String BS.ByteString)
captureGeneratedScaffoldTarget recipe requested =
  fmap capturedGeneratedScaffoldBytes <$> captureGeneratedScaffoldSource recipe requested

-- Interface-only template edges never enter the native/source replay inventory.
data GeneratedScaffoldImportAuthority = GeneratedScaffoldImportAuthority
  [(Module,Fingerprint,SrcSpan,ExactIfaceArtifact)]
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
    (\(target,fingerprint,span',artifact) -> ms_mod summary == target && ms_hs_hash summary == fingerprint
      && requested == (exactUnit artifact,exactModule artifact)
      && sameImportOccurrenceSpan (getLoc imported) span') scaffold

sameImportOccurrenceSpan :: SrcSpan -> SrcSpan -> Bool
sameImportOccurrenceSpan left right = left == right && case (getBufSpan left,getBufSpan right) of
  (Strict.Just before,Strict.Just after) -> before == after
  _ -> False

readGeneratedScaffoldImportAuthority :: CapturedInterfaceInputs -> GeneratedScaffoldRecipe
  -> CapturedGeneratedImports -> DriverImportSyntax -> ModuleGraph -> HscEnv
  -> IO (Either String GeneratedScaffoldImportAuthority)
readGeneratedScaffoldImportAuthority (CapturedInterfaceInputs captured)
    recipe@(GeneratedScaffoldRecipe path target expected _ templateInterfaces templateGraph instanceScope purpose census)
    (CapturedGeneratedImports (CapturedGeneratedScaffoldSource capturedPath capturedBytes capturedCensus) _ syntax classified)
    driver sourceGraph env = do
  let parsed = driverParsedModule driver
  checked <- captureGeneratedScaffoldTarget recipe path
  (source,fingerprint) <- sourceEvidenceWithFingerprint path
  targetPaths <- forM [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph
      , moduleNameString (moduleName (ms_mod summary)) == target] $ \summary ->
    traverse canonicalizePath (ml_hs_file (ms_location summary))
  supportInterface <- if purpose == CheckingCellTemplate
    then pure (Right Nothing)
    else fmap Just <$> resolveResumeInterface env
  pure $ do
    _ <- checked
    unless (capturedPath == path && capturedBytes == expected && capturedCensus == census)
      (Left "generated scaffold imports belong to another captured source")
    unless (dependencySourceSha256 source == hexBytes (SHA256.hash expected))
      (Left "generated scaffold source changed during admission")
    -- This is DATA consistency, not request/attempt identity. The private
    -- pipeline transition supplies the driver matched to this whole capture.
    let signature occurrence = (importOccurrenceId occurrence,importOccurrenceDescriptor occurrence)
        occurrences = driverImportOccurrences driver
        capturedOccurrences = capturedImportOccurrences syntax
        identities = map importOccurrenceId capturedOccurrences
        origins = Map.fromList [(importOccurrenceId occurrence,origin)
          | CapturedGeneratedImportOccurrence occurrence origin <- classified]
        declarations = hsmodImports (unLoc (pm_parsed_source parsed))
    unless (map signature occurrences == map signature capturedOccurrences
        && map (\(CapturedGeneratedImportOccurrence occurrence _) -> signature occurrence) classified
          == map signature capturedOccurrences
        && length identities == Set.size (Set.fromList identities)
        && length declarations == length occurrences)
      (Left "generated scaffold driver lost its captured import occurrences")
    let protected = [(occurrence,unLoc located) | (occurrence,located) <- zip occurrences declarations
          , Map.lookup (importOccurrenceId occurrence) origins == Just ProtectedCompiler]
        -- The span is borrowed from this occurrence's actual driver AST. It
        -- never chooses a role or links raw and preprocessed source buffers.
        consumed summary predicate declaration = case
            [name | (qualifier,name) <- ms_textual_imps summary
              , predicate qualifier
              , unLoc name == unLoc (ideclName declaration)
              , sameImportOccurrenceSpan (getLoc name) (getLocA (ideclName declaration))] of
          [name] -> Right name
          _ -> Left "generated scaffold import lacks its exact consumed occurrence"
    summary <- case [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph
        , moduleNameString (moduleName (ms_mod summary)) == target] of
      [summary] | ms_hs_hash summary == fingerprint && targetPaths == [Just path]
          && ms_mod summary == mkModule (stringToUnit (unitString (homeUnitId (hsc_home_unit env)))) (mkModuleName target)
          && ms_mod (pm_mod_summary parsed) == ms_mod summary
          && ms_hs_hash (pm_mod_summary parsed) == fingerprint -> Right summary
      _ -> Left "generated scaffold target summary differs from its protected source"
    support <- supportInterface
    forM_ support $ \iface -> do
      let owner = mi_module iface
          hiddenHomeWitnesses = filter (isHomeUnit (hsc_home_unit env) . moduleUnit)
            (dep_orphs (mi_deps iface) ++ dep_finsts (mi_deps iface))
      unless (null (mi_insts iface))
        (Left "generated scaffold support defines class instances")
      unless (null (mi_fam_insts iface))
        (Left "generated scaffold support defines family instances")
      unless (null hiddenHomeWitnesses)
        (Left "generated scaffold support imports home orphan or family witnesses")
      let exports = concatMap availNames (mi_exports iface)
      unless (all (\occurrence -> any (\name -> nameModule_maybe name == Just owner
          && occNameString (nameOccName name) == occurrence) exports) (if isTypedSegmentPurpose purpose
          then ["segmentPure", "segmentBind", "segmentFail", "settle", "resumeLifted"] else ["settle","resumeLifted"]))
        (Left "generated scaffold support has another export owner")
      declaration <- case [declaration | (_,declaration) <- protected
          , unLoc (ideclName declaration) == moduleName owner
          , ideclQualified declaration == QualifiedPre
          , fmap unLoc (ideclAs declaration) == Just (generatedScaffoldQualifier purpose)
          , ideclSource declaration == NotBoot && ideclImportList declaration == Nothing
          , case ideclPkgQual declaration of RawPkgQual package -> sl_fs package == fsLit "tidepool-resume"; _ -> False] of
        [declaration] -> Right declaration
        _ -> Left "generated scaffold package import lacks its protected occurrence"
      _ <- consumed summary (\qualifier -> case qualifier of
        OtherPkg package -> RealUnit (Definite package) == moduleUnit owner
        _ -> False) declaration
      pure ()
      -- This remains an ordinary external import. Package witnesses and the
      -- defining fat interface own its type and executable body evidence.
    templateNodes <- forM templateGraph $ \selected -> do
      let owner = (templateInterfaceUnit selected,templateInterfaceModule selected)
      (verified,_) <- maybe (Left "checked template graph interface is not retained") Right (Map.lookup owner captured)
      let (artifact,_) = verifiedExactInterface verified
      unless (exactSha256 artifact == templateInterfaceSha256 selected)
        (Left "checked template graph interface seal changed")
      pure (artifact,templateInterfaceImports selected)
    (reversedTemplateEdges,_) <- foldM (\(edges,remaining) (selected,_,expectedShape) -> do
      let owner = (templateInterfaceUnit selected,templateInterfaceModule selected)
          original = mkModule (stringToUnit (fst owner)) (mkModuleName (snd owner))
      unless (moduleUnit original == homeUnitAsUnit (hsc_home_unit env)
          && not (any (\case ModuleNode _ loaded -> ms_mod loaded == original; _ -> False)
            (mgModSummaries' sourceGraph)))
        (Left "checked template interface conflicts with current source")
      (verified,_) <- maybe (Left "checked template interface is not retained") Right
        (Map.lookup owner captured)
      let (artifact,iface) = verifiedExactInterface verified
      unless (exactSha256 artifact == templateInterfaceSha256 selected && mi_module iface == original)
        (Left "checked template interface seal changed")
      let matches (occurrence,declaration) =
            unLoc (ideclName declaration) == moduleName original
              && TemplateImportShape (importOccurrenceDescriptor occurrence) == expectedShape
              && ideclSource declaration == NotBoot
              && case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False
      (declaration,unclaimed) <- case break matches remaining of
        (before,(_,declaration):after) -> Right (declaration,before ++ after)
        _ -> Left "checked template interface lacks its protected structural occurrence"
      imported <- consumed summary (== NoPkgQual) declaration
      pure ((ms_mod summary,fingerprint,getLoc imported,artifact):edges,unclaimed))
      ([],protected) templateInterfaces
    let templateEdges = reverse reversedTemplateEdges
    orphanScopes <- case instanceScope of
      ImportedTemplateInstances -> pure []
      OriginalPreviewInstances originalTarget -> do
        let owner = (templateInterfaceUnit originalTarget,templateInterfaceModule originalTarget)
        (verified,_) <- maybe (Left "activation preview original target interface is not retained") Right
          (Map.lookup owner captured)
        let (artifact,iface) = verifiedExactInterface verified
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
    let instanceEdges = case instanceScope of
          ImportedTemplateInstances -> []
          OriginalPreviewInstances _ -> [(ms_mod summary,fingerprint,
            [(exactUnit artifact,exactModule artifact) | (artifact,_) <- templateNodes])]
    pure (GeneratedScaffoldImportAuthority templateEdges templateNodes instanceEdges orphanScopes)

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
  | ExactContextContainsForeignHome Module
  | ExactContextMissingHomeDependency Module Module
  deriving Eq

instance Show ExactContextForkFailure where
  show ExactContextRequiresCompilationManager = "exact context requires the compilation manager"
  show ExactContextRequiresSingleDefiniteHomeUnit = "exact context requires one definite HOME unit"
  show ExactContextContainsHomePackageInterface = "exact context contains a HOME package interface"
  show (ExactContextContainsForeignHome owner) =
    "exact context contains foreign HOME module " ++ exactModuleLabel owner
  show (ExactContextMissingHomeDependency owner dependency) =
    "exact context HOME module " ++ exactModuleLabel owner
      ++ " is missing dependency " ++ exactModuleLabel dependency

exactModuleLabel :: Module -> String
exactModuleLabel owner = unitString (moduleUnit owner) ++ ":" ++ moduleNameString (moduleName owner)

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

-- Completed HOME details retain their original HPT knot. They need no HOME
-- filesystem fallback in CompManager, but every recorded HOME dependency must
-- remain in that table. Publish no request-local Finder or source graph.
retainClosedHomeContext :: PackageFinderFacts -> Maybe Module -> HscEnv -> IO HscEnv
retainClosedHomeContext packages excluded env = do
  retained <- forkExactContextWithPackageFacts packages env
  let active = homeUnitAsUnit (hsc_home_unit retained)
      dependencyModules iface = Set.fromList $
        [mkModule (stringToUnit unit) (mkModuleName name)
        | (unit,name) <- homeInterfaceUsageOwners retained iface]
        ++ [mkModule (stringToUnit (unitIdString unit)) (gwib_mod name)
           | (unit,name) <- Set.toList (dep_direct_mods (mi_deps iface))]
        ++ dep_orphs (mi_deps iface) ++ dep_finsts (mi_deps iface)
      roots = Set.toAscList (Set.fromList
        [mi_module (hm_iface home) | home <- eltsHpt (hsc_HPT retained)
          , Just (mi_module (hm_iface home)) /= excluded])
      close seen [] = pure seen
      close seen ((issuer,owner):pending)
        | owner `Set.member` seen = close seen pending
        | otherwise = do
            unless (moduleUnit owner == active) (throwIO (ExactContextContainsForeignHome owner))
            home <- case lookupHpt (hsc_HPT retained) (moduleName owner) of
              Just selected | mi_module (hm_iface selected) == owner -> pure selected
              _ -> throwIO (ExactContextMissingHomeDependency issuer owner)
            let dependencies = [dependency | dependency <- Set.toAscList (dependencyModules (hm_iface home))
                  , moduleUnit dependency == active]
            close (Set.insert owner seen) ([(owner,dependency) | dependency <- dependencies] ++ pending)
  selected <- close Set.empty [(owner,owner) | owner <- roots]
  pure (hscUpdateHPT (filterHpt ((`Set.member` selected) . mi_module . hm_iface)) retained)
    {hsc_mod_graph=mkModuleGraph [],hsc_targets=[],hsc_type_env_vars=emptyKnotVars}

-- Reading every interface before installing any of them prevents a corrupt
-- member of an implementation SCC from partially mutating the HPT.
readExactIfaceArtifacts
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String [(ExactIfaceArtifact, ModIface)])
readExactIfaceArtifacts = readExactIfaceArtifactsWith (BS.readFile . exactPath)

readExactIfaceArtifactsWith :: (ExactIfaceArtifact -> IO BS.ByteString)
  -> HscEnv -> [ExactIfaceArtifact] -> IO (Either String [(ExactIfaceArtifact, ModIface)])
readExactIfaceArtifactsWith readInput = readExactIfaceArtifactsUsing (readOneWith readInput)

readExactIfaceArtifactsUsing :: (HscEnv -> ExactIfaceArtifact -> IO (Either String value))
  -> HscEnv -> [ExactIfaceArtifact] -> IO (Either String [value])
readExactIfaceArtifactsUsing readInterface env artifacts
  | Set.size moduleNames /= length artifacts = pure (Left "duplicate exact interface owner")
  | any (\artifact -> length (exactSha256 artifact) /= 64
      || not (all isHexDigit (exactSha256 artifact))) artifacts =
      pure (Left "invalid exact interface digest")
  | any (\artifact -> any (`Set.notMember` owners) (exactRequirements artifact)) artifacts =
      pure (Left "incomplete exact interface dependency closure")
  | otherwise = do
      timing <- readTimingEnabled
      timeDetailPhase timing "exact_scope" "verify_interfaces" $
        sequence <$> forM artifacts (readInterface env)
  where
    moduleNames = Set.fromList (map exactModule artifacts)
    owners = Set.fromList [(exactUnit artifact, exactModule artifact) | artifact <- artifacts]

-- A physical request owns lazy decoded interfaces separately from encoded
-- custody. ModIface retains Names but no HscEnv or captured-file path. Sharing
-- is valid only for the actual NameCache that issued those Names; a fresh cache
-- decodes independently even when every input digest is identical.
newtype RequestIfaceDecoder = RequestIfaceDecoder
  (MVar (IfaceRetention,[((Char,MVar OrigNameCache),Map.Map (String,String,String) VerifiedExactInterface)]))

data IfaceRetention = UnrestrictedIfaceRetention | RetainedIfaceSeals (Set.Set String)

instance Show RequestIfaceDecoder where show _ = "RequestIfaceDecoder"

newRequestIfaceDecoder :: IO RequestIfaceDecoder
newRequestIfaceDecoder = RequestIfaceDecoder <$> newMVar (UnrestrictedIfaceRetention,[])

pruneRequestIfaceDecoder :: Set.Set String -> RequestIfaceDecoder -> IO Int
pruneRequestIfaceDecoder selected (RequestIfaceDecoder state) = modifyMVarMasked state $ \(_,universes) -> do
  let retained = [(cache,chosen) | (cache,facts) <- universes
        , let chosen = Map.filterWithKey (\(_,_,sha) _ -> sha `Set.member` selected) facts
        , not (Map.null chosen)]
  -- Decoding follows scope acquisition. Later readers may use live captured
  -- inputs without reintroducing facts already excluded from inactive retention.
  count <- evaluate (sum [Map.size facts | (_,facts) <- retained])
  _ <- evaluate (Set.size selected)
  pure ((RetainedIfaceSeals selected,retained),count)

readCapturedExactIfaceArtifacts :: RequestIfaceDecoder -> (ExactIfaceArtifact -> IO ArtifactBytes)
  -> HscEnv -> [ExactIfaceArtifact] -> IO (Either String [(ExactIfaceArtifact,ModIface)])
readCapturedExactIfaceArtifacts decoder readInput = readExactIfaceArtifactsUsing (readCapturedOne decoder readInput)

readCapturedExactIfaceClosureWithCheckedValues :: RequestIfaceDecoder -> (ExactIfaceArtifact -> IO ArtifactBytes)
  -> HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact] -> IO (Either String CapturedInterfaceInputs)
readCapturedExactIfaceClosureWithCheckedValues decoder readInput =
  readVerifiedExactIfaceClosureWithCheckedValuesUsing (readCapturedVerifiedOne decoder readInput)

readCapturedOne :: RequestIfaceDecoder -> (ExactIfaceArtifact -> IO ArtifactBytes)
  -> HscEnv -> ExactIfaceArtifact -> IO (Either String (ExactIfaceArtifact,ModIface))
readCapturedOne decoder readInput env artifact =
  fmap verifiedExactInterface <$> readCapturedVerifiedOne decoder readInput env artifact

readCapturedVerifiedOne :: RequestIfaceDecoder -> (ExactIfaceArtifact -> IO ArtifactBytes)
  -> HscEnv -> ExactIfaceArtifact -> IO (Either String VerifiedExactInterface)
readCapturedVerifiedOne (RequestIfaceDecoder state) readInput env artifact = do
  -- HscEnv unpacks NameCache: its selector can rebox the record on every
  -- call. The mutable intern-table cell has identity even across that reboxing;
  -- the unique character also belongs to the name issuer.
  let names = hsc_NC env
      cache = (nsUniqChar names, nsNames names)
  let key = (exactUnit artifact,exactModule artifact,exactSha256 artifact)
  modifyMVar state $ \(retention,universes) -> do
    let known = maybe Map.empty id (lookup cache universes)
    case Map.lookup key known of
      Just (VerifiedExactInterface _ iface body issuer) -> do
        -- Authorization and a relocated alias's seal still belong to the
        -- consuming scope, even though decoding has already completed.
        token <- readInput artifact
        unless (artifactSha256 token == exactSha256 artifact) (fail "captured interface token differs from requested seal")
        timing <- readTimingEnabled
        emitCount timing ("exact_iface_decode_reuse." ++ exactModule artifact) 1
        pure ((retention,universes),Right (VerifiedExactInterface artifact iface body issuer))
      Nothing -> do
        token <- readInput artifact
        unless (artifactSha256 token == exactSha256 artifact) (fail "captured interface token differs from requested seal")
        result <- readVerifiedOneTokenWith (const (pure token)) env artifact
        let retained = case result of
              Right verified | retains retention -> (cache,Map.insert key verified known) : filter ((/= cache) . fst) universes
              Right _ -> universes
              Left _ -> universes
        pure ((retention,retained),result)
  where
    retains UnrestrictedIfaceRetention = True
    retains (RetainedIfaceSeals selected) = exactSha256 artifact `Set.member` selected

readOneWith :: (ExactIfaceArtifact -> IO BS.ByteString) -> HscEnv -> ExactIfaceArtifact
  -> IO (Either String (ExactIfaceArtifact, ModIface))
readOneWith readInput env artifact = fmap verifiedExactInterface <$> readVerifiedOneWith readInput env artifact

readVerifiedOneWith :: (ExactIfaceArtifact -> IO BS.ByteString) -> HscEnv -> ExactIfaceArtifact
  -> IO (Either String VerifiedExactInterface)
readVerifiedOneWith readInput = readVerifiedOneTokenWith (fmap captureArtifactBytes . readInput)

readVerifiedOneTokenWith :: (ExactIfaceArtifact -> IO ArtifactBytes) -> HscEnv -> ExactIfaceArtifact
  -> IO (Either String VerifiedExactInterface)
readVerifiedOneTokenWith readInput env artifact = do
  timing <- readTimingEnabled
  emitCount timing ("exact_iface_read_calls." ++ exactModule artifact) 1
  readResult <- try (readInput artifact) :: IO (Either IOException ArtifactBytes)
  case readResult of
    Left _ -> pure (Left ("interface unavailable: " ++ exactModule artifact))
    Right body -> do
      emitCount timing ("exact_iface_read_bytes." ++ exactModule artifact) (fromIntegral (BS.length (artifactBytes body)))
      readVerified timing body
  where
   readVerified timing body =
    if artifactSha256 body /= map toLower (exactSha256 artifact)
    then pure (Left ("interface digest mismatch: " ++ exactModule artifact))
    else do
      -- Decode the exact bytes that passed the digest check. Reading the
      -- candidate path again would admit a different interface between the
      -- hash and GHC's decoder, even if a later revalidation observed the
      -- original bytes restored.
      let owner = mkModule (stringToUnit (exactUnit artifact))
            (mkModuleName (exactModule artifact))
      decoded <- try @SomeException (withCapturedIface timing (exactModule artifact) (artifactBytes body) $ \path -> do
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
            | otherwise -> Right (VerifiedExactInterface artifact (DecodedTypeInterface iface) body
                (nsUniqChar (hsc_NC env),nsNames (hsc_NC env)))

data OriginalInterfaceCaptureFailure
  = OriginalInterfaceChanged String String
  | ConflictingOriginalInterfaces
  | FinalizedOriginalConflict String String
  deriving Show

instance Exception OriginalInterfaceCaptureFailure

-- Original product publication and type witnesses share the same serializer
-- and immutable bytes. This cache belongs only to the completed transaction;
-- it neither consults source nor survives in a worker-global map.
data OriginalInterfaceArtifacts = OriginalInterfaceArtifacts
  HscEnv (Map.Map Module FinalizedModule) (Map.Map Module (Maybe ExactIfaceArtifact,ArtifactBytes)) [OriginalSessionInterface] FilePath
  (IORef (Map.Map Module (Maybe ArtifactBytes)))
  (Module -> Maybe (ExactIfaceArtifact,ArtifactBytes))

data SessionCaptureOrigin = ImportedSessionCapture | ProducedSessionCapture
  deriving Eq

data OriginalSessionInterface = OriginalSessionInterface SessionCaptureOrigin CapturedSessionInterface

-- Only finalization products and explicit admitted captures can supply a home
-- owner. A checking HPT entry is provisional and cannot issue original bytes.
newOriginalInterfaceArtifacts :: HscEnv -> Map.Map ModuleName FinalizedModule
  -> [ExactIfaceArtifact] -> FilePath -> IO OriginalInterfaceArtifacts
newOriginalInterfaceArtifacts env finalized retained =
  newOriginalInterfaceArtifactsWithSessionCaptures env finalized retained []

-- | Hydrated session snapshots supply type-dependency seals alongside exact
-- captures, including compiler-produced typed outputs. They do not enter the
-- finalized-source or executable inventory or authorize live values.
newOriginalInterfaceArtifactsWithSessionCaptures :: HscEnv -> Map.Map ModuleName FinalizedModule
  -> [ExactIfaceArtifact] -> [CapturedSessionInterface] -> FilePath -> IO OriginalInterfaceArtifacts
newOriginalInterfaceArtifactsWithSessionCaptures env finalized retained injected =
  newOriginalInterfaceArtifactsWithSessionOutputs env finalized retained injected []

newOriginalInterfaceArtifactsWithSessionOutputs :: HscEnv -> Map.Map ModuleName FinalizedModule
  -> [ExactIfaceArtifact] -> [CapturedSessionInterface] -> [CapturedSessionInterface]
  -> FilePath -> IO OriginalInterfaceArtifacts
newOriginalInterfaceArtifactsWithSessionOutputs = newOriginalInterfaceArtifactsWithReader (fmap captureArtifactBytes . BS.readFile . exactPath)

newOriginalInterfaceArtifactsWithReader :: (ExactIfaceArtifact -> IO ArtifactBytes)
  -> HscEnv -> Map.Map ModuleName FinalizedModule
  -> [ExactIfaceArtifact] -> [CapturedSessionInterface] -> [CapturedSessionInterface]
  -> FilePath -> IO OriginalInterfaceArtifacts
newOriginalInterfaceArtifactsWithReader readInput =
  newOriginalInterfaceArtifactsWithLookup readInput (const Nothing)

newOriginalInterfaceArtifactsWithLookup :: (ExactIfaceArtifact -> IO ArtifactBytes)
  -> (Module -> Maybe (ExactIfaceArtifact,ArtifactBytes))
  -> HscEnv -> Map.Map ModuleName FinalizedModule
  -> [ExactIfaceArtifact] -> [CapturedSessionInterface] -> [CapturedSessionInterface]
  -> FilePath -> IO OriginalInterfaceArtifacts
newOriginalInterfaceArtifactsWithLookup readInput borrow env finalized retained injected produced directory = do
  captures <- forM retained $ \artifact -> do
    body <- readInput artifact
    unless (artifactSha256 body == exactSha256 artifact) $
      throwIO (OriginalInterfaceChanged (exactUnit artifact) (exactModule artifact))
    let owner = mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
    pure (owner, (Just artifact,body))
  let originals = Map.fromList
        [(mi_module (hm_iface (finalizedHomeModInfo original)), original)
        | original <- Map.elems finalized]
      snapshotsWithOrigin = [(ImportedSessionCapture,snapshot) | snapshot <- injected]
        ++ [(ProducedSessionCapture,snapshot) | snapshot <- produced]
      selected = captures ++ [(owner,(Nothing,body)) | (_,snapshot) <- snapshotsWithOrigin
        , let (owner,body) = capturedSessionInterfaceBody snapshot]
      preserveDescriptor new@(Just _,_) _ = new
      preserveDescriptor _ old = old
      admitted = Map.fromListWith preserveDescriptor selected
      matches (owner,(descriptor,body)) = case Map.lookup owner admitted of
        Just (actual,bytes) -> bytes == body && case (descriptor,actual) of
          (Just requested,Just issued) -> sameExactInterfaceIdentity requested issued
          (Just _,Nothing) -> False
          _ -> True
        Nothing -> False
  unless (all matches selected) $
    throwIO ConflictingOriginalInterfaces
  let snapshots = Map.fromList [(owner,(origin,snapshot)) | (origin,snapshot) <- snapshotsWithOrigin
        , let (owner,_) = capturedSessionInterface snapshot]
  unless (all (\(origin,snapshot) -> let (owner,bytes) = capturedSessionInterface snapshot
          in case Map.lookup owner snapshots of
            Just (selectedOrigin,selectedSnapshot) -> selectedOrigin == origin
              && sameCapturedSessionInterfaceOwner selectedSnapshot snapshot
              && capturedSessionInterface selectedSnapshot == (owner,bytes)
              && capturedSessionInterfaceEvidence selectedSnapshot == capturedSessionInterfaceEvidence snapshot
            Nothing -> False) snapshotsWithOrigin) $ throwIO ConflictingOriginalInterfaces
  let paired = [OriginalSessionInterface origin snapshot | (origin,snapshot) <- Map.elems snapshots]
  captured <- newIORef Map.empty
  pure (OriginalInterfaceArtifacts env originals admitted paired directory captured borrow)

originalSessionInterfaces :: OriginalInterfaceArtifacts -> [CapturedSessionInterface]
originalSessionInterfaces (OriginalInterfaceArtifacts _ _ _ selected _ _ _) =
  [snapshot | OriginalSessionInterface ImportedSessionCapture snapshot <- selected]

originalProducedSessionInterfaces :: OriginalInterfaceArtifacts -> [CapturedSessionInterface]
originalProducedSessionInterfaces (OriginalInterfaceArtifacts _ _ _ selected _ _ _) =
  [snapshot | OriginalSessionInterface ProducedSessionCapture snapshot <- selected]

originalInterfaceBytes :: OriginalInterfaceArtifacts -> Module -> IO (Maybe BS.ByteString)
originalInterfaceBytes artifacts owner = fmap artifactBytes <$> originalInterfaceBody artifacts owner

originalInterfaceSha256 :: OriginalInterfaceArtifacts -> Module -> IO (Maybe String)
originalInterfaceSha256 artifacts owner = fmap artifactSha256 <$> originalInterfaceBody artifacts owner

originalRetainedInterface :: OriginalInterfaceArtifacts -> Module -> Maybe (ExactIfaceArtifact,ArtifactBytes)
originalRetainedInterface (OriginalInterfaceArtifacts _ _ retained _ _ _ borrow) owner =
  case Map.lookup owner retained of
    Just (Just artifact,body) -> Just (artifact,body)
    Just _ -> Nothing
    Nothing -> borrow owner

-- Explicit admitted input captures keep their original descriptor and bytes.
-- Provisional HPT homes and descriptor-less session snapshots are excluded.
originalRetainedInterfaceCaptures :: OriginalInterfaceArtifacts
  -> Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes)
originalRetainedInterfaceCaptures (OriginalInterfaceArtifacts _ _ retained _ _ _ _) = Map.fromList
  [((exactUnit artifact,exactModule artifact),(artifact,body))
  | (Just artifact,body) <- Map.elems retained]

originalInterfaceBody :: OriginalInterfaceArtifacts -> Module
  -> IO (Maybe ArtifactBytes)
originalInterfaceBody (OriginalInterfaceArtifacts env originals retained _ directory captured borrow) owner = do
  known <- Map.lookup owner <$> readIORef captured
  case known of
    Just artifact -> pure artifact
    Nothing -> do
      external <- hscEPS env
      let matches interface = if mi_module interface == owner then Just interface else Nothing
          productInterface = Map.lookup owner originals >>= matches . hm_iface . finalizedHomeModInfo
          packageInterface = lookupModuleEnv (eps_PIT external) owner >>= matches
      retainedBody <- case Map.lookup owner retained of
        Just (_,bytes) -> pure (Just bytes)
        Nothing -> case borrow owner of
          Nothing -> pure Nothing
          Just (descriptor,bytes) -> do
            unless (exactUnit descriptor == unitString (moduleUnit owner)
                && exactModule descriptor == moduleNameString (moduleName owner)
                && exactSha256 descriptor == artifactSha256 bytes) $
              throwIO (OriginalInterfaceChanged (unitString (moduleUnit owner))
                (moduleNameString (moduleName owner)))
            pure (Just bytes)
      artifact <- case productInterface of
        Just _ -> do
          original <- maybe (fail "finalized interface has no semantic owner") pure (Map.lookup owner originals)
          finalized <- Just <$> captureFinalizedInterface original
          case (retainedBody, finalized) of
            (Just admitted, Just bytes) | admitted /= bytes ->
              throwIO (FinalizedOriginalConflict (unitString (moduleUnit owner)) (moduleNameString (moduleName owner)))
            _ -> pure finalized
        Nothing -> case retainedBody of
          Just bytes -> pure (Just bytes)
          Nothing | toUnitId (moduleUnit owner) `Set.member` hsc_all_home_unit_ids env -> pure Nothing
          Nothing -> packageArtifact packageInterface
      modifyIORef' captured (Map.insert owner artifact)
      pure artifact
  where
    seal bytes = Just (captureArtifactBytes bytes)
    serialize interface = seal <$> serializeOriginalInterface env directory interface
    sameOriginal selected actual = mi_module actual == owner
      && mi_iface_hash (mi_final_exts actual) == mi_iface_hash (mi_final_exts selected)
    packageArtifact selected = do
      -- Resolving a wired-in Name can supply its TyThing without loading its
      -- interface. Sealing still requires the original package interface;
      -- load it through the pinned module environment before capturing bytes.
      admitted <- case selected of
        Just interface -> pure (Just interface)
        Nothing -> initIfaceLoad env
          (loadInterface (text "original type witness interface") owner ImportBySystem)
          >>= \loaded -> pure $ case loaded of
            MErr.Succeeded interface | mi_module interface == owner -> Just interface
            MErr.Succeeded _ -> Nothing
            MErr.Failed _ -> Nothing
      case admitted of
        Nothing -> pure Nothing
        Just original -> do
          full <- readPackageInterface env owner
          case full of
            Right (interface, location)
              | mi_module interface == owner
              , sameOriginal original interface ->
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

-- GHC's home-interface knot allows mutually recursive source/boot modules to
-- resolve each other's original Names while typechecking their details.
hydrateExactScope
  :: HscEnv -> [(ExactIfaceArtifact, ModIface)] -> IO HscEnv
hydrateExactScope env loaded = do
  forM_ loaded $ \(artifact, iface) -> do
    let owner = mi_module iface
    unless (owner == mkModule (stringToUnit (exactUnit artifact))
        (mkModuleName (exactModule artifact))) $
      fail "exact hydration interface owner differs from its selected artifact"
  hydrateOriginalInterfaces env (map snd loaded)

-- The selected HPT, captured inputs and actual environment are one issued
-- owner. A later installer cannot substitute another environment for its homes.
data HydratedExactInterfaces = HydratedExactInterfaces HscEnv CapturedInterfaceInputs
  [(ExactIfaceArtifact,HomeModInfo)]

hydratedExactEnvironment :: HydratedExactInterfaces -> HscEnv
hydratedExactEnvironment (HydratedExactInterfaces environment _ _) = environment

hydratedExactInputs :: HydratedExactInterfaces -> CapturedInterfaceInputs
hydratedExactInputs (HydratedExactInterfaces _ inputs _) = inputs

hydratedExactInterfaces :: HydratedExactInterfaces -> [(ExactIfaceArtifact,HomeModInfo)]
hydratedExactInterfaces (HydratedExactInterfaces _ _ homes) = homes

hydrateInterfaceInputs :: HscEnv -> CapturedInterfaceInputs -> IO HydratedExactInterfaces
hydrateInterfaceInputs env inputs = do
  let verified = verifiedInterfaceProducts inputs
  forM_ verified $ \(VerifiedExactInterface _ _ _ issuer) ->
    unless (issuer == (nsUniqChar (hsc_NC env),nsNames (hsc_NC env)))
      (throwIO InterfaceNameCacheMismatch)
  let actualHomes = [completedInterfaceHome owner | VerifiedExactInterface _ (CompletedHomeInterface owner) _ _ <- verified]
      initial = hscUpdateHPT_lazy (\table -> foldr
        (\home current -> addToHpt current (moduleName (mi_module (hm_iface home))) home) table actualHomes) env
      pending = [iface | VerifiedExactInterface _ (DecodedTypeInterface iface) _ _ <- verified]
  completed <- hydrateOriginalInterfaces initial pending
  homes <- forM verified $ \selected -> do
    let (artifact,iface) = verifiedExactInterface selected
    home <- maybe (throwIO (InterfaceHydratedHomeMissing (exactUnit artifact,exactModule artifact))) pure
      (lookupHpt (hsc_HPT completed) (moduleName (mi_module iface)))
    unless (mi_module (hm_iface home) == mi_module iface
        && mi_hsc_src (hm_iface home) == mi_hsc_src iface
        && mi_iface_hash (mi_final_exts (hm_iface home)) == mi_iface_hash (mi_final_exts iface))
      (throwIO (InterfaceHydratedHomeMismatch (exactUnit artifact,exactModule artifact)))
    pure (artifact,home)
  pure (HydratedExactInterfaces completed inputs homes)

-- Make constructs a phase environment. Borrowing the same selected owner
-- restores its actual HMI knots after checking the phase's issuer and any
-- existing same-owner entries; equal fingerprints cannot replace those knots.
restoreHydratedInterfaceEnvironment :: HydratedExactInterfaces -> HscEnv -> IO HydratedExactInterfaces
restoreHydratedInterfaceEnvironment (HydratedExactInterfaces original inputs homes) phase = do
  unless ((nsUniqChar (hsc_NC original),nsNames (hsc_NC original))
      == (nsUniqChar (hsc_NC phase),nsNames (hsc_NC phase)))
    (throwIO InterfaceNameCacheMismatch)
  forM_ homes $ \(_,home) -> forM_ (lookupHpt (hsc_HPT phase) (moduleName (mi_module (hm_iface home)))) $ \present ->
    unless (mi_module (hm_iface present) == mi_module (hm_iface home)
        && mi_hsc_src (hm_iface present) == mi_hsc_src (hm_iface home)
        && mi_iface_hash (mi_final_exts (hm_iface present)) == mi_iface_hash (mi_final_exts (hm_iface home)))
      (throwIO (InterfacePhaseHomeConflict
        (unitString (moduleUnit (mi_module (hm_iface home))),moduleNameString (moduleName (mi_module (hm_iface home))))))
  let restored = hscUpdateHPT_lazy (\table -> foldr
        (\(_,home) current -> addToHpt current (moduleName (mi_module (hm_iface home))) home) table homes) phase
  pure (HydratedExactInterfaces restored inputs homes)

hydratedInterfaceProduct :: HydratedExactInterfaces -> ExactIfaceArtifact
  -> Either String (VerifiedExactInterface,HomeModInfo)
hydratedInterfaceProduct (HydratedExactInterfaces _ inputs@(CapturedInterfaceInputs captured) homes) artifact = do
  if checkedValueOwner artifact
    then do _ <- selectVerifiedValueInterfaces inputs [artifact]; pure ()
    else do _ <- selectVerifiedExactInterfaceProducts inputs [artifact]; pure ()
  (verified,_) <- maybe (Left "hydrated selection lacks its captured interface") Right
    (Map.lookup (exactUnit artifact,exactModule artifact) captured)
  home <- case [selected | (actual,selected) <- homes
      , (exactUnit actual,exactModule actual) == (exactUnit artifact,exactModule artifact)] of
    [selected] -> Right selected
    _ -> Left "hydrated selection lacks its actual HOME association"
  pure (verified,home)

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
projectExactLexicalGraph
  :: ModuleGraph -> [(ExactIfaceArtifact, [(String,String)])]
  -> CheckedValueImportAuthority -> GeneratedScaffoldImportAuthority -> HscEnv
  -> Either String (HscEnv,[(ExactIfaceArtifact,[(String,String)])])
projectExactLexicalGraph sourceGraph lexical (CheckedValueImportAuthority checkedValues)
    authority@(GeneratedScaffoldImportAuthority _ scaffoldGraph instanceEdges orphanScopes) env
  | not (null conflictingRows) = (Left "checked template graph conflicts with current exact lexical graph")
  | Set.size lexicalModuleNames /= length lexical = (Left "duplicate virtual lexical owner")
  | Set.size virtualModuleNames /= length virtualRows = (Left "duplicate virtual graph module name")
  | any (\(artifact, _) -> exactUnit artifact /= unitString home) virtualRows =
      (Left "virtual graph owner is outside active home unit")
  | any (\(_, deps) -> any (`Set.notMember` virtualOwners) deps) virtualRows =
      (Left "virtual lexical edge leaves admitted graph")
  | any (\(_,_,deps) -> any (`Set.notMember` virtualOwners) deps) instanceEdges =
      (Left "activation preview instance edge leaves admitted graph")
  | any (\(target,fingerprint,_) -> length [() | ModuleNode _ summary <- mgModSummaries' sourceGraph
      , ms_mod summary == target && ms_hs_hash summary == fingerprint] /= 1) instanceEdges =
      (Left "activation preview instance target differs from its protected recipe")
  | any (\(OriginalPreviewOrphans target fingerprint _) ->
      not (any (\(owner,seal,_) -> owner == target && seal == fingerprint) instanceEdges)) orphanScopes =
      (Left "activation preview orphan scope differs from its protected instance target")
  | any (\node -> case node of
      ModuleNode _ summary -> keyOf summary `Set.member` Set.union virtualOwners checkedValues
      _ -> False) (mgModSummaries' sourceGraph) =
      (Left "virtual lexical owner collides with source graph")
  | any (\(artifact, _) -> case lookupHpt (hsc_HPT env)
        (mkModuleName (exactModule artifact)) of
      Nothing -> True
      Just hmi -> mi_module (hm_iface hmi) /=
        mkModule (stringToUnit (exactUnit artifact))
          (mkModuleName (exactModule artifact))) virtualRows =
      (Left "virtual lexical interface missing from exact HPT")
  | not (null unadmittedHomeEdges) =
      (Left ("source graph imports unadmitted home implementation: "
        ++ show unadmittedHomeEdges))
  | otherwise = Right ((withOriginalPreviewOrphans orphanScopes env)
      {hsc_mod_graph=mkModuleGraph (sourceNodes ++ virtualNodes)},virtualRows)
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
      [ModuleNode (map nodeKey deps) (exactInterfaceSummary env artifact) | (artifact,deps) <- virtualRows]

-- GHC downsweep indexes every retained summary by its source path. Exact
-- lexical/linker nodes have no source and belong only to their admitted graph.
-- CPP summaries also need fresh preprocessing to observe changed includes.
depanalSourceModules :: [ModuleName] -> Ghc ModuleGraph
depanalSourceModules excluded = do
  env <- getSession
  let sourceSummary (ModuleNode _ summary) =
        isJust (ml_hs_file (ms_location summary))
          && not (xopt LangExt.Cpp (ms_hspp_opts summary))
      sourceSummary _ = True
  setSession env {hsc_mod_graph = mkModuleGraph
    (filter sourceSummary (mgModSummaries' (hsc_mod_graph env)))}
  depanal excluded False

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
