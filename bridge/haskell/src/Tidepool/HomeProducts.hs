-- | Revalidate immutable home products in one fresh compiler transaction.
-- Boot declarations are source inputs, not executable products. A boot SCC
-- receives fresh GHC load validation before its original prepared bodies reuse.
module Tidepool.HomeProducts
  ( hydrateCandidateHomeProducts, hydrateCandidateHomeProductsWithOriginals, hydrateCandidateHomeProductsWithOriginalsUsing
  , CandidateCoreFailure(..), validateCandidateInterfaceRequirements
  , materializeCandidateCompilerView, materializeAdmittedCompilerView
  , admittedCompilerInterface, validateAdmittedInterfaceRequirements
  , AdmittedFinalizedOriginal, recoverAdmittedFinalizedOriginal
  , admittedOriginalModule, admittedOriginalProof, admittedOriginalInterface
  , admittedOriginalLocation, OriginalVersion, originalVersionOwner, originalVersionInScope, originalVersionLookup, originalVersionSeal
  , OriginalRecoveryScope, admitOriginalRecoveryScope, originalVersionInRecoveryScope, recoverAdmittedFinalizedOriginalWithPrevious
  , revalidateAdmittedCore ) where

import Control.Exception
  ( Exception, SomeException, SomeAsyncException, bracket, displayException, fromException, throwIO, try )
import Control.Monad (forM, forM_, unless, when)
import Control.Monad.IO.Class (liftIO)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import Data.Text.Encoding qualified as TextEncoding
import Tidepool.ExtractUtil (shaHex)
import Data.IORef (newIORef, readIORef, atomicModifyIORef')
import Data.ByteString qualified as BS
import Crypto.Hash.SHA256 qualified as SHA256
import Numeric (showHex)
import Tidepool.BoundedRead (readFileAtMost)
import GHC
  ( Ghc, ModSummary(..), getSession, parseModule, setSession, typecheckModule
  , tm_internals_, ms_mod_name, LoadHowMuch(LoadAllTargets), SuccessFlag(..)
  , topSortModuleGraph )
import GHC.Driver.Env
  ( HscEnv(..), hsc_HPT, hscUpdateHPT )
import GHC.Driver.Backend (backendGeneratesCode)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.InstEnv (instEnvElts)
import GHC.Driver.Config.Diagnostic (initDiagOpts, initPrintConfig)
import GHC.Driver.Errors (printOrThrowDiagnostics)
import GHC.Driver.Errors.Types (GhcMessage(GhcTcRnMessage))
import GHC.Driver.Monad (reflectGhc, reifyGhc)
import GHC.Iface.Recomp (MaybeValidated(..), checkOldIface)
import GHC.Iface.Tidy (mkBootModDetailsTc)
import GHC.Driver.Session (GeneralFlag(Opt_Pp, Opt_BuildDynamicToo), backend, gopt, xopt, dynamicNow, targetProfile)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Rename.Names (gresFromAvails)
import GHC.Tc.Module (checkHiBootIface')
import GHC.Tc.Types (TcGblEnv, tcg_dependent_files, tcg_mod, tcg_rdr_env, tcg_top_loc)
import GHC.Tc.Utils.Monad (initTcWithGbl)
import GHC.Types.Name.Reader (mkGlobalRdrEnv)
import GHC.Types.Id (idName, idType)
import GHC.Types.TyThing (TyThing(..))
import GHC.Types.TypeEnv (lookupTypeEnv)
import GHC.Types.SourceFile (HscSource(..))
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), addToHpt, eltsHpt, lookupHpt )
import GHC.Unit.Module.Graph
  ( ModuleGraph, ModuleGraphNode(..), NodeKey(..), mgModSummaries', mkModuleGraph
  , mgTransDeps, mkNodeKey, nodeDependencies )
import GHC.Driver.Make (load')
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Unit.Module.ModDetails (md_exports, md_insts, md_types)
import GHC.Unit.Module.ModIface
  ( ModIface, mi_module, mi_usages, mi_iface_hash, mi_final_exts, set_mi_extra_decls )
import GHC.Unit.Module.Location (ModLocation(..))
import GHC.Iface.Binary (writeBinIface, CompressionIFace(..), TraceBinIFace(..))
import GHC.Unit.Module.Deps (Usage(..))
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString, unitIdString)
import GHC.Utils.Outputable (ppr, renderWithContext, defaultSDocContext)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), forkExactContext, hydrateExactScope, withExactHomeInstances
  , exactInterfaceSummary )
import Tidepool.CompileInputPolicy (pluginInputIssues)
import Tidepool.FamilyConsistency (validateEnvironmentFamilies)
import Tidepool.RetainedUnfoldings (scopeRetainedSummaryHscEnv, scopeRetainedModuleGraph)
import Tidepool.Timing (emitCount, readTimingEnabled, timeDetailPhase)
import Tidepool.ExactScope
  ( ExactScope , scopeProducerSha256, scopeInterfaces, CanonicalInterfaceProof, CanonicalInterfaceAdmission(..)
  , admittedInterfaceCore, admittedInterfaceHomeUnits, admittedInterfaceRequirements
  , scopeModuleInterfaceProofs, canonicalOrigin, isSourceOriginal, canonicalCoreArtifact
  , canonicalCoreSha256, canonicalCertificateSha256, canonicalRequirements, validateExactScopeEnvironment, admittedInterfaceCoreBytes )
import Tidepool.FinalizedCore (FinalizedCoreFailure, attachFinalizedCore, decodeFinalizedCore)
import Tidepool.FinalizedModule (FinalizedModule)
import System.Directory (getModificationTime, getTemporaryDirectory, removeDirectoryRecursive)
import System.FilePath ((</>))
import System.Posix.Temp (mkdtemp)

data CandidateCoreFailure
  = CandidateInterfaceRequirementsMismatch
  | CandidateCoreMissing
  | CandidateCoreTooLarge
  | CandidateCoreBytesMismatch
  | CandidateCoreDecodeFailure FinalizedCoreFailure
  | CandidateCoreHomeMissing
  | CandidateCompilerViewUnsupportedProfile
  deriving (Eq, Show)
instance Exception CandidateCoreFailure

-- Issued only after the exact original interface/Core pair has been checked.
-- This authorizes native preparation, not fresh source selection or a product.
data AdmittedFinalizedOriginal = AdmittedFinalizedOriginal
  FinalizedModule CanonicalInterfaceProof (ExactIfaceArtifact,FilePath,String) ModLocation

admittedOriginalModule :: AdmittedFinalizedOriginal -> FinalizedModule
admittedOriginalModule (AdmittedFinalizedOriginal original _ _ _) = original

admittedOriginalProof :: AdmittedFinalizedOriginal -> CanonicalInterfaceProof
admittedOriginalProof (AdmittedFinalizedOriginal _ proof _ _) = proof

admittedOriginalInterface :: AdmittedFinalizedOriginal -> (ExactIfaceArtifact,FilePath,String)
admittedOriginalInterface (AdmittedFinalizedOriginal _ _ row _) = row

admittedOriginalLocation :: AdmittedFinalizedOriginal -> ModLocation
admittedOriginalLocation (AdmittedFinalizedOriginal _ _ _ location) = location

-- Content identity includes the compiler producer, complete original Core,
-- interface and package witness. Paths and request purposes do not identify a
-- body. The certificate seals its source census and imported requirements.
data OriginalVersion = OriginalVersion Module String String String String String
  deriving (Eq, Ord)

originalVersionOwner :: OriginalVersion -> Module
originalVersionOwner (OriginalVersion owner _ _ _ _ _) = owner

-- Diagnostic rendering of the same complete canonical cache identity. The
-- length-delimited representation excludes paths and request-local identity.
originalVersionSeal :: OriginalVersion -> String
originalVersionSeal (OriginalVersion owner producer certificate core interface packages) =
  shaHex (TextEncoding.encodeUtf8 (Text.pack (show
    [unitString (moduleUnit owner),moduleNameString (moduleName owner)
    ,producer,certificate,core,interface,packages])))

originalVersionInScope :: ExactScope -> Module -> Maybe OriginalVersion
originalVersionInScope scope owner = originalVersionFromRows scope owner
  [row | row@(artifact,_,_) <- scopeInterfaces scope
    , (exactUnit artifact,exactModule artifact) == originalOwnerKey owner]

-- A descriptor-only view for callers comparing several owners. Acquiring
-- this function once shares the scope index; it does not admit any body.
originalVersionLookup :: ExactScope -> (Module -> Maybe OriginalVersion)
originalVersionLookup scope =
  let interfaces = originalInterfaceRows scope
  in \owner -> originalVersionFromRows scope owner
       (Map.findWithDefault [] (originalOwnerKey owner) interfaces)

originalInterfaceRows :: ExactScope -> Map.Map (String,String) [(ExactIfaceArtifact,FilePath,String)]
originalInterfaceRows scope = Map.fromListWith (++)
  [((exactUnit artifact,exactModule artifact),[row])
    | row@(artifact,_,_) <- scopeInterfaces scope]

originalOwnerKey :: Module -> (String,String)
originalOwnerKey owner = (unitString (moduleUnit owner),moduleNameString (moduleName owner))

originalVersionFromRows :: ExactScope -> Module -> [(ExactIfaceArtifact,FilePath,String)]
  -> Maybe OriginalVersion
originalVersionFromRows scope owner rows = do
  proof <- Map.lookup (originalOwnerKey owner) (scopeModuleInterfaceProofs scope)
  unless (isSourceOriginal (canonicalOrigin proof)) Nothing
  core <- canonicalCoreArtifact proof
  case rows of
    [(artifact,_,packages)] -> Just (OriginalVersion owner (scopeProducerSha256 scope)
      (canonicalCertificateSha256 proof) (canonicalCoreSha256 core)
      (exactSha256 artifact) packages)
    _ -> Nothing

-- Absence of a defining capability remains unavailable. Advertised invalid
-- artifacts are refusals; source lookup and frontend compilation never occur.
recoverAdmittedFinalizedOriginal
  :: HscEnv -> ExactScope -> Module -> IO (Maybe AdmittedFinalizedOriginal)
recoverAdmittedFinalizedOriginal env scope owner =
  admitOriginalRecoveryScope env scope >>= \admitted ->
    fmap (fmap snd) (recoverAdmittedFinalizedOriginalWithPrevious admitted owner Nothing)

-- Whole-scope certificate/interface validation is shared by one immutable
-- acquisition stage. Defining Core bytes are still checked on every lookup,
-- including hits; only their decoding and native lowering can be retained.
data OriginalRecoveryScope = OriginalRecoveryScope HscEnv ExactScope
  (Map.Map (String,String) [(ExactIfaceArtifact,FilePath,String)])
  (Map.Map (String,String) String)

admitOriginalRecoveryScope :: HscEnv -> ExactScope -> IO OriginalRecoveryScope
admitOriginalRecoveryScope env scope = do
  either (ioError . userError) pure =<< validateExactScopeEnvironment env scope
  let interfaces = originalInterfaceRows scope
      seals = Map.fromList [((exactUnit artifact,exactModule artifact),exactSha256 artifact)
        | (artifact,_,_) <- scopeInterfaces scope]
  pure (OriginalRecoveryScope env scope interfaces seals)

originalVersionInRecoveryScope :: OriginalRecoveryScope -> Module -> Maybe OriginalVersion
originalVersionInRecoveryScope (OriginalRecoveryScope _ scope interfaces _) owner =
  originalVersionFromRows scope owner (Map.findWithDefault [] (originalOwnerKey owner) interfaces)

-- A retained body never suppresses current artifact validation. On a hit only
-- immutable decoded Core is reused; proof, interface row and location belong
-- to the current consuming scope, including relocated identical artifacts.
recoverAdmittedFinalizedOriginalWithPrevious
  :: OriginalRecoveryScope -> Module -> Maybe (OriginalVersion,AdmittedFinalizedOriginal)
  -> IO (Maybe (OriginalVersion,AdmittedFinalizedOriginal))
recoverAdmittedFinalizedOriginalWithPrevious admittedScope@(OriginalRecoveryScope env scope interfaces seals) owner previous = case Map.lookup key (scopeModuleInterfaceProofs scope) of
  Just proof | isSourceOriginal (canonicalOrigin proof), Just _ <- canonicalCoreArtifact proof -> do
    row@(artifact,_,_) <- case Map.findWithDefault [] key interfaces of
      [row] -> pure row
      _ -> throwIO CandidateCoreHomeMissing
    unless (all (\(required,seal) -> Map.lookup required seals == Just seal)
        (Map.toAscList (canonicalRequirements proof)))
      (throwIO CandidateInterfaceRequirementsMismatch)
    let admission = ModuleInterfaceAdmission proof
        location = ms_location (exactInterfaceSummary env artifact)
    home <- admittedHomeInterface env admission owner
    version <- maybe (throwIO CandidateCoreHomeMissing) pure (originalVersionInRecoveryScope admittedScope owner)
    bytes <- readAdmittedCore admission
    original <- case previous of
      Just (oldVersion,old) | oldVersion == version -> pure (admittedOriginalModule old)
      _ -> decodeFinalizedCore env home location bytes >>= either (throwIO . CandidateCoreDecodeFailure) pure
    pure (Just (version,AdmittedFinalizedOriginal original proof row location))
  _ -> pure Nothing
  where
    key = (unitString (moduleUnit owner),moduleNameString (moduleName owner))

-- The producer's complete home census classifies package-form usages too.
-- Authored source import adjacency cannot substitute for this native census.
validateCandidateInterfaceRequirements
  :: CanonicalInterfaceProof -> ModIface -> Either CandidateCoreFailure ()
validateCandidateInterfaceRequirements proof =
  validateAdmittedInterfaceRequirements (ModuleInterfaceAdmission proof)

validateAdmittedInterfaceRequirements
  :: CanonicalInterfaceAdmission -> ModIface -> Either CandidateCoreFailure ()
validateAdmittedInterfaceRequirements proof iface =
  unless (all (`Set.member` homes) (map fst actual)
      && Set.fromList actual == Map.keysSet (admittedInterfaceRequirements proof))
    (Left CandidateInterfaceRequirementsMismatch)
  where
    homes = admittedInterfaceHomeUnits proof
    self = (unitString (moduleUnit (mi_module iface)), moduleNameString (moduleName (mi_module iface)))
    actual = Set.toAscList (Set.delete self (Set.fromList (concatMap owner (mi_usages iface))))
    owner UsageHomeModule{usg_mod_name = name, usg_unit_id = unit} =
      [(unitIdString unit, moduleNameString name)]
    owner UsageHomeModuleInterface{usg_mod_name = name, usg_unit_id = unit} =
      [(unitIdString unit, moduleNameString name)]
    owner UsagePackageModule{usg_mod = required}
      | unitString (moduleUnit required) `Set.member` homes =
          [(unitString (moduleUnit required), moduleNameString (moduleName required))]
    owner _ = []

-- A request-local make view preserves source selection while giving GHC its
-- own supported interface-to-bytecode handoff. Durable artifact paths are
-- never handed to make, which may delete its input interface during cleanup.
materializeCandidateCompilerView
  :: FilePath -> Int -> HscEnv -> CanonicalInterfaceProof -> ModSummary
  -> IO ModSummary
materializeCandidateCompilerView directory index env proof =
  materializeAdmittedCompilerView directory index env (ModuleInterfaceAdmission proof)

materializeAdmittedCompilerView
  :: FilePath -> Int -> HscEnv -> CanonicalInterfaceAdmission -> ModSummary
  -> IO ModSummary
materializeAdmittedCompilerView directory index env proof summary = do
  let flags = ms_hspp_opts summary
  unless (not (gopt Opt_BuildDynamicToo flags) && not (dynamicNow flags))
    (throwIO CandidateCompilerViewUnsupportedProfile)
  interface <- if backendGeneratesCode (backend flags)
    then admittedCompilerInterface env proof (ms_mod summary)
    else set_mi_extra_decls Nothing . hm_iface <$> admittedHomeInterface env proof (ms_mod summary)
  materializeInterfaceView directory index interface summary

-- Exact executable demand can consume this interface through GHC's native
-- interface-to-bytecode API without reconstructing an authored source summary.
admittedCompilerInterface
  :: HscEnv -> CanonicalInterfaceAdmission -> Module -> IO ModIface
admittedCompilerInterface env proof owner = do
  home <- admittedHomeInterface env proof owner
  bytes <- readAdmittedCore proof
  attachFinalizedCore env home bytes >>= either (throwIO . CandidateCoreDecodeFailure) pure

admittedHomeInterface
  :: HscEnv -> CanonicalInterfaceAdmission -> Module -> IO HomeModInfo
admittedHomeInterface env proof owner = do
  home <- maybe (throwIO CandidateCoreHomeMissing) pure
    (lookupHpt (hsc_HPT env) (moduleName owner))
  unless (mi_module (hm_iface home) == owner) (throwIO CandidateCoreHomeMissing)
  either throwIO pure (validateAdmittedInterfaceRequirements proof (hm_iface home))
  pure home

-- Both validation and executable make consume request-owned copies. Source
-- paths and usage fingerprints stay on the original finalized interface.
materializeInterfaceView :: FilePath -> Int -> ModIface -> ModSummary -> IO ModSummary
materializeInterfaceView directory index interface summary = do
  unless (mi_module interface == ms_mod summary) (throwIO CandidateCoreHomeMissing)
  let flags = ms_hspp_opts summary
      path = directory </> "candidate-" ++ show index ++ ".hi"
  writeBinIface (targetProfile flags) QuietBinIFace NormalCompression path interface
  modified <- getModificationTime path
  pure summary
    { ms_location = (ms_location summary)
        { ml_hi_file = path, ml_dyn_hi_file = path ++ ".dyn_hi" }
    , ms_iface_date = Just modified
    }

revalidateAdmittedCore :: CanonicalInterfaceAdmission -> IO ()
revalidateAdmittedCore = fmap (const ()) . readCurrentAdmittedCore

readAdmittedCore :: CanonicalInterfaceAdmission -> IO BS.ByteString
readAdmittedCore proof = case admittedInterfaceCore proof of
  Nothing -> throwIO CandidateCoreMissing
  Just _ -> admittedInterfaceCoreBytes proof >>= maybe (readCurrentAdmittedCore proof) pure

readCurrentAdmittedCore :: CanonicalInterfaceAdmission -> IO BS.ByteString
readCurrentAdmittedCore proof = do
  (path, sha) <- maybe (throwIO CandidateCoreMissing) pure (admittedInterfaceCore proof)
  bytes <- readFileAtMost path (32 * 1024 * 1024 + 1)
  unless (BS.length bytes <= 32 * 1024 * 1024) (throwIO CandidateCoreTooLarge)
  unless (digest bytes == sha) (throwIO CandidateCoreBytesMismatch)
  pure bytes
  where
    digest = concatMap (\byte -> let rendered = showHex byte ""
      in replicate (2 - length rendered) '0' ++ rendered) . BS.unpack . SHA256.hash

-- Every ordinary summary and every boot summary is from the current
-- downsweep. The caller has already checked source/package witnesses and
-- closed the candidate set over all selected home imports, including SOURCE.
-- No target summary belongs to this set. The load graph uses the same
-- representation flags as the producer load pass; validation order uses the
-- original downsweep graph retained in the environment.
hydrateCandidateHomeProducts
  :: HscEnv -> ModuleGraph -> [(ExactIfaceArtifact, ModIface)]
  -> [ModSummary] -> [ModSummary]
  -> Ghc (Either String HscEnv)
hydrateCandidateHomeProducts initial loadGraph interfaces =
  hydrateCandidateHomeProductsWithOriginals initial loadGraph interfaces [] (\_ -> pure . Right)

-- Original HMIs contribute implementation Names; only the selected lexical
-- graph contributes instances and families during current interface checks.
hydrateCandidateHomeProductsWithOriginals
  :: HscEnv -> ModuleGraph -> [(ExactIfaceArtifact, ModIface)]
  -> [(ExactIfaceArtifact, ModIface)]
  -> (ModuleGraph -> HscEnv -> IO (Either String HscEnv))
  -> [ModSummary] -> [ModSummary] -> Ghc (Either String HscEnv)
hydrateCandidateHomeProductsWithOriginals = hydrateCandidateHomeProductsWithOriginalsUsing forkExactContext

hydrateCandidateHomeProductsWithOriginalsUsing
  :: (HscEnv -> IO HscEnv) -> HscEnv -> ModuleGraph -> [(ExactIfaceArtifact, ModIface)]
  -> [(ExactIfaceArtifact, ModIface)]
  -> (ModuleGraph -> HscEnv -> IO (Either String HscEnv))
  -> [ModSummary] -> [ModSummary] -> Ghc (Either String HscEnv)
hydrateCandidateHomeProductsWithOriginalsUsing forkContext initial loadGraph interfaces originals installLexical
    summaries boots = reifyGhc $ \session -> do
  rollback <- forkContext initial
  result <- try (reflectGhc hydrate session)
  case result of
    Left failure | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
    Left (failure :: SomeException) -> do
      -- Typechecking may have mutated the attempt's EPS/finder cells. The
      -- private pre-attempt fork retains package facts and admitted homes.
      reflectGhc (setSession rollback) session
      pure (Left (displayException failure))
    Right environment -> pure (Right environment)
  where
    hydrate = do
      timing <- liftIO readTimingEnabled
      let selected = Map.fromList [(ms_mod_name summary, ()) | summary <- summaries]
          selectedGraph = mkModuleGraph
            [node | node@(ModuleNode _ summary) <- mgModSummaries' loadGraph
              , Map.member (ms_mod_name summary) selected]
          nativeSelectedGraph = mkModuleGraph
            [node | node@(ModuleNode _ summary) <- mgModSummaries' (hsc_mod_graph initial)
              , Map.member (ms_mod_name summary) selected]
          ordered = [summary | ModuleNode _ summary <- flattenSCCs
            (topSortModuleGraph True (hsc_mod_graph initial) Nothing)
            , ms_hsc_src summary == HsSrcFile
            , Map.member (ms_mod_name summary) selected]
          ordinaryByName = Map.fromList
            [(moduleName (mi_module iface), iface) | (_, iface) <- interfaces]
          ordinaryByModule = Map.fromList [(mi_module iface, iface) | (_, iface) <- interfaces]
          ordinaryNames = Map.fromList [(ms_mod_name summary, ()) | summary <- ordered]
          bootNames = Map.fromList [(ms_mod_name summary, ()) | summary <- boots]
          graphBootNames = Map.fromList [(ms_mod_name summary, ())
            | ModuleNode _ summary <- mgModSummaries' selectedGraph
            , ms_hsc_src summary == HsBootFile]
      unless (Map.keysSet selected == Map.keysSet ordinaryByName
          && (null boots || Set.fromList (map mkNodeKey (mgModSummaries' selectedGraph))
            == Set.fromList (map mkNodeKey (mgModSummaries' nativeSelectedGraph)))
          && Map.keysSet ordinaryByModule == Set.fromList (map ms_mod summaries)
          && Map.keysSet selected == Map.keysSet ordinaryNames
          && length summaries == Map.size selected
          && length interfaces == Map.size ordinaryByName
          && length boots == Map.size bootNames
          && Map.keysSet bootNames == Map.keysSet graphBootNames) $
        liftIO (ioError (userError "cached home owner/graph inventory differs"))
      forM_ (summaries ++ boots) $ \summary -> do
        let flags = ms_hspp_opts summary
        unless (not (gopt Opt_Pp flags) && not (any
            (`xopt` flags)
            [LangExt.Cpp, LangExt.TemplateHaskell, LangExt.QuasiQuotes])) $
          liftIO (ioError (userError "cached home input has untracked compile-time inputs"))
        case pluginInputIssues flags of
          [] -> pure ()
          issues -> liftIO (ioError (userError
            ("cached home input has untracked compiler plugin inputs: " ++ show issues)))
      (validationBase, sourceValidated) <- if null boots then pure (initial, Set.empty) else do
        -- GHC make supplies each module's actual dependency HPT, including
        -- boot interfaces inside SOURCE loops. Its completed HPT has ordinary
        -- interfaces instead, so it cannot recheck those original usage seals.
        sourceGraph <- either (liftIO . ioError . userError) pure
          (sourceValidationGraph nativeSelectedGraph boots)
        validatedRef <- liftIO (newIORef Set.empty)
        let expected = Set.fromList [ms_mod summary
              | ModuleNode _ summary <- mgModSummaries' sourceGraph
              , ms_hsc_src summary == HsSrcFile]
            validateLoaded env _ _ (ModuleNode _ summary)
              | ms_hsc_src summary == HsSrcFile = do
                  iface <- maybe (ioError (userError "cached SOURCE interface owner missing")) pure
                    (Map.lookup (ms_mod summary) ordinaryByModule)
                  validateHomeInterface env summary iface
                  duplicate <- atomicModifyIORef' validatedRef $ \validated ->
                    (Set.insert (ms_mod summary) validated, Set.member (ms_mod summary) validated)
                  when duplicate $
                    ioError (userError "cached SOURCE interface checked twice")
            validateLoaded _ _ _ _ = pure ()
        -- The native summaries use the producer's canonical checking profile.
        -- Offer original interfaces to make so its no-code path cannot replace
        -- a canonical dependency with a newly generated checking-only iface.
        flag <- withSourceValidationViews sourceGraph ordinaryByModule $ \viewGraph -> do
          setSession initial {hsc_mod_graph = viewGraph}
          timeDetailPhase timing "ghc_setup" "home_products_source_load" $
            load' Nothing LoadAllTargets mkUnknownDiagnostic (Just validateLoaded)
              (scopeRetainedModuleGraph viewGraph)
        unless (case flag of Succeeded -> True; Failed -> False) $
          liftIO (ioError (userError "cached home SOURCE graph failed fresh load"))
        validated <- liftIO (readIORef validatedRef)
        unless (validated == expected) $
          liftIO (ioError (userError "cached SOURCE interface validation inventory differs"))
        loaded <- getSession
        let current = loaded {hsc_mod_graph = selectedGraph}
        setSession current
        liftIO $ emitCount timing "home_products_source_load_owners"
          (toInteger (length (eltsHpt (hsc_HPT current))))
        liftIO $ emitCount timing "home_products_source_validation_owners"
          (toInteger (Set.size validated))
        forM_ boots $ \summary -> do
          unless (ms_hsc_src summary == HsBootFile) $
            liftIO (ioError (userError "cached home boot input is not a boot summary"))
          parsed <- parseModule summary
          typed <- withExactHomeInstances summary (typecheckModule parsed)
          let bootEnvironment = fst (tm_internals_ typed)
          dependentFiles <- liftIO (readIORef (tcg_dependent_files bootEnvironment))
          unless (null dependentFiles) $
            liftIO (ioError (userError "cached home boot input read untracked dependent files"))
          iface <- maybe
            (liftIO (ioError (userError "cached boot implementation interface missing"))) pure
            (Map.lookup (ms_mod summary) ordinaryByModule)
          liftIO (validateBootImplementation current summary bootEnvironment iface)
        liftIO $ emitCount timing "home_products_boot_compatibility_owners"
          (toInteger (length boots))
        setSession current
        pure (current, validated)
      combined <- liftIO (hydrateExactScope validationBase (originals ++ interfaces))
      hydrated <- liftIO (installLexical selectedGraph combined)
        >>= either (liftIO . ioError . userError) pure
      liftIO (validateEnvironmentFamilies hydrated)
      setSession (if null boots then hydrated else validationBase)
      forM_ ordered $ \summary -> do
        current <- getSession
        unless (ms_mod summary `Set.member` sourceValidated) $ do
          iface <- maybe
            (liftIO (ioError (userError "cached interface owner missing"))) pure
            (Map.lookup (ms_mod summary) ordinaryByModule)
          liftIO (validateHomeInterface current summary iface)
        hmi <- maybe
          (liftIO (ioError (userError "hydrated home product owner missing"))) pure
          (lookupHpt (hsc_HPT hydrated) (ms_mod_name summary))
        setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name summary) hmi) current)
      final <- getSession
      let restored = final {hsc_mod_graph = hsc_mod_graph initial}
      setSession restored
      pure restored

-- A cached ordinary interface skips GHC's ordinary frontend self-boot check.
-- Usage seals cover imported entities, while every current boot declaration
-- must match its original implementation, including unused families.
validateBootImplementation :: HscEnv -> ModSummary -> TcGblEnv -> ModIface -> IO ()
validateBootImplementation environment summary bootEnvironment iface = do
  home <- maybe (ioError (userError "cached boot implementation owner missing")) pure
    (lookupHpt (hsc_HPT environment) (ms_mod_name summary))
  unless (tcg_mod bootEnvironment == ms_mod summary
      && mi_module iface == ms_mod summary
      && mi_module (hm_iface home) == ms_mod summary
      && mi_iface_hash (mi_final_exts (hm_iface home)) == mi_iface_hash (mi_final_exts iface)) $
    ioError (userError "cached boot implementation differs from original interface")
  bootDetails <- mkBootModDetailsTc (hsc_logger environment) bootEnvironment
  let details = hm_details home
      flags = ms_hspp_opts summary
      checkingEnvironment = environment {hsc_dflags = flags}
      checkingBoot = bootEnvironment
        {tcg_rdr_env = mkGlobalRdrEnv (gresFromAvails checkingEnvironment Nothing (md_exports details))}
  (messages, checked) <- initTcWithGbl checkingEnvironment checkingBoot (tcg_top_loc bootEnvironment)
    (checkHiBootIface' (instEnvElts (md_insts details)) (md_types details) (md_exports details) bootDetails)
  printOrThrowDiagnostics (hsc_logger environment) (initPrintConfig flags) (initDiagOpts flags)
    (fmap GhcTcRnMessage messages)
  case checked of
    Just bridges -> forM_ bridges $ \(bootId, _) ->
      -- Reuse cannot synthesize the bindings that ordinary typechecking would
      -- add for boot DFuns or record selectors. They must already be native.
      case lookupTypeEnv (md_types details) (idName bootId) of
        Just (AnId originalId) | eqType (idType originalId) (idType bootId) -> pure ()
        _ -> ioError (userError "cached ordinary interface lacks current boot impedance binding")
    Nothing -> ioError (userError "cached ordinary interface does not implement current boot declarations")

validateHomeInterface :: HscEnv -> ModSummary -> ModIface -> IO ()
validateHomeInterface environment summary iface = do
  unless (mi_module iface == ms_mod summary) $
    ioError (userError "cached interface has another module owner")
  decision <- checkOldIface
    (scopeRetainedSummaryHscEnv summary environment) summary (Just iface)
  case decision of
    UpToDateItem _ -> pure ()
    OutOfDateItem reason _ -> ioError (userError
      ("cached home product failed fresh interface validation: "
        ++ moduleNameString (ms_mod_name summary) ++ ": "
        ++ renderWithContext defaultSDocContext (ppr reason)))

-- Make may delete its interface inputs. Only these request-owned copies enter
-- its cleanup set; boot summaries still consume the current authored source.
withSourceValidationViews
  :: ModuleGraph -> Map.Map Module ModIface -> (ModuleGraph -> Ghc a) -> Ghc a
withSourceValidationViews graph interfaces action = reifyGhc $ \session -> bracket
  (getTemporaryDirectory >>= \directory -> mkdtemp (directory </> "tidepool-source-view.XXXXXX"))
  removeDirectoryRecursive
  (\directory -> do
    nodes <- forM (zip [0..] (mgModSummaries' graph)) $ \(index,node) -> case node of
      ModuleNode dependencies summary | ms_hsc_src summary == HsSrcFile -> do
        interface <- maybe (ioError (userError "cached SOURCE interface view owner missing")) pure
          (Map.lookup (ms_mod summary) interfaces)
        view <- materializeInterfaceView directory index interface summary
        pure (ModuleNode dependencies view)
      _ -> pure node
    reflectGhc (action (mkModuleGraph nodes)) session)

-- GHC's cached reachability retains boot nodes and their real downsweep
-- edges. Root both forms of every boot owner, keep the original node order,
-- and refuse a dangling home edge before allowing any candidate to skip.
sourceValidationGraph :: ModuleGraph -> [ModSummary] -> Either String ModuleGraph
sourceValidationGraph graph boots = do
  let nodes = mgModSummaries' graph
      bootNames = Set.fromList (map ms_mod_name boots)
      roots = [mkNodeKey node | node@(ModuleNode _ summary) <- nodes
        , ms_mod_name summary `Set.member` bootNames]
      ordinaryRoots = Set.fromList [ms_mod_name summary
        | ModuleNode _ summary <- nodes, ms_hsc_src summary == HsSrcFile
        , ms_mod_name summary `Set.member` bootNames]
  unless (ordinaryRoots == bootNames) (Left "cached SOURCE ordinary owner is absent")
  closures <- mapM (\root -> maybe (Left "cached SOURCE reachability is absent")
    (Right . Set.insert root) (Map.lookup root (mgTransDeps graph))) roots
  let required = Set.unions closures
      selected = [node | node <- nodes, mkNodeKey node `Set.member` required]
      homeDependencies = [dependency | node <- selected, dependency <- nodeDependencies False node
        , NodeKey_Module _ <- [dependency]]
  unless (all (`Set.member` required) homeDependencies)
    (Left "cached SOURCE fresh-load closure is incomplete")
  pure (mkModuleGraph selected)
