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
  , HydratedExactInterfaces, hydrateExactScopeWithProducts, hydratedExactInterfaces
  , exactHomeInstancesFor, withExactHomeInstances
  , CheckedValueImportAuthority
  , checkedValueOwner, noCheckedValueImports
  , checkedValueImportAuthorityFromCaptures, unionCheckedValueImports, admitsCheckedValueImport
  , readCheckedValueImportAuthority
  , VerifiedExactIfaceClosure
  , readVerifiedExactIfaceClosure
  , readVerifiedExactIfaceClosureWithCheckedValues, readVerifiedExactIfaceClosureWithCheckedValuesReader
  , selectVerifiedExactInterfaces
  , VerifiedExactInterface, selectVerifiedExactInterfaceProducts, verifiedExactInterface, verifiedExactInterfaceBody
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
  , capturedGeneratedScaffoldBytes, capturedGeneratedScaffoldPath
  , capturedSourceImportOrigins
  , CapturedGeneratedImports, parseCapturedGeneratedImports
  , capturedGeneratedImportSource, capturedGeneratedImportFlags, capturedGeneratedImportSyntax
  , CapturedGeneratedImportOccurrence, capturedGeneratedImportOccurrences
  , capturedGeneratedImportOccurrenceSyntax, capturedGeneratedImportOccurrenceOrigin
  , permitsGeneratedScaffoldImport
  , GeneratedScaffoldImportAuthority, noGeneratedScaffoldImports, readGeneratedScaffoldImportAuthority
  , installExactLexicalGraphWithScaffold
  , installExactLexicalGraph
  ) where

import Tidepool.ArtifactBytes (ArtifactBytes, captureArtifactBytes, artifactBytes, artifactSha256)
import Tidepool.Timing (readTimingEnabled, emitCount, timeDetailPhase)
import Tidepool.TypedSegment.Types (GeneratedSegmentOperations, generatedSegmentQualifier)
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString, CapturedSessionInterface, capturedSessionInterfaceBody, capturedSessionInterface, capturedSessionInterfaceEvidence)
import Control.Monad (forM, forM_, unless, foldM)
import Control.Concurrent.MVar (MVar, newMVar, modifyMVar, modifyMVarMasked)
import GHC.Types.Name.Cache (NameCache(nsNames,nsUniqChar), OrigNameCache)
import Control.Monad.IO.Class (liftIO)
import Data.List (mapAccumL)
import Control.Exception
  ( Exception, IOException, SomeException, SomeAsyncException, bracket, try, fromException, throwIO, evaluate )
import Data.Char (isHexDigit, toLower)
import Data.Maybe (isJust, isNothing)
import Data.List (sortOn)
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
import GHC.Unit.Finder (addHomeModuleToFinder)
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
import Tidepool.ExecutionSource (ExecutionSourceIdentity(..))
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
import Tidepool.FinalizedModule (FinalizedModule, finalizedHomeModInfo, captureFinalizedInterface, homeInterfaceUsageOwners)
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

newtype VerifiedExactIfaceClosure = VerifiedExactIfaceClosure
  (Map.Map (String, String) (VerifiedExactInterface, [ExactIfaceArtifact]))

readVerifiedExactIfaceClosure
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosure = readVerifiedExactIfaceClosureWith (BS.readFile . exactPath)

readVerifiedExactIfaceClosureWith :: (ExactIfaceArtifact -> IO BS.ByteString)
  -> HscEnv -> [ExactIfaceArtifact] -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosureWith readInput = readVerifiedExactIfaceClosureUsing (readVerifiedOneWith readInput)

readVerifiedExactIfaceClosureUsing :: (HscEnv -> ExactIfaceArtifact -> IO (Either String VerifiedExactInterface))
  -> HscEnv -> [ExactIfaceArtifact] -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosureUsing readInterface env artifacts = fmap (VerifiedExactIfaceClosure . Map.fromList
  . map (\verified -> let (artifact,_) = verifiedExactInterface verified
      in ((exactUnit artifact,exactModule artifact),(verified,[artifact]))))
  <$> readExactIfaceArtifactsUsing readInterface env artifacts

readVerifiedExactIfaceClosureWithCheckedValues
  :: HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosureWithCheckedValues = readVerifiedExactIfaceClosureWithCheckedValuesReader (BS.readFile . exactPath)

readVerifiedExactIfaceClosureWithCheckedValuesReader :: (ExactIfaceArtifact -> IO BS.ByteString)
  -> HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosureWithCheckedValuesReader readInput = readVerifiedExactIfaceClosureWithCheckedValuesUsing (readVerifiedOneWith readInput)

readVerifiedExactIfaceClosureWithCheckedValuesUsing :: (HscEnv -> ExactIfaceArtifact -> IO (Either String VerifiedExactInterface))
  -> HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String VerifiedExactIfaceClosure)
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
        Right (VerifiedExactIfaceClosure captured) -> do
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
      Just (verified,_) | fst (verifiedExactInterface verified) == artifact -> Right (verifiedExactInterface verified)
      _ -> Left "requested exact interface differs from its verified closure"

-- The admitted TYPE product keeps authenticated bytes and decoded Names with
-- their actual issuer. Its logical input alias is never a filesystem grant.
data VerifiedExactInterface = VerifiedExactInterface !ExactIfaceArtifact ModIface !ArtifactBytes
  !(Char,MVar OrigNameCache)

verifiedExactInterface :: VerifiedExactInterface -> (ExactIfaceArtifact,ModIface)
verifiedExactInterface (VerifiedExactInterface artifact iface _ _) = (artifact,iface)

verifiedExactInterfaceBody :: VerifiedExactInterface -> ArtifactBytes
verifiedExactInterfaceBody (VerifiedExactInterface _ _ body _) = body

selectVerifiedExactInterfaceProducts :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact]
  -> Either String [VerifiedExactInterface]
selectVerifiedExactInterfaceProducts (VerifiedExactIfaceClosure captured) = mapM select
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (verified,_) | fst (verifiedExactInterface verified) == artifact -> Right verified
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
      Just (verified,aliases)
        | let (original,iface) = verifiedExactInterface verified
        , checkedValueOwner artifact
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
parseCapturedGeneratedImports flags captured@(CapturedGeneratedScaffoldSource path bytes _) =
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
      (stringToStringBuffer (Text.unpack (TextEncoding.decodeUtf8 bytes)))
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
      && sameImportOccurrenceSpan (getLoc imported) span') scaffold

sameImportOccurrenceSpan :: SrcSpan -> SrcSpan -> Bool
sameImportOccurrenceSpan left right = left == right && case (getBufSpan left,getBufSpan right) of
  (Strict.Just before,Strict.Just after) -> before == after
  _ -> False

readGeneratedScaffoldImportAuthority :: VerifiedExactIfaceClosure -> [ExecutionSourceIdentity]
  -> Maybe ((String,String),String) -> GeneratedScaffoldRecipe
  -> CapturedGeneratedImports -> DriverImportSyntax -> ModuleGraph -> HscEnv
  -> IO (Either String GeneratedScaffoldImportAuthority)
readGeneratedScaffoldImportAuthority (VerifiedExactIfaceClosure captured) nativeOwners planned
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
    originals <- case planned of
      Nothing -> Right noGeneratedScaffoldImports
      Just (owner,expectedFingerprint) -> do
        let original = mkModule (stringToUnit (fst owner)) (mkModuleName (snd owner))
        unless (moduleUnit original == homeUnitAsUnit (hsc_home_unit env))
          (Left "checked recipe original belongs to another home unit")
        unless (not (any (\case ModuleNode _ loaded -> ms_mod loaded == original; _ -> False)
          (mgModSummaries' sourceGraph)))
          (Left "checked recipe original collides with fresh source")
        (verified,_) <- maybe (Left "checked recipe lacks its verified original interface") Right
          (Map.lookup owner captured)
        let (artifact,iface) = verifiedExactInterface verified
        unless (mi_module iface == original
          && show (mi_iface_hash (mi_final_exts iface)) == expectedFingerprint)
          (Left "checked recipe original fingerprint differs")
        native <- case [candidate | candidate <- nativeOwners
            , (executionUnit candidate,executionModule candidate) == owner
            && executionIfaceSha256 candidate == exactSha256 artifact] of
          [candidate] -> Right candidate
          _ -> Left "checked recipe lacks its paired original native owner"
        declaration <- case [declaration | (_,declaration) <- protected
            , unLoc (ideclName declaration) == moduleName original
            , ideclQualified declaration == NotQualified && isNothing (ideclAs declaration)
            , ideclSource declaration == NotBoot && ideclImportList declaration == Nothing
            , case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False] of
          [declaration] -> Right declaration
          _ -> Left "checked recipe original lacks its protected import occurrence"
        imported <- consumed summary (== NoPkgQual) declaration
        -- Original instances and families belong to this sealed declaration;
        -- only the protected recipe edge receives visibility.
        pure (GeneratedScaffoldImportAuthority [(ms_mod summary,fingerprint,NativeScaffoldOwner native,getLoc imported,artifact)] [(artifact,[])] [] [])
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
      pure ((ms_mod summary,fingerprint,TemplateInterfaceOwner,getLoc imported,artifact):edges,unclaimed))
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
    let GeneratedScaffoldImportAuthority originalEdges originalNodes _ _ = originals
        instanceEdges = case instanceScope of
          ImportedTemplateInstances -> []
          OriginalPreviewInstances _ -> [(ms_mod summary,fingerprint,
            [(exactUnit artifact,exactModule artifact) | (artifact,_) <- templateNodes])]
    pure (GeneratedScaffoldImportAuthority (originalEdges ++ templateEdges)
      (originalNodes ++ templateNodes) instanceEdges orphanScopes)

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
  -> HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact] -> IO (Either String VerifiedExactIfaceClosure)
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
            | otherwise -> Right (VerifiedExactInterface artifact iface body
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

-- The interface reader and this hydration operation own the association.
-- This is a compiler TYPE product, never native execution authority.
newtype HydratedExactInterfaces = HydratedExactInterfaces [(ExactIfaceArtifact,HomeModInfo)]

hydratedExactInterfaces :: HydratedExactInterfaces -> [(ExactIfaceArtifact,HomeModInfo)]
hydratedExactInterfaces (HydratedExactInterfaces homes) = homes

hydrateExactScopeWithProducts :: HscEnv -> [VerifiedExactInterface]
  -> [(ExactIfaceArtifact,ModIface)] -> IO (HscEnv,HydratedExactInterfaces)
hydrateExactScopeWithProducts env verified additional = do
  forM_ verified $ \(VerifiedExactInterface _ _ _ issuer) ->
    unless (issuer == (nsUniqChar (hsc_NC env),nsNames (hsc_NC env)))
      (fail "verified interface belongs to another NameCache")
  let inputs = map verifiedExactInterface verified
      keys = Set.fromList [(exactUnit artifact,exactModule artifact) | (artifact,_) <- inputs]
  unless (all (\(artifact,_) -> (exactUnit artifact,exactModule artifact) `Set.notMember` keys) additional)
    (fail "unverified hydration rows overlap the exact selected product")
  completed <- hydrateExactScope env (inputs ++ additional)
  homes <- forM inputs $ \(artifact,iface) -> do
    home <- maybe (fail "hydrated interface lost its actual home") pure
      (lookupHpt (hsc_HPT completed) (moduleName (mi_module iface)))
    unless (mi_module (hm_iface home) == mi_module iface
        && mi_hsc_src (hm_iface home) == mi_hsc_src iface
        && mi_iface_hash (mi_final_exts (hm_iface home)) == mi_iface_hash (mi_final_exts iface))
      (fail "hydrated interface differs from its admitted input")
    pure (artifact,home)
  pure (completed,HydratedExactInterfaces homes)

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
