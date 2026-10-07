{-# LANGUAGE GADTs #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}

module Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedModuleCompletion, PreparedModuleCompletionInputs(..), PreparedModuleObserver(..), PreparedPipelineResult(..), PreparedSegmentProductsResult(..), TypedSegmentPreparation, CheckedEnvironmentResult(..)
  , PreparedDependencies, preparedFreshDependencies, preparedExactCompilation, preparedHomeRequirements
  , ProgramSourceImports, retainProgramSourceImports, withProgramSourceImports
  , runPipelineSelected, runPipelineSessionSelected
  , runPipelineSelectedRetaining
  , CompilerProducerIdentity, captureCompilerProducerIdentity
  , runPipelineSessionSelectedWithProducer
  , CompilePurpose(..), withSourceImportIntents, PipelineResult(..)
  , generatedScaffoldRecipe
  , FinalizedModule, finalizedHomeModInfo, finalizedTidyGuts
  , FinalizedExecutionFailure(..)
  , RetainedCompilerArtifact, retainedCompilerInterface, retainedCompilerArtifactClosure
    -- * Bound-value type analysis
  , stripMonadHead, isClosureType, renderType
  , splitTupleType
  , cellExpressionPlans, cellExpressionEvidence, cellCheckedBinderSignatures, satisfiesCapturedConstraint
  , checkCellInstances, activationPreviewInputType
  , GeneratedInstanceRecipe, generatedInstanceRecipe, cellGeneratedInstanceRecipe
  , withGeneratedInstanceRecovery
    -- * Resident session
  , CompilerTransactionFailure(..)
  , withResidentPipelineSelected
  , withResidentPipelineSelectedRequests
  , CompilerScope(..), CompilerScopeRunner, withResidentCompilerScopes
  , CompilerRecoveryCaches(..)
  , withScopedExactInterfaceTransaction
  , withExactInterfaceTransaction
  ) where

import GHC hiding (typeKind)
import GHC.Driver.Main (hscDesugar, hscDesugar', hscSimplify, hscMaybeWriteIface, batchMsg, hscTidy, hscCompileCoreExpr', loadIfaceByteCode, generateFreshByteCode, mkCgInteractiveGuts)
import GHC.Driver.Pipeline (compileOne')
import GHC.Driver.Pipeline.Execute (runPhase)
import GHC.Driver.Pipeline.Phases (TPhase(..), PhaseHook(..))
import GHC.Driver.Hooks (hscCompileCoreExprHook, hscFrontendHook, runPhaseHook, runMetaHook)
import GHC.Data.StringBuffer (stringToStringBuffer)
import qualified GHC.Data.Maybe as MaybeErr
import GHC.Driver.Backend (backendGeneratesCode, backendWritesFiles, backendCanReuseLoadedCode)
import GHC.Driver.Env (hscUpdateFlags, hscUpdateHPT, hsc_HPT, hsc_home_unit, runHsc')
import GHC.Driver.Env.Types (HscEnv(hsc_mod_graph, hsc_unit_env, hsc_logger, hsc_dflags, hsc_FC, hsc_targets, hsc_hooks, hsc_interp, hsc_type_env_vars))
import GHC.Driver.Env.KnotVars (emptyKnotVars)
import GHC.Driver.Monad (reflectGhc, reifyGhc, Session(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), HomeModLinkable(..), emptyHomeModInfoLinkable, emptyHomePackageTable, justBytecode, addToHpt, lookupHpt, eltsHpt, filterHpt)
import GHC.Unit.Module.ModDetails (ModDetails, md_types)
import GHC.Unit.Module.Status (HscBackendAction(..))
import GHC.Types.ForeignStubs (ForeignStubs(NoStubs))
import GHC.Driver.Config.Diagnostic (initDiagOpts, initPrintConfig)
import GHC.Driver.Errors (printOrThrowDiagnostics)
import GHC.Iface.Load (loadInterface, WhereFrom(..))
import GHC.Rename.Names (renameRawPkgQual)
import GHC.Driver.Make (load', ModIfaceCache, newIfaceCache, filterModIfaceCache, addHmiToCache)
import qualified GHC.Linker.Loader as Linker
import GHC.Linker.Types (Linkable(..), linkableObjs, linkableLibs)
import GHC.Unit.Module.Env (moduleEnvElts)
import GHC.Runtime.Interpreter (stopInterp, purgeLookupSymbolCache)
import GHC.Runtime.Interpreter.Types
  ( Interp(..), InterpInstance(ExternalInterp), ExtInterp(ExtIServ), ExtInterpState(..)
  , InterpStatus(..), ExtInterpInstance(..), InterpProcess(..) )
import GHCi.Message (Pipe(..))
import Control.Concurrent.MVar (modifyMVar_, readMVar)
import System.Process (waitForProcess, terminateProcess)
import System.Timeout (timeout)
import GHC.Iface.Make (mkIfaceTc, mkPartialIface, mkFullIface)
import GHC.Iface.Recomp (MaybeValidated(..), checkOldIface)
import GHC.Unit.Finder (initFinderCache)
import GHC.Unit.Module.ModIface (set_mi_extra_decls, mi_iface_hash)
import GHC.Unit.Module.Deps (imp_mods)
import GHC.Unit.Finder (FindResult(Found), findImportedModule)
import GHC.Iface.Tidy (mkBootModDetailsTc)
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.Error (MessageClass(..), mkLocMessage, getMessages, errMsgDiagnostic, unionMessages)
import GHC.Types.SourceError (SourceError, srcErrorMessages)
import GHC.Driver.Errors.Types (GhcMessage(..))
import GHC.Tc.Errors.Types
  ( TcRnMessage(..), TcRnMessageDetailed(..), DeriveInstanceErrReason(..)
  , SolverReportWithCtxt(..) )
import GHC.Tc.Errors.Types qualified as TcError
import GHC.Utils.Logger (LogAction, makeThreadSafe)
import Tidepool.TypedSegment (TypedSegmentPlan, GeneratedSegmentOperations, TypedSegment, PendingTypedSegment, TypedSegmentFailure(..),
  pendingSegmentItems, pendingSegmentSupportRoots, typedItemRoot, captureTypedSegment, closeTypedSegment, installTypedSegmentRoots)
import Tidepool.TypedSegment.Source (rewriteParsedSegmentRoot)
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), InputRejection(..), DependencyLoadFailure(..), dependencyDiagnostic, spanOf)
import GHC.Data.FastString (unpackFS, mkFastString)
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Unit.Module.Graph (mgModSummaries', ModuleGraphNode(..), NodeKey, mkNodeKey, nodeDependencies)
import GHC.Unit.Home (homeUnitAsUnit, homeUnitId, isHomeUnit)
import GHC.Unit.Env (UnitEnv(..))
import GHC.Unit.Types (unitString, unitIdString, stringToUnit)
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Driver.Session
  ( updOptLevel, gopt_set, gopt_unset, xopt
  , WarningFlag
      ( Opt_WarnMissingFields
      , Opt_WarnIncompletePatterns
      , Opt_WarnIncompleteUniPatterns
      )
  , wopt_set
  , PackageFlag(..), PackageArg(..), ModRenaming(..)
  , PackageDBFlag(..), PkgDbRef(..), ParMakeCount(..) )
import GHC.Unit.Module.ModGuts (ModGuts(..), CgGuts(..))
import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), Alt(..))
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BS8
import qualified Crypto.Hash.SHA256 as SHA256
import Data.Char (digitToInt)
import qualified Data.Graph as Graph
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable
  ( renderWithContext, defaultSDocContext, ppr, SDocContext(..), Outputable, showSDocUnsafe
  , mkUserStyle, NamePprCtx(..), QualifyName(..), Depth(..), PromotionTickContext(..) )
import GHC.Types.Id (idName, setIdExported)
import GHC.Core.Type
  ( mkInvisForAllTys
  , mkInvisFunTys
  , splitAppTy_maybe
  , splitTyConApp_maybe
  , splitFunTy_maybe, splitFunTys
  , mkTyVarTy, typeKind, tyCoVarsOfType
  )
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.FVs (scopedSort, tyConsOfType)
import GHC.Core.TyCon (isTupleTyCon, tyConDataCons_maybe, unwrapNewTyCon_maybe, tyConUnique, tyConName)
import GHC.Types.SrcLoc (mkRealSrcSpan, mkRealSrcLoc)
import GHC.Core.Class (className)
import GHC.Core.InstEnv (is_cls, is_tys)
import GHC.Core.FamInstEnv (fi_fam, fi_tys)
import GHC.Core.Predicate (mkClassPred, getClassPredTys_maybe, isCoVarType)
import GHC.Builtin.Names (gHC_PRIM, fUNTyConKey, unrestrictedFunTyConKey, genClassKey, repTyConKey)
import GHC.Builtin.Types (zonkAnyTyCon)
import GHC.Core.DataCon (dataConRepArgTys)
import GHC.Data.Bag (listToBag)
import GHC.Tc.Solver (tcCheckGivens, tcCheckWanteds)
import GHC.Tc.Solver.InertSet (emptyInert)
import GHC.Tc.Utils.Monad (initTcWithGbl, initIfaceCheck, getGblEnv)
import GHC.Tc.Gen.Splice (defaultRunMeta)
import GHC.Tc.Utils.TcMType (newEvVars)
import GHC.Core.TyCo.Rep (Scaled(..), Type(..))
import GHC.Types.Unique.Set (UniqSet, emptyUniqSet, addOneToUniqSet, elementOfUniqSet)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.TypeEnv (typeEnvIds, typeEnvTyCons)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Tc.Types (FrontendResult(..), TcGblEnv, tcg_mod, tcg_th_coreplugins, tcg_import_decls, tcg_dependent_files, tcg_binds, tcg_rdr_env, tcg_type_env, tcg_imports, tcg_keep)
import GHC.Types.Name.Set (extendNameSetList)
import GHC.Types.Name.Reader (GlobalRdrEnv, rdrNameOcc)
import GHC.Types.Name.Ppr (mkNamePprCtx)
import GHC.Types.Name (nameOccName, nameUnique, mkExternalName, mkInternalName, nameModule_maybe)
import GHC.Types.Name.Occurrence (mkOccName, mkTyVarOcc, occNameSpace, occNameString, isTcOcc)
import GHC.Types.Unique.Supply (UniqSupply, mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Types.Var (mkTyVar, mkTyVarBinder, setVarName)
import GHC.Types.Var.Set (isEmptyVarSet)
import Language.Haskell.Syntax.Specificity (Specificity (SpecifiedSpec))
import GHC.Types.Var.Env (mkVarEnv, lookupVarEnv)
import Control.Applicative ((<|>))
import Control.DeepSeq (force)
import Control.Concurrent (myThreadId)
import Control.Exception
  ( Exception, SomeException, SomeAsyncException, fromException, finally, bracket, mask, try, catch, throwIO, IOException )
import Data.Maybe (fromMaybe, isJust, isNothing, catMaybes, mapMaybe)
import Data.List (find, isPrefixOf, nub, nubBy, sort, sortOn, intercalate, foldl')
import Data.Containers.ListUtils (nubOrd)
import Data.IORef (IORef, atomicModifyIORef', newIORef, modifyIORef', readIORef, writeIORef)
import Numeric (showHex)
import System.Environment (lookupEnv)
import System.FilePath (takeBaseName, takeFileName, normalise, pathSeparator, (</>))
import System.Directory (canonicalizePath, makeAbsolute, doesFileExist, getCurrentDirectory, getModificationTime, getTemporaryDirectory, removeDirectoryRecursive)
import System.Posix.Temp (mkdtemp)
import System.IO (hPutStrLn, stderr, readFile', hClose)
import Tidepool.BoundedRead (readFileAtMost)
import Control.Monad.IO.Class (liftIO)
import Control.Monad (forM, forM_, when, unless, filterM, foldM, (>=>))
import Data.Data (Data, cast, gmapQ)
import Data.Foldable (toList)
import Data.Unique qualified as RequestUnique
import Data.Word (Word64)
import Tidepool.Binders (CheckedBinderPin(..), CellSourcePlan(..), SourcePrologue(..), LocatedImport(..), ImportIntent(..), CellGenericDeclaration(..), CellStructuralDisplayTarget(..), CellExpressionPlan(..), ExpressionLiftPlan(..), omitCellGenericDeclarations, omitCellStructuralDisplayDeclarations)
import Tidepool.CheckedCell (CheckedSignature, captureCheckedSignature, rewriteCheckedAnnotations, rewriteHostInputType, rewriteRequestTypes
  , NativeParsedModule, unannotatedModule, mapNativeModule, thenNativeModule, typecheckNativeModuleWithDiagnostics)
import Tidepool.FinalizedModule (FinalizedModule(..), homeInterfaceUsageOwners)
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, finalizedLocalAdmissions, localFinalizedInterface
  , localFinalizedSourceSha256, localFinalizedRequirements, localFinalizedHomeUnits, localFinalizedCore
  , matchesCapturedFinalization )
import Tidepool.HomeProducts
  ( hydrateCandidateHomeProductsWithOriginalsUsing, materializeCandidateCompilerView, admittedCompilerInterface
  , validateCandidateInterfaceRequirements, revalidateAdmittedCore )
import Tidepool.CompileInputPolicy (pluginInputIssues)
import Tidepool.PlannedDeclaration
  ( PlannedDeclarationInventory, transformPlannedDeclarationImports, transformPlannedDeclarationImportsWithCompleted, transformProgramDeclarationImports, hydratePlannedDeclarationInventory )
import Tidepool.CheckedPrefixImports
  ( CompletedValueImport(..), CompletedValueImports, hydrateCompletedValueImportsWithVerifiedDependencies
  , transformCompletedValueImports, refineParsedDeclarationImports, selectedImportNames )
import Tidepool.FamilyConsistency (validateCompilationFamilies, validateEnvironmentFamilies)
import Tidepool.TypePolicy (nominalHeadsOfType, stabilizeEffectRows)
import Tidepool.ExtractUtil (getLibdir, capitalize)
import Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences)
import Tidepool.Introspection (normalizeLookupWildcards)
import Tidepool.Session
  ( SessionModule(..), SessionModuleKind(..), SessionScope(..)
  , isSessionScopeActive, injectSessionScopeWithCaptures, CapturedSessionInterface, capturedSessionInterface, registerSessionInterfaceLocation, renderSessionModule
  , scaffoldTargetName, scaffoldOutputBase, evalUserBinder, parseSessionModule, isReservedSessionModuleName )
import Tidepool.Timing
  ( readTimingEnabled, timeSection, timePhase, emitPhase, emitCount
  , timeDetailPhase, timeModuleDetailPhase, ResourceTimingStart, beginResourceTiming, endResourceTiming
  , monotonicTime, elapsedMs
  , emitCompileSummary, emitModuleTiming, emitModuleInterfaceTiming
  , InterfaceStage(..), InterfaceReuse(..), measureModuleInterface
  , newTimingRequestIdentity
  , ReuseContext(..), ReuseModule(..), ReuseStage(..), ReuseDecision(..), ReuseReason(..)
  , ReuseVersionKind(..), emitReuse, emitReuseComplete, emitCheckOnlyReuseApplicability
  , readMemoTraceEnabled, MemoSelectionState(..), MemoSelectionTrace(..), MemoExecutableTrace(..)
  , emitMemoCycleGraph, emitMemoMissTrace )
import Tidepool.PreparedStg (PreparedModule, preparedUsesSiteAuthority, resolvePreparedSiteEnvironment, preparedSiteDependenciesMatch, preparedSiteDependenciesEquivalent, acquirePreparedModuleWithSiteEnvironment, runPreparedModuleTask
  , PreparedBodyCache, newPreparedBodyCache, mergePreparedBodyCaches, selectPreparedBodyCaches, evictPreparedBodyMatching)
import Tidepool.FatIface
  ( FatIfaceCache, newFatIfaceCache, mergeFatIfaceCaches, selectFatIfaceCaches, evictFatIfaceMatching
  , OwnerInterfaceCache, newOwnerInterfaceCache, mergeOwnerInterfaceCaches, selectOwnerInterfaceCaches, evictOwnerInterfaceMatching)
import Tidepool.ExecutionProjection
  ( OriginalProjectionCache, newOriginalProjectionCache, mergeOriginalProjectionCaches, selectOriginalProjectionCaches, evictOriginalProjectionMatching
  , preparedRootIdentity )
import Tidepool.CompilerExecution
  ( CompilerExecutionGrant, serialCompilerExecutionGrant, compilerModuleJobs
  , CompilerExecutor, withCompilerExecutor, runCompilerTasks, dependencyClosedReuse )
import Tidepool.PreparedSites
  ( resolvePreparedSiblings, resolvePreparedInterfaceSiblings )
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.RetainedUnfoldings
  ( RetainedContext, retainedContext
  , installRetainedUnfoldingsPlugin, retainedDefinedBy
  , scopeRetainedModuleGraph, scopeRetainedSummaryHscEnv )
import Tidepool.TurnSource (extractModuleName)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyResolution(..)
  , DependencyModule(..), DependencyImport(..), DependencyQualifier(..), ProductAvailability(..)
  , sourceEvidenceWithFingerprint, selectedFreshHomeRequirements )
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), freshExactState, PackageFinderFacts, newPackageFinderFacts, forkExactContextWithPackageFacts, hydrateExactScope, exactInterfaceSummary, serializeOriginalInterface, exactHomeInstancesFor
  , readVerifiedExactIfaceClosureWithCheckedValues, selectVerifiedExactInterfaces, selectVerifiedValueInterfaces
  , checkedValueImportAuthorityFromVerified
  , GeneratedScaffoldRecipe, generatedScaffoldRecipe, captureGeneratedScaffoldTarget
  , noGeneratedScaffoldImports, readGeneratedScaffoldImportAuthority, permitsGeneratedScaffoldImport, installExactLexicalGraphWithScaffold )
import Tidepool.ExactScope
  ( ExactScope(..), scopeInterfaces, scopeInterfaceEvidence, ExactScopePurpose(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..), SourceSelectedOriginals(..), extendSourceSelectedOriginals, CheckedCellAdmission(..), CheckedItemAdmission(..), readExactScope, revalidateExactScope, writeCheckedExactCompilation, scopeValueInterfaces
  , scopeAvailableOriginalProducts
  , ActivationPreviewAdmission(..), scopeActivationPreview
  , scopeCheckedCell, scopeCheckedItem, scopeIncludePaths
  , compilationOriginalSourceImports, scopeExecutionNativeOwners, ExactInterfaceEvidence(..), CanonicalOrigin(..), canonicalOrigin, normalizeInterfaceEvidence )
import Tidepool.ExactScope
  ( CanonicalInterfaceProof, CanonicalInterfaceAdmission, scopeCanonicalInterfaces
  , scopeSourceOriginalInterfaces, admittedInterfaceRequirements
  , validateCandidateCanonicalInterfaceProof
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalCoreArtifact
  , canonicalCorePath, canonicalCoreSha256, canonicalHomeUnits, canonicalRequirements
  , scopeModuleInterfaceProofs, canonicalSourceSha256, canonicalSourceImports, isSourceOriginal )
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceIdentity(..)
  , ExecutionSourceFailure(..), ExecutionSourceValidationStage(..), ExecutionSourceInterfaceReason(..)
  , ExecutionSourceRef(..), executionSourceClosure, executionIdentityKey
  , executionSourceGraphsFit
  , executionNodeIdentity, executionNodeModule, executionNodeSourceSha256, executionNodeRequirements )
import Tidepool.PackageWitness
  ( PackageImportEvidence(..), CompilerProvidedImport(..), emptyPackageImports, packageImportRoot, readPackageImports
  , validatePackageImportRoot, encodePackageImports )
import Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateImport(..), CandidateQualifier(..), CandidateGroup(..)
  , CapturedCandidateManifest, captureCandidateManifest, candidateManifestSha256
  , readCapturedModuleCandidatesWithGraphs, readModuleCandidatesWithGraphs
  , candidateExecutionSources, candidateOriginalIdentity )
import Tidepool.OriginalProductRoots (candidateOriginalGlobalDemand)

-- | Selects the compiler representation produced at the internal GHC API
-- boundary. Metadata consumers stop at the checked environment.
data PipelineSelection result where
  PreparedStg :: PipelineSelection PreparedPipelineResult
  PreparedProducts :: Maybe FilePath -> PipelineSelection PreparedPipelineResult
  PreparedSegmentProducts :: TypedSegmentPlan -> Maybe FilePath -> PipelineSelection PreparedSegmentProductsResult
  WithTypedSegmentPreparation :: TypedSegmentPreparation -> PipelineSelection result -> PipelineSelection result
  CheckedEnvironment :: PipelineSelection CheckedEnvironmentResult
  CheckedEnvironmentProducts :: FilePath -> PipelineSelection CheckedEnvironmentResult
  WithCompilerExecution :: CompilerExecutionGrant -> CompilerExecutor -> PipelineSelection result -> PipelineSelection result
  WithPreparedModuleCompletion :: PreparedModuleCompletion -> PipelineSelection result -> PipelineSelection result

-- The Session owner emits and hydrates all captures as one private batch. The
-- returned actual globals replace earlier capture parameters before simplify.
type TypedSegmentPreparation = HscEnv -> Map.Map (String,String) CanonicalInterfaceAdmission
  -> PendingTypedSegment -> IO (HscEnv, [(Id,Id)], [CapturedSessionInterface])

data PreparedSegmentProductsResult = PreparedSegmentProductsResult
  { preparedSegmentProducts :: PreparedPipelineResult
  , preparedSegmentCaptures :: TypedSegment
  }

-- | Acquired after finalization, before independent lowering starts. The
-- observer owns actual projection inputs captured by the request, including
-- retained generations. Projection consumes the lowering job's allowance;
-- demand incorporation runs serially on its coordinator.
type PreparedModuleCompletion = HscEnv -> Map.Map ModuleName ModIface -> Module
  -> Maybe ExactScope -> PreparedModuleCompletionInputs -> IO PreparedModuleObserver

-- | Immutable facts fixed before the native tasks start. A queued source
-- owner is distinct from a missing original; exact external executable groups
-- are already supplied. Siblings retain the full finalized typed Id census.
data PreparedModuleCompletionInputs = PreparedModuleCompletionInputs
  { completionSourceOwners :: Set.Set Module
  , completionSiblings :: Map.Map String Id
  , completionExternalOriginals :: Set.Set SymbolIdentity
  , completionExternalOriginalBindings :: Map.Map Name SymbolIdentity
  , completionReuseContext :: ReuseContext
  }

data PreparedModuleObserver = PreparedModuleObserver
  { observePreparedModule :: PreparedModule -> IO ()
    -- ^ Worker-safe immutable projection and short result publication only.
  , completedPreparedModule :: PreparedModule -> IO ()
    -- ^ Coordinator continuation; may schedule newly demanded work on the
    -- request's existing executor, without mutating the live Session.
  }

data PreparationKind = CheckOnly | PrepareStg

-- | Type-only compilation only needs a freshly built home interface while a
-- later source module in this same dependency-ordered pass imports it. The
-- checked target is returned directly to metadata consumers, so it is a leaf
-- unless another source module still has to typecheck against it.
data HomeInterfaceUse
  = HomeInterfaceLeaf
  | HomeInterfaceNeededBy ModuleName
  | HomeInterfaceNeededForSessionInjection

homeInterfaceUse :: ModSummary -> Map.Map ModuleName ModuleName -> HomeInterfaceUse
homeInterfaceUse summary laterConsumers
  | Just (SessionModule LibMod _) <- parseSessionModule
      (moduleNameString (ms_mod_name summary)) =
      HomeInterfaceNeededForSessionInjection
  | otherwise = maybe HomeInterfaceLeaf HomeInterfaceNeededBy
      (Map.lookup (ms_mod_name summary) laterConsumers)

-- | The consumer maps line up with summaries: each map records normal home
-- imports from only the modules after that position. Building them in one
-- reverse pass avoids rescanning every remaining suffix for every module.
homeInterfaceConsumers :: [ModSummary] -> [Map.Map ModuleName ModuleName]
homeInterfaceConsumers summaries = drop 1 (scanr addConsumer Map.empty summaries)
  where
    addConsumer summary laterConsumers
      | ms_hsc_src summary /= HsSrcFile = laterConsumers
      | otherwise = foldr addImport laterConsumers (ms_textual_imps summary)
      where
        addImport (_, imported) = Map.insertWith keepNearest (unLoc imported) (ms_mod_name summary)
        keepNearest new _ = new

-- | Prepared mode keeps the ordinary typed pipeline observations alongside
-- the unflattened per-module STG handoff.
data PreparedPipelineResult = PreparedPipelineResult
  { pprPipelineResult :: PipelineResult
  , pprModules :: [PreparedModule]
  , pprDependencies :: PreparedDependencies
  , pprProductInterfaces :: Map.Map ModuleName ModIface
  , pprFinalizedModules :: Map.Map ModuleName FinalizedModule
  , pprPackageImports :: Map.Map ModuleName PackageImportEvidence
  , pprAcceptedCandidates :: [ModuleCandidate]
  , pprOriginalBindings :: Map.Map Name SymbolIdentity
  }

-- | One publication owns both source lookup and retained exact imports.
-- Constructors stay private so consumers cannot pair unrelated compilations.
data PreparedDependencies
  = SourceOnly DependencyEvidence
  | ExactScoped DependencyEvidence ExactCompilation CompletedSourceImports

-- Only the completed compiler pass captures which original authored requests
-- actually resolved to fresh home roots. Generated imports cannot issue one.
newtype CompletedSourceImports = CompletedSourceImports [(ImportIntent,(String,String))]

preparedDependencies
  :: HscEnv -> [ImportIntent] -> DependencyEvidence -> Maybe ExactCompilation
  -> IO PreparedDependencies
preparedDependencies _ _ fresh Nothing = pure (SourceOnly fresh)
preparedDependencies env intents fresh (Just exact) = do
  roots <- fmap catMaybes $ forM intents $ \intent -> case intent of
    RetainedGeneratedImport -> pure Nothing
    AuthoredSourceImport owner rawQualifier -> do
      let qualifier = renameRawPkgQual (hsc_unit_env env) owner rawQualifier
      resolved <- findImportedModule env owner qualifier
      pure $ case resolved of
        Found _ original
          | let key = (unitString (moduleUnit original),moduleNameString (moduleName original))
          , [node] <- [node | node <- dependencyModules fresh
              , (dependencyModuleUnit node,dependencyModuleName node) == key
              , not (dependencyModuleBoot node)]
          , any (\selection -> dependencyResolutionQualifier selection == dependencyQualifier qualifier
              && dependencyResolutionModule selection == moduleNameString owner
              && not (dependencyResolutionBoot selection)
              && dependencyResolutionSelected selection == Just (dependencyModuleSource node))
              (dependencyResolutions fresh) -> Just (intent,key)
        _ -> Nothing
  pure (ExactScoped fresh exact (CompletedSourceImports (nub roots)))

-- | Fresh source lookup only; retained exact imports are deliberately absent.
preparedFreshDependencies :: PreparedPipelineResult -> DependencyEvidence
preparedFreshDependencies prepared = case pprDependencies prepared of
  SourceOnly fresh -> fresh
  ExactScoped fresh _ _ -> fresh

-- | Exact-scope evidence only; fresh source imports remain in their own view.
preparedExactCompilation :: PreparedPipelineResult -> Maybe ExactCompilation
preparedExactCompilation prepared = case pprDependencies prepared of
  SourceOnly _ -> Nothing
  ExactScoped _ exact _ -> Just exact

-- Private, request-local reuse of completed authored import decisions. The
-- retained original rows grant neither current-source selection nor purity.
data ProgramSourceImports = ProgramSourceImports
  ProgramImportIdentity [(ImportIntent,(String,String))]
  (Map.Map (String,String) ProgramImportOriginal)
  deriving (Eq, Show)

data ProgramImportIdentity = ProgramImportIdentity String String String String [FilePath]
  deriving (Eq, Show)

data ProgramImportOriginal = ProgramImportOriginal
  (ExactIfaceArtifact,FilePath,String) ExactInterfaceEvidence [(String,String)]
  deriving (Eq, Show)

programImportIdentity :: ExactScope -> Maybe ProgramImportIdentity
programImportIdentity scope = do
  admission <- scopeCheckedCell scope
  includes <- scopeIncludePaths scope
  pure (ProgramImportIdentity (scopeRequestSha256 scope) (scopeProducerSha256 scope)
    (scopeSemanticSha256 scope) (checkedAdmissionDigest admission) includes)

programImportOriginal :: ExactScope -> (String,String) -> Maybe ProgramImportOriginal
programImportOriginal scope key = do
  row <- case [row | row@(artifact,_,_) <- scopeInterfaces scope
      , (exactUnit artifact,exactModule artifact) == key] of
    [row] -> Just row
    _ -> Nothing
  proof <- Map.lookup key (scopeInterfaceEvidence scope)
  lexical <- lookup key (scopeLexical scope)
  pure (ProgramImportOriginal row proof lexical)

validateProgramSourceImports :: ExactScope -> ProgramSourceImports -> Either String ()
validateProgramSourceImports scope (ProgramSourceImports identity _ originals) = do
  unless (programImportIdentity scope == Just identity)
    (Left "completed program imports belong to another checked cell")
  forM_ (Map.toAscList originals) $ \(key,original) ->
    unless (programImportOriginal scope key == Just original)
      (Left "completed program import original changed")

programImportClosure :: ExactScope -> (String,String) -> Maybe (Map.Map (String,String) ProgramImportOriginal)
programImportClosure scope root = close (Set.singleton root)
  where
    close selected = do
      originals <- traverse (programImportOriginal scope) (Map.fromSet id selected)
      let children = Set.fromList [child
            | ProgramImportOriginal _ _ lexical <- Map.elems originals, child <- lexical]
          grown = Set.union selected children
      if grown == selected then Just originals else close grown

-- Issue only after the completed products were retained. Existing independently
-- lexical originals may close a fresh root, but source-selected owners still
-- require their own current-source validation and receipt on every pass.
retainProgramSourceImports
  :: Maybe ProgramSourceImports -> PreparedPipelineResult -> FinalizedModuleArtifacts -> ExactScope
  -> IO (Maybe ProgramSourceImports)
retainProgramSourceImports previous prepared captured retained = do
  forM_ previous (either fail pure . validateProgramSourceImports retained)
  case pprDependencies prepared of
    SourceOnly _ -> pure previous
    ExactScoped _ _ (CompletedSourceImports []) -> pure previous
    ExactScoped fresh compilation (CompletedSourceImports roots) -> do
      let admitted = compilationScope compilation
          env = prHscEnv (pprPipelineResult prepared)
      case programImportIdentity admitted of
        Nothing -> pure previous
        Just identity -> do
          unless (programImportIdentity retained == Just identity)
            (fail "completed program imports leave their checked cell")
          either fail pure =<< revalidateExactScope env retained
          let freshNodes = Map.fromList [((dependencyModuleUnit node,dependencyModuleName node),node)
                | node <- dependencyModules fresh, not (dependencyModuleBoot node)]
              sourceMatches key originalRow@(ProgramImportOriginal row@(iface,_,packageSha) evidence lexical) =
                case (Map.lookup key freshNodes, evidence) of
                  (Nothing, _) -> pure (programImportOriginal admitted key == Just originalRow
                    && key `Set.notMember` scopeSourceSelectedOwners admitted)
                  (Just node, ModuleInterfaceEvidence canonical)
                    | Just original <- Map.lookup (mkModuleName (snd key)) (pprFinalizedModules prepared)
                    , mi_module (hm_iface (finalizedHomeModInfo original)) ==
                        mkModule (stringToUnit (fst key)) (mkModuleName (snd key))
                    , Just packages <- Map.lookup (mkModuleName (snd key)) (pprPackageImports prepared)
                    , Just finalized <- Map.lookup key (finalizedLocalAdmissions captured)
                    , localFinalizedInterface finalized == row
                    , localFinalizedSourceSha256 finalized == canonicalSourceSha256 canonical
                    , localFinalizedRequirements finalized == canonicalRequirements canonical
                    , localFinalizedHomeUnits finalized == canonicalHomeUnits canonical
                    , localFinalizedCore finalized ==
                        fmap (\core -> (canonicalCorePath core,canonicalCoreSha256 core)) (canonicalCoreArtifact canonical)
                    , Right imports <- compilationOriginalSourceImports compilation fresh key
                    , canonicalSourceImports canonical == Just imports
                    , not (any (\(_,_,boot,_) -> boot) imports)
                    , sort lexical == sort (nub [(unit,name) | (_,name,False,Just unit) <- imports])
                    , [canonicalSourceSha256 canonical] ==
                        [dependencySourceSha256 source | source <- dependencySources fresh
                          , dependencySourcePath source == dependencyModuleSource node]
                    , packageSha == hexBytes (SHA256.hash (encodePackageImports iface packages)) ->
                        matchesCapturedFinalization finalized original
                  (Just node, ModuleInterfaceEvidence canonical)
                    | [candidate] <- [candidate | candidate <- pprAcceptedCandidates prepared
                        , (candidateUnit candidate,candidateModule candidate) == key]
                    , candidateSource candidate == dependencyModuleSource node
                    , Right imports <- compilationOriginalSourceImports compilation fresh key
                    , canonicalSourceImports canonical == Just imports
                    , not (any (\(_,_,boot,_) -> boot) imports)
                    , sort lexical == sort (nub [(unit,name) | (_,name,False,Just unit) <- imports])
                    , [canonicalSourceSha256 canonical] ==
                        [dependencySourceSha256 source | source <- dependencySources fresh
                          , dependencySourcePath source == dependencyModuleSource node] -> do
                        -- Accepted cached providers keep their original canonical
                        -- capture; they cannot supply a fresh finalization object.
                        -- Bind that capture to this completed pass's source imports
                        -- and independently retained exact interface closure.
                        proof <- validateCandidateCanonicalInterfaceProof
                          (scopeProducerSha256 retained) (scopeInterfaces retained) candidate
                        pure (proof == Right canonical)
                  _ -> pure False
          granted <- fmap catMaybes $ forM roots $ \root@(_,key) -> case programImportClosure retained key of
            Just originals
              | Map.member key freshNodes
              , Set.null (Map.keysSet originals `Set.intersection` scopeSourceSelectedOwners retained) -> do
                  matches <- and <$> mapM (uncurry sourceMatches) (Map.toAscList originals)
                  pure (if matches then Just (root,originals) else Nothing)
            _ -> pure Nothing
          let (oldRoots,oldOriginals) = case previous of
                Nothing -> ([],Map.empty)
                Just (ProgramSourceImports _ requests originals) -> (requests,originals)
              requests = nub (oldRoots ++ map fst granted)
              originals = Map.unions (oldOriginals : map snd granted)
          pure $ if null requests then Nothing else Just (ProgramSourceImports identity requests originals)

withProgramSourceImports :: Maybe ProgramSourceImports -> CompilePurpose -> CompilePurpose
withProgramSourceImports Nothing = id
withProgramSourceImports (Just imports) = CompletedProgramImports imports

programSourceImports :: CompilePurpose -> Maybe ProgramSourceImports
programSourceImports (CompletedProgramImports imports _) = Just imports
programSourceImports (ParsedImportSelection _ inner) = programSourceImports inner
programSourceImports (GeneratedInstanceCheck _ inner) = programSourceImports inner
programSourceImports (GeneratedScaffoldCompile _ inner) = programSourceImports inner
programSourceImports (ExactScopeCompile inner _) = programSourceImports inner
programSourceImports _ = Nothing

-- | Complete direct home imports of a fresh original, combining selected source
-- owners with retained exact owners. Fresh SOURCE imports resolve through their
-- boot witnesses; exact SOURCE imports are refused before pipeline publication.
-- Package imports confer no home requirement.
preparedHomeRequirements :: PreparedPipelineResult -> String -> String -> Either String [(String, String)]
preparedHomeRequirements prepared unit owner = do
  fresh <- selectedFreshHomeRequirements (preparedFreshDependencies prepared) unit owner
  let exact = [(importedUnit,name)
        | compilation <- maybe [] pure (preparedExactCompilation prepared)
        , ((sourceUnit,sourceName,False), edges) <- compilationExactImports compilation
        , sourceUnit == unit, sourceName == owner
        , (_,name,_,importedUnit) <- edges]
  pure (nub (fresh ++ exact))

-- | Metadata has no executable projection. The environment
-- retains dependency interfaces and the target's exact checked reader scope.
data CheckedEnvironmentResult = CheckedEnvironmentResult
  { crHscEnv :: HscEnv
  , crTargetTcGblEnv :: TcGblEnv
  , crTargetRdrEnv :: GlobalRdrEnv
  , crInspectionProbes :: Map.Map String Id
  , crResultType :: Maybe Type
  , crCheckedBinderPins :: [CheckedBinderPin]
  , crWarnings :: [Diag]
  }

selectionKind :: PipelineSelection result -> PreparationKind
selectionKind PreparedStg = PrepareStg
selectionKind (PreparedProducts _) = PrepareStg
selectionKind (PreparedSegmentProducts _ _) = PrepareStg
selectionKind (WithTypedSegmentPreparation _ inner) = selectionKind inner
selectionKind CheckedEnvironment = CheckOnly
selectionKind (CheckedEnvironmentProducts _) = CheckOnly
selectionKind (WithCompilerExecution _ _ selection) = selectionKind selection
selectionKind (WithPreparedModuleCompletion _ selection) = selectionKind selection

capturesProductInterfaces :: PipelineSelection result -> Bool
capturesProductInterfaces (PreparedProducts _) = True
capturesProductInterfaces (PreparedSegmentProducts _ _) = True
capturesProductInterfaces (WithTypedSegmentPreparation _ inner) = capturesProductInterfaces inner
capturesProductInterfaces (WithCompilerExecution _ _ selection) = capturesProductInterfaces selection
capturesProductInterfaces (WithPreparedModuleCompletion _ selection) = capturesProductInterfaces selection
capturesProductInterfaces _ = False

data PipelineResult = PipelineResult
  { prBinds  :: [CoreBind]
  , prTyCons :: [TyCon]
  , prHscEnv :: HscEnv
  -- | Actual thin-interface bytes selected by session injection.
  -- These authorize type-dependency seals, not source or native products.
  , prInjectedSessionInterfaces :: [CapturedSessionInterface]
  -- | New typed capture outputs, kept distinct from injected inputs.
  , prProducedSessionInterfaces :: [CapturedSessionInterface]
  -- | Verified canonical owners selected by this exact compiler request.
  -- Retained home interfaces have no source location in the GHC finder.
  , prCanonicalInterfaceAdmissions :: Map.Map (String,String) CanonicalInterfaceAdmission
  -- | The GHC-inferred type of the target module's @__user@ binding (the eval's
  -- top-level expression), rendered to a string via 'ppr'. 'Nothing' when no
  -- @__user@ binding is present (e.g. fixture/Suite extraction). Captured at the
  -- typecheck stage because our CBOR serializer strips all type information.
  --
  -- This display string is not parser-faithful: 'ppr' can elide qualifiers or
  -- use Unicode. Cross-turn typechecking must use structured type data.
  , prCapturedType :: Maybe String
  -- | Post-zonk types of compiler-reserved local aliases emitted by the cell
  -- checker. These are structured separately from display-only inspection
  -- strings because the runtime replants them into staged statement compiles.
  , prCheckedBinderPins :: [CheckedBinderPin]
  -- | The GHC 'Type' of the target module's @result@ binding, captured for the
  -- value-binding mode. For @result = do { x <- action;
  -- pure x } :: Eff stack T@ this is the FULL @Eff stack T@; 'stripMonadHead'
  -- recovers the bound value type @T@. 'Nothing' when the module has no @result@
  -- binder (every non-bind extraction — reference turns, fixtures, one-shot
  -- evals — so the field is inert off the bind path).
  , prResultType :: Maybe Type
  -- | GHC diagnostic warnings (@-Wincomplete-patterns@, name shadowing, ...)
  -- emitted while compiling the TARGET module — dependency modules (the
  -- preamble, stdlib) are excluded, see 'diagnosticCollectorHook'. Rendered by
  -- GHC's own diagnostic pretty-printer, so a warning carries its
  -- @Expr.hs:<line>:<col>@ location exactly like a compile error does. Empty
  -- on a clean compile.
  , prWarnings :: [String]
  -- | GHC's resolved reader environment for the target module. Inspection
  -- uses this exact scope rather than reconstructing visibility from source.
  , prTargetRdrEnv :: GlobalRdrEnv
  , prTargetTcGblEnv :: TcGblEnv
  }

checkCellInstances
  :: (CellSourcePlan -> IO result)
  -> CellSourcePlan
  -> IO (CellSourcePlan, result)
checkCellInstances compile plan = do
  attempted <- try (compile plan)
  case attempted of
    Right result -> pure (plan, result)
    Left (GeneratedInstanceRejection failure rejected) -> do
      let generic = [name | GenericDuplicate name <- rejected]
            ++ [name | UnsupportedGeneric name <- rejected]
          structural = [name | StructuralDisplayCompanionDuplicate name <- rejected]
            ++ [name | UnsupportedGeneric name <- rejected]
          remaining = omitCellStructuralDisplayDeclarations structural
            (omitCellGenericDeclarations generic plan)
      if remaining == plan
        then throwIO failure
        else checkCellInstances compile remaining

-- | Capture native expression types and effect lifting without probing display
-- instances. Output is an explicit authored effect.
cellExpressionPlans :: CheckedEnvironmentResult -> IO [CellExpressionPlan]
cellExpressionPlans result = map fst <$> cellExpressionEvidence result

cellExpressionEvidence :: CheckedEnvironmentResult -> IO [(CellExpressionPlan, CheckedSignature)]
cellExpressionEvidence result = forM expressionIds $ \identifier -> do
  effectConstructor <- maybe
    (fail "effect-row helper type was not captured") pure
    (capturedEffectConstructor environment)
  uniqueSupply <- mkSplitUniqSupply 'z'
  stableType <- maybe (fail "expression evidence contains a dependently-kinded unresolved type") pure
    (stabilizeCellEvidenceType uniqueSupply (stabilizeEffectRows (idType identifier)))
  when (zonkAnyTyCon `elementOfUniqSet` tyConsOfType stableType) $
    fail "expression evidence still contains an unresolved internal type"
  let occurrence = occNameString (nameOccName (idName identifier))
      (_, _, outerBody) = tcSplitSigmaTy stableType
      liftPlan = case effectResultType effectConstructor outerBody of
        Just _ -> ExpressionEffectful
        Nothing -> ExpressionPure
  let plan = CellExpressionPlan
        { expressionPlanKey = occurrence
        , expressionPlanLift = liftPlan
        , expressionPlanType = renderCellPinType names stableType
        , expressionPlanHeads = nominalHeadsOfType stableType
        }
  signature <- captureCheckedSignature (crHscEnv result) occurrence stableType
  pure (plan, signature)
  where
    environment = crTargetTcGblEnv result
    names = mkNamePprCtx (PromTickCtx True True)
      (hsc_unit_env (crHscEnv result)) (tcg_rdr_env environment)
    expressionIds = Map.elems $ Map.fromList
      [ (occurrence, identifier)
      | identifier <- collectDataIds (tcg_binds environment)
      , let occurrence = occNameString (nameOccName (idName identifier))
      , "__tidepool_cell_expr_" `isPrefixOf` occurrence
      ]

-- The pure preview argument carries the original native input type.
activationPreviewInputType :: TcGblEnv -> Either String Type
activationPreviewInputType environment = do
  preview <- unique "__activationPreview"
  case splitFunTys preview of
    ([Scaled _ input], _) -> Right input
    _ -> Left "activation preview has no single monomorphic input argument"
  where
    unique name = case [idType identifier | identifier <- typeEnvIds (tcg_type_env environment)
      , occNameString (nameOccName (idName identifier)) == name] of
      [ty] -> Right ty
      _ -> Left ("activation preview has no unique checked binder: " ++ name)

cellCheckedBinderSignatures :: CheckedEnvironmentResult -> IO [CheckedSignature]
cellCheckedBinderSignatures result = forM identifiers $ \identifier -> do
  supply <- mkSplitUniqSupply 'z'
  stable <- maybe (fail "checked binder has a dependently-kinded unresolved type") pure
    (stabilizeCellEvidenceType supply (stabilizeEffectRows (idType identifier)))
  when (zonkAnyTyCon `elementOfUniqSet` tyConsOfType stable) $
    fail "checked binder still contains an unresolved internal type"
  captureCheckedSignature (crHscEnv result)
    (occNameString (nameOccName (idName identifier))) stable
  where
    identifiers = Map.elems $ Map.fromList
      [(occNameString (nameOccName (idName identifier)), identifier)
      | identifier <- collectDataIds (tcg_binds (crTargetTcGblEnv result))
      , "__tidepool_cell_pin_" `isPrefixOf` occNameString (nameOccName (idName identifier))]

-- | Solve the class predicate carried by a reserved generated helper after
-- replacing its final type argument with the checked value type. This keeps
-- multi-parameter context such as the exact effect row and uses the resolved
-- class Name rather than source spelling.
satisfiesCapturedConstraint :: HscEnv -> TcGblEnv -> String -> Type -> IO Bool
satisfiesCapturedConstraint hsc environment helper valueType = do
  (constraintClass, constraintArguments) <-
    maybe (fail ("constraint helper was not captured: " ++ helper)) pure captured
  let (_, predicates, body) = tcSplitSigmaTy valueType
  (_, checked) <- initTcWithGbl hsc environment
    (mkRealSrcSpan (mkRealSrcLoc (mkFastString "<display constraint>") 1 1)
      (mkRealSrcLoc (mkFastString "<display constraint>") 1 1)) $ do
      givens <- newEvVars predicates
      inert <- tcCheckGivens emptyInert (listToBag givens)
      case inert of
        Nothing -> pure False
        Just solved -> tcCheckWanteds solved
          [mkClassPred constraintClass (replaceLast constraintArguments body)]
  maybe (fail "captured constraint solver failed") pure checked
  where
    captured = case capturedBindingType helper environment of
      Just helperType -> case tcSplitSigmaTy helperType of
        (_, predicate : _, _) -> getClassPredTys_maybe predicate
        _ -> Nothing
      Nothing -> Nothing
    replaceLast [] _ = []
    replaceLast arguments replacement = init arguments ++ [replacement]

capturedEffectConstructor :: TcGblEnv -> Maybe TyCon
capturedEffectConstructor environment = do
  helperType <- capturedBindingType "__tidepoolInEffectRow" environment
  let (_, _, body) = tcSplitSigmaTy helperType
  (_, _, argument, _) <- splitFunTy_maybe body
  fst <$> splitTyConApp_maybe argument

effectResultType :: TyCon -> Type -> Maybe Type
effectResultType expected ty = do
  (constructor, arguments) <- splitTyConApp_maybe ty
  if tyConUnique constructor == tyConUnique expected
    then case reverse arguments of
      resultType : _ -> Just resultType
      [] -> Nothing
    else Nothing

-- GHC closes unconstrained metavariables in a checked expression with its
-- internal, unexported @ZonkAny@ type. Re-generalize equal placeholders into
-- the same quantified variable before serializing the type into a later
-- source module. This preserves parametric expressions such as @pure id@
-- without exposing an internal type constructor or choosing an arbitrary
-- concrete instantiation.
stabilizeCellEvidenceType :: UniqSupply -> Type -> Maybe Type
stabilizeCellEvidenceType supply ty
  | all hasNoOriginalKindVariables zonkTypes = Just $
      mkInvisForAllTys (map (mkTyVarBinder SpecifiedSpec) sortedVariables) (go ty)
  | otherwise = Nothing
  where
    hasNoOriginalKindVariables zonk = isEmptyVarSet (tyCoVarsOfType kind)
      where kind = typeKind zonk
    zonkTypes = nubBy eqType (collect ty)
    variables = zipWith3 variable [0 :: Int ..] uniques zonkTypes
    sortedVariables = scopedSort variables
    variable index unique zonk = mkTyVar
      (mkInternalName unique
        (mkTyVarOcc ("cell" ++ show index)) noSrcSpan)
      (go (typeKind zonk))
    uniques = let (unique, rest) = takeUniqFromSupply supply
              in unique : uniquesFrom rest
    uniquesFrom current = let (unique, rest) = takeUniqFromSupply current
                          in unique : uniquesFrom rest
    replacements = zip zonkTypes variables
    go zonk@(TyConApp tc _)
      | tc == zonkAnyTyCon
      , Just (_, replacement) <- find (eqType zonk . fst) replacements
      = mkTyVarTy replacement
    go (TyConApp tc args) = TyConApp tc (map go args)
    go (AppTy f x) = AppTy (go f) (go x)
    go (ForAllTy binder body) = ForAllTy binder (go body)
    go (FunTy flag mult arg result) =
      FunTy flag (go mult) (go arg) (go result)
    go (CastTy inner coercion) = CastTy (go inner) coercion
    go other = other
    collect zonk@(TyConApp tc args)
      | tc == zonkAnyTyCon = zonk : concatMap collect args
      | otherwise = concatMap collect args
    collect (AppTy f x) = collect f ++ collect x
    collect (ForAllTy _ body) = collect body
    collect (FunTy _ mult arg result) =
      collect mult ++ collect arg ++ collect result
    collect (CastTy inner _) = collect inner
    collect _ = []

-- The retry recipe names generated candidates; GHC supplies their exact
-- target and class identities before the target's typecheck can fail.
data GeneratedInstanceRecipe = GeneratedInstanceRecipe
  { generatedDisplayAlias :: String
  , generatedGenericTargets :: [String]
  , generatedStructuralTargets :: [String]
  } deriving (Eq, Show)

cellGeneratedInstanceRecipe :: CellSourcePlan -> GeneratedInstanceRecipe
cellGeneratedInstanceRecipe plan = GeneratedInstanceRecipe
  { generatedDisplayAlias = cellPlanStructuralDisplayAlias plan
  , generatedGenericTargets = map genericDeclarationTarget (cellPlanGenericDeclarations plan)
  , generatedStructuralTargets = map structuralDisplayTargetName (cellPlanStructuralDisplayTargets plan)
  }

data GeneratedInstanceFailure
  = GenericDuplicate String
  | UnsupportedGeneric String
  | StructuralDisplayCompanionDuplicate String
  deriving (Eq, Show)

data GeneratedInstanceRejection = GeneratedInstanceRejection SourceError [GeneratedInstanceFailure]

instance Show GeneratedInstanceRejection where
  show (GeneratedInstanceRejection _ failures) = "generated instance rejection: " ++ show failures

instance Exception GeneratedInstanceRejection

data GeneratedInstanceAuthority = GeneratedInstanceAuthority
  { generatedTargetModule :: Module
  , generatedDisplayName :: Maybe Name
  }

-- Capture only from the actual generated import under this admitted compiler
-- environment. Export Names preserve the defining unit through reexports.
withGeneratedInstanceRecovery
  :: GeneratedInstanceRecipe -> HscEnv -> ModSummary -> ParsedModule -> IO a -> IO a
withGeneratedInstanceRecovery recipe environment summary parsed action = do
  displayName <- if null (generatedStructuralTargets recipe)
    then pure Nothing
    else Just <$> captureDisplayName
  let authority = GeneratedInstanceAuthority (ms_mod summary) displayName
  action `catch` \failure -> case rejectedCellInstances recipe authority failure of
    [] -> throwIO failure
    rejected -> throwIO (GeneratedInstanceRejection failure rejected)
  where
    captureDisplayName = do
      declaration <- case
          [decl | L _ decl <- hsmodImports (unLoc (pm_parsed_source parsed))
          , fmap unLoc (ideclAs decl) == Just (mkModuleName (generatedDisplayAlias recipe))] of
        [found] -> pure found
        _ -> fail "generated Display companion has no unique compiler import"
      let importedName = unLoc (ideclName declaration)
          qualifier = renameRawPkgQual (hsc_unit_env environment) importedName (ideclPkgQual declaration)
      resolved <- findImportedModule environment importedName qualifier
      owner <- case resolved of
        Found _ found -> pure found
        _ -> fail "generated Display companion import cannot be resolved"
      loaded <- initIfaceCheck (ppr importedName) environment $
        loadInterface (ppr importedName) owner (ImportByUser (ideclSource declaration))
      iface <- case loaded of
        MaybeErr.Succeeded found | mi_module found == owner -> pure found
        _ -> fail "generated Display companion import has no admitted interface"
      exports <- either fail pure (selectedImportNames (mi_exports iface) (ideclImportList declaration))
      case nub [name | name <- exports, isTcOcc (nameOccName name)
                    , occNameString (nameOccName name) == "Display"] of
        [name] -> pure name
        _ -> fail "generated Display companion import has no unique Display export"

rejectedCellInstances
  :: GeneratedInstanceRecipe -> GeneratedInstanceAuthority -> SourceError -> [GeneratedInstanceFailure]
rejectedCellInstances recipe authority failure = nub
  [ rejection occurrence
  | envelope <- toList (getMessages (srcErrorMessages failure))
  , GhcTcRnMessage diagnostic <- [errMsgDiagnostic envelope]
  , (rejection, candidates, ty) <- rejectedTypes diagnostic
  , Just (constructor, _) <- [splitTyConApp_maybe ty]
  , nameModule_maybe (tyConName constructor) == Just (generatedTargetModule authority)
  , let occurrence = occNameString (nameOccName (tyConName constructor))
  , occurrence `elem` candidates
  ]
  where
    generic = generatedGenericTargets recipe
    structural = generatedStructuralTargets recipe
    rejectedTypes (TcRnMessageWithInfo _ (TcRnMessageDetailed _ message)) = rejectedTypes message
    rejectedTypes (TcRnWithHsDocContext _ message) = rejectedTypes message
    rejectedTypes (TcRnCannotDeriveInstance cls types _ _ (DerivErrGenerics _))
      | nameUnique (className cls) == genClassKey = [(UnsupportedGeneric, generic, ty) | ty <- types]
    rejectedTypes (TcRnDupInstanceDecls _ instances) =
      [ (rejection, candidates, ty)
      | instance' <- toList instances
      , (rejection, candidates) <-
          if nameUnique (className (is_cls instance')) == genClassKey
            then [(GenericDuplicate, generic)]
            else [(StructuralDisplayCompanionDuplicate, structural)
                 | Just (className (is_cls instance')) == generatedDisplayName authority]
      , ty <- is_tys instance' ]
    -- A derived consumer can demand Generic before duplicate-instance
    -- validation runs. Use the solver's actual matching instances, with the
    -- same class/target identity checks as declaration-time recovery.
    rejectedTypes (TcRnSolverReport (SolverReportWithCtxt
      { reportContent = TcError.OverlappingInstances { TcError.overlappingInstances_matches = instances } }) _)
      | let matches = [instance' | instance' <- toList instances
              , nameUnique (className (is_cls instance')) == genClassKey]
      , length matches >= 2 =
          [(GenericDuplicate, generic, ty) | instance' <- matches, ty <- is_tys instance']
    rejectedTypes (TcRnConflictingFamInstDecls instances) =
      [(GenericDuplicate, generic, ty) | instance' <- toList instances
      , nameUnique (fi_fam instance') == repTyConKey, ty <- fi_tys instance']
    rejectedTypes _ = []

-- | Resolve the target module's 'ModuleName' for one input file. Prefers the
-- declared name from a conventional @module ... where@ header (via
-- 'Tidepool.TurnSource.extractModuleName'), so a hierarchical module such as
-- @Tidepool.Session.Val.G1@ (declared in a file whose bare basename is only
-- @G1@) is recognised by its own dotted name rather than reduced to that
-- basename. Falls back to the historical @capitalize (takeBaseName path)@
-- derivation when the file has no recognisable header, or can't be read.
-- Every existing caller compiles a flat module whose declared name already
-- equals its basename, so this changes nothing for them.
targetModuleNameFor :: FilePath -> IO ModuleName
targetModuleNameFor path = do
  contents <- try (readFile' path) :: IO (Either IOException String)
  let declared = either (const Nothing) extractModuleName contents
  pure (mkModuleName (fromMaybe (capitalize (takeBaseName path)) declared))

runPipelineSelected :: PipelineSelection result -> FilePath -> [FilePath] -> IO result
runPipelineSelected selection path includes =
  runPipelineSessionSelected selection Set.empty GeneralCompile Nothing path includes Nothing

-- | Like 'runPipelineSelected', but withholding unfoldings for the given
-- retained-generation symbols from GHC's own simplifier -- see
-- 'Tidepool.RetainedUnfoldings' for the mechanism and why it must run this
-- early. Used today only by 'test-prepared-stg/ExecutionProjectionTest.hs';
-- 'app/Main.hs' threads a live request's @--retained-generation@ set through
-- 'runPipelineSessionSelected' directly for its one-shot (non-resident)
-- 'PreparedStg' compiles, and through 'withResidentPipelineSelected' for its
-- resident-daemon compiles.
runPipelineSelectedRetaining
  :: PipelineSelection result -> Set.Set SymbolIdentity -> FilePath -> [FilePath] -> IO result
runPipelineSelectedRetaining selection retained path includes = do
  producer <- captureCompilerProducerIdentity
  variant <- normalVariant GeneralCompile path
  runCompile selection retained variant {pvCompilerProducer = producer} path includes Nothing

-- The process owner supplies this configuration before any authored compiler
-- effect runs. A resident compiler retains it across requests and recovery;
-- candidates and mutable source code cannot replace their own authority.
newtype CompilerProducerIdentity = CompilerProducerIdentity String
  deriving (Eq, Show)

captureCompilerProducerIdentity :: IO (Maybe CompilerProducerIdentity)
captureCompilerProducerIdentity = lookupEnv "TIDEPOOL_COMPILER_PRODUCER" >>= traverse decode
  where
    decode raw
      | length raw /= 64 || any (`notElem` ("0123456789abcdef" :: String)) raw
          || all (== '0') raw = fail "invalid compiler launch producer identity"
      | otherwise = pure (CompilerProducerIdentity (concatMap byteHex
          (BS.unpack (SHA256.hash (BS.pack (pairs raw))))))
    pairs [] = []
    pairs (first:second:remaining) =
      fromIntegral (16 * digitToInt first + digitToInt second) : pairs remaining
    pairs _ = error "validated compiler producer has odd length"
    byteHex byte = let rendered = showHex byte "" in replicate (2-length rendered) '0' ++ rendered

-- ---------------------------------------------------------------------------
-- The shared compile loop and its two seams
--
-- 'runCompileCycle' owns load-time finalization, dependency-ordered deferred
-- finalization, demand-driven STG preparation and the result merge. Normal and
-- session compilation supply their admission and injection seams through one
-- 'PipelineVariant'.
-- ---------------------------------------------------------------------------

-- | Which finalized modules proceed to prepared STG.
data TierPolicy
  = OptimizeEveryModule
    -- ^ Prepare each module in dependency order.
  | OptimizeCoreReachable
    -- ^ Finalize every source owner in dependency order, then prepare only
    -- the target's desugared Core reference closure.

-- | A pipeline variant: everything the shared skeleton cannot decide for
-- itself. 'pvPlan' runs after @depanal@ (it needs the downsweep graph) and
-- before @load'@.
data PipelineVariant = PipelineVariant
  { pvLabel :: String
    -- ^ Prefix on this variant's own error messages.
  , pvPurpose :: CompilePurpose
    -- ^ Original request purpose, shared by all compile variants.
  , pvExactScope :: Maybe ExactScope
    -- ^ Admitted immutable declaration owners, independent of live values.
  , pvCompilerProducer :: Maybe CompilerProducerIdentity
  , pvGeneratedScaffold :: Maybe GeneratedScaffoldRecipe
    -- ^ The protected target and its compiler-owned support import.
  , pvDownsweepExcludes :: [ModuleName]
    -- ^ Modules @depanal@ must NOT try to summarise (the session path's
    -- source-less @Val.G\<g\>@ ifaces). Empty on the normal path.
  , pvPlan :: FilePath -> Bool -> ModuleGraph -> Maybe ExactScope -> Ghc CompilePlan
    -- ^ @pvPlan compilerViewDirectory timingEnabled downsweepGraph selectedExactScope@.
  , pvGeneratedInstanceCheck :: Maybe GeneratedInstanceRecipe
  , pvSourceImportIntents :: [ImportIntent]
  , pvTransformParsed :: HscEnv -> ModSummary -> ParsedModule -> IO NativeParsedModule
  }

candidateManifestFor :: PipelineSelection result -> Maybe FilePath
candidateManifestFor (PreparedProducts path) = path
candidateManifestFor (PreparedSegmentProducts _ path) = path
candidateManifestFor (WithTypedSegmentPreparation _ inner) = candidateManifestFor inner
candidateManifestFor (CheckedEnvironmentProducts path) = Just path
candidateManifestFor (WithCompilerExecution _ _ selection) = candidateManifestFor selection
candidateManifestFor (WithPreparedModuleCompletion _ selection) = candidateManifestFor selection
candidateManifestFor _ = Nothing

executionGrantFor :: PipelineSelection result -> CompilerExecutionGrant
executionGrantFor (WithCompilerExecution grant _ _) = grant
executionGrantFor (WithPreparedModuleCompletion _ selection) = executionGrantFor selection
executionGrantFor (WithTypedSegmentPreparation _ selection) = executionGrantFor selection
executionGrantFor _ = serialCompilerExecutionGrant

executorFor :: PipelineSelection result -> Maybe CompilerExecutor
executorFor (WithCompilerExecution _ executor _) = Just executor
executorFor (WithPreparedModuleCompletion _ selection) = executorFor selection
executorFor (WithTypedSegmentPreparation _ selection) = executorFor selection
executorFor _ = Nothing

completionFactoryFor :: PipelineSelection result -> Maybe PreparedModuleCompletion
completionFactoryFor (WithCompilerExecution _ _ selection) = completionFactoryFor selection
completionFactoryFor (WithPreparedModuleCompletion factory _) = Just factory
completionFactoryFor (WithTypedSegmentPreparation _ selection) = completionFactoryFor selection
completionFactoryFor _ = Nothing

typedPreparationFor :: PipelineSelection result -> Maybe TypedSegmentPreparation
typedPreparationFor (WithTypedSegmentPreparation prepare _) = Just prepare
typedPreparationFor (WithCompilerExecution _ _ inner) = typedPreparationFor inner
typedPreparationFor (WithPreparedModuleCompletion _ inner) = typedPreparationFor inner
typedPreparationFor _ = Nothing

typedPlanFor :: PipelineSelection result -> Maybe TypedSegmentPlan
typedPlanFor (PreparedSegmentProducts plan _) = Just plan
typedPlanFor (WithTypedSegmentPreparation _ inner) = typedPlanFor inner
typedPlanFor (WithCompilerExecution _ _ inner) = typedPlanFor inner
typedPlanFor (WithPreparedModuleCompletion _ inner) = typedPlanFor inner
typedPlanFor _ = Nothing

-- Body demand is a property of the request, not of whether its environment
-- came from a session. GHC still loads and finalizes the complete selected
-- source graph for both tiers; only the native STG handoff is demand-driven.
nativeBodyTier :: CompilePurpose -> TierPolicy
nativeBodyTier purpose
  -- Explicit certification prepares the complete source cohort. Optional
  -- native originals satisfy ordinary demand without expanding fresh work.
  | originalPurpose purpose == CertifyHomeProductsCompile = OptimizeEveryModule
  | otherwise = OptimizeCoreReachable

compilerProducerFor :: PipelineVariant -> Maybe String
compilerProducerFor variant = case pvCompilerProducer variant of
  Just (CompilerProducerIdentity producer) -> Just producer
  Nothing -> Nothing

exactCompileCycle :: PipelineSelection result -> PipelineVariant -> Bool
exactCompileCycle selection variant =
  isJust (candidateManifestFor selection) || isJust (pvExactScope variant)

-- The worker owns one interpreter. An interrupted retirement leaves it
-- unusable; a later request must not borrow its partially unloaded symbols.
data ResidentAvailability = ResidentAvailable | ResidentBusy | ResidentPoisoned | ResidentClosed
  deriving (Eq)

data CompilerTransactionFailure
  = CompilerTransactionBusy
  | CompilerTransactionPoisoned
  | CompilerTransactionReleased
  | CompilerTransactionWrongThread
  | CompilerTransactionFailed
  deriving (Show)

instance Exception CompilerTransactionFailure

data CompilerPhase = CompilerReady | CompilerRunning | CompilerFailed | CompilerClosed
  deriving (Eq)

-- A loader can physically load an object before its registry update fails.
-- Only successful settlement can confirm the attempt's interpreter boundary.
data InterpreterAttemptState = InterpreterConfirmed | InterpreterMutationPending
  deriving (Eq)

-- Legacy value injection is mutable and cannot select a completed exact view.
data ExactReuseAdmission = ScopeAuthenticatedValueInputs | UnsealedSessionValueInputs

exactReuseAdmission :: ExactScope -> Maybe SessionScope -> ExactReuseAdmission
exactReuseAdmission scope session
  | null requested = ScopeAuthenticatedValueInputs
  | scopePurpose scope == NoCheckedPurpose = UnsealedSessionValueInputs
  | all ((`Set.member` admitted) . moduleNameString) requested = ScopeAuthenticatedValueInputs
  | otherwise = UnsealedSessionValueInputs
  where
    requested = maybe [] (map renderSessionModule . ssValIfaces) session
    admitted = Set.fromList (map exactModule (scopeValueInterfaces scope))

data CompilerRecoveryCaches = CompilerRecoveryCaches
  { compilerFatIface :: FatIfaceCache
  , compilerOwnerIface :: OwnerInterfaceCache
  , compilerPreparedBodies :: PreparedBodyCache
  , compilerOriginalProjections :: OriginalProjectionCache
  }

freshCompilerRecoveryCaches :: IO CompilerRecoveryCaches
freshCompilerRecoveryCaches = CompilerRecoveryCaches
  <$> newFatIfaceCache <*> newOwnerInterfaceCache <*> newPreparedBodyCache <*> newOriginalProjectionCache

evictCompilerRecovery :: CompilerRecoveryCaches -> (Module -> Bool) -> IO ()
evictCompilerRecovery caches stale = do
  evictFatIfaceMatching (compilerFatIface caches) stale
  evictOwnerInterfaceMatching (compilerOwnerIface caches) stale
  evictPreparedBodyMatching (compilerPreparedBodies caches) stale
  evictOriginalProjectionMatching (compilerOriginalProjections caches) stale

-- The universe is fixed by the booted package closure, compiler producer and
-- base compilation policy. Changing a request's lexical view cannot discard
-- its loaded package facts or completed module versions. Attempts fork its
-- home resolution cells; lazy retained interfaces share their fixed package
-- state owner with the selected attempt.
data CompilerUniverse = CompilerUniverse
  { universeEnvironment :: HscEnv
  , universeSourceVersions :: Map.Map Module (Map.Map MemoSelectionKey (Map.Map MemoValidity CompletedModuleVersion))
  , universeOriginalVersions :: Map.Map (Module, HomeDependencyDigest, Maybe String) CompletedOriginalVersion
  , universeRecovery :: RecoveryContext
  , universeInterpreter :: IORef CompilerInterpreterState
  , universePackageFinder :: PackageFinderFacts
  }

-- Executable seals come from the same module-version owner as the products.
-- Timestamps associate GHC's loaded linkable with a seal; they do not prove
-- unchanged code. Keeping an inactive module also requires its old dependency
-- closure to remain compatible with every currently selected replacement.
data CompilerExecutableVersion
  = SourceExecutableVersion MemoValidity
  | OriginalExecutableVersion HomeDependencyDigest (Maybe String)
  deriving (Eq)
data CompilerInterpreterState = CompilerInterpreterState
  { compilerInterpreterEpoch :: Word64
  , compilerLoadedExecutables :: Map.Map Module CompilerExecutableContext
  }

data CompilerExecutableContext = CompilerExecutableContext
  { executableVersion :: CompilerExecutableVersion
  , executableDependencies :: Set.Set Module
  }

data RecoveryContext = RecoveryContext
  { recoveryContextIdentity :: RequestUnique.Unique
  , recoveryContextCaches :: CompilerRecoveryCaches
  }

data CompletedModuleVersion = CompletedModuleVersion
  { completedModuleEntry :: GutsMemoEntry
  , completedModuleEnvironment :: HscEnv
  , completedModuleHome :: Maybe HomeModInfo
  , completedModuleRecovery :: RecoveryContext
  , completedModuleEpoch :: Word64
  , completedModuleProducts :: [ModuleProduct]
  }

data CompletedOriginalVersion = CompletedOriginalVersion
  { completedOriginalEnvironment :: HscEnv
  , completedOriginalHome :: HomeModInfo
  , completedOriginalRecovery :: RecoveryContext
  , completedOriginalEpoch :: Word64
  }

-- Only the active attempt carries targets and a mutable memo. It is released
-- after the request; immutable dependency nodes survive independently.
data ActiveCompilerAttempt = ActiveCompilerAttempt
  { activeEnvironment :: HscEnv
  , activeRecovery :: CompilerRecoveryCaches
  }

data CycleState
  = StandaloneCycle
  | TransactionCycle ModIfaceCache (IORef GutsMemo) (Maybe (Either String CapturedCandidateManifest))
      (Map.Map Module (Map.Map MemoSelectionKey (Map.Map MemoValidity CompletedModuleVersion)))
      (Map.Map Module CompletedModuleVersion -> IO ())
      (IORef CompilerInterpreterState) PackageFinderFacts (IORef InterpreterAttemptState)

data CompilePurpose = GeneralCompile | LookupTypeCompile | CertifyHomeProductsCompile | OriginalDeclarationCompile
  | CheckedItemCompile [(String,CheckedSignature)] (Maybe ((String,String),String)) [CompletedValueImport]
  | HostActivationPreviewCompile CheckedSignature
  | ProgramItemCompile Bool [(String,CheckedSignature)] [((String,String),String)] [CompletedValueImport]
  | TypedSegmentCompile TypedSegmentPlan GeneratedSegmentOperations CompilePurpose
  | PlannedDeclarationCheck PlannedDeclarationInventory ExactScope
  | ExactScopeCompile CompilePurpose ExactScope
  | GeneratedScaffoldCompile GeneratedScaffoldRecipe CompilePurpose
  | GeneratedInstanceCheck GeneratedInstanceRecipe CompilePurpose
  | ParsedImportSelection [ImportIntent] CompilePurpose
  | CompletedProgramImports ProgramSourceImports CompilePurpose
  deriving (Eq, Show)

-- The original parser supplies demand intent; rendered template imports and
-- inferred type requirements cannot manufacture a current-source request.
withSourceImportIntents :: SourcePrologue -> CompilePurpose -> CompilePurpose
withSourceImportIntents prologue = ParsedImportSelection
  (map locatedImportIntent (prologueImports prologue))

sourceImportIntents :: CompilePurpose -> [ImportIntent]
sourceImportIntents (TypedSegmentCompile _ _ inner) = sourceImportIntents inner
sourceImportIntents (ParsedImportSelection intents inner) = intents ++ sourceImportIntents inner
sourceImportIntents (CompletedProgramImports _ inner) = sourceImportIntents inner
sourceImportIntents (GeneratedInstanceCheck _ inner) = sourceImportIntents inner
sourceImportIntents (GeneratedScaffoldCompile _ inner) = sourceImportIntents inner
sourceImportIntents (ExactScopeCompile inner _) = sourceImportIntents inner
sourceImportIntents _ = []

generatedInstanceRecipe :: CompilePurpose -> Maybe GeneratedInstanceRecipe
generatedInstanceRecipe (TypedSegmentCompile _ _ inner) = generatedInstanceRecipe inner
generatedInstanceRecipe (ParsedImportSelection _ inner) = generatedInstanceRecipe inner
generatedInstanceRecipe (CompletedProgramImports _ inner) = generatedInstanceRecipe inner
generatedInstanceRecipe (GeneratedInstanceCheck recipe _) = Just recipe
generatedInstanceRecipe (GeneratedScaffoldCompile _ inner) = generatedInstanceRecipe inner
generatedInstanceRecipe (ExactScopeCompile inner _) = generatedInstanceRecipe inner
generatedInstanceRecipe _ = Nothing

-- A request supplies its admitted baseline; an inner ordered-cell stage may
-- carry a scope extended with freshly admitted originals from that request.
purposeExactScope :: CompilePurpose -> Maybe ExactScope
purposeExactScope (TypedSegmentCompile _ _ inner) = purposeExactScope inner
purposeExactScope (ParsedImportSelection _ inner) = purposeExactScope inner
purposeExactScope (CompletedProgramImports _ inner) = purposeExactScope inner
purposeExactScope (GeneratedInstanceCheck _ inner) = purposeExactScope inner
purposeExactScope (GeneratedScaffoldCompile _ inner) = purposeExactScope inner
purposeExactScope (ExactScopeCompile inner scope) = purposeExactScope inner <|> Just scope
purposeExactScope (PlannedDeclarationCheck _ scope) = Just scope
purposeExactScope _ = Nothing

generatedRecipe :: CompilePurpose -> Maybe GeneratedScaffoldRecipe
generatedRecipe (TypedSegmentCompile _ _ inner) = generatedRecipe inner
generatedRecipe (ParsedImportSelection _ inner) = generatedRecipe inner
generatedRecipe (CompletedProgramImports _ inner) = generatedRecipe inner
generatedRecipe (GeneratedScaffoldCompile recipe _) = Just recipe
generatedRecipe (ExactScopeCompile inner _) = generatedRecipe inner
generatedRecipe (GeneratedInstanceCheck _ inner) = generatedRecipe inner
generatedRecipe _ = Nothing

originalPurpose :: CompilePurpose -> CompilePurpose
originalPurpose (TypedSegmentCompile _ _ inner) = originalPurpose inner
originalPurpose (ParsedImportSelection _ inner) = originalPurpose inner
originalPurpose (CompletedProgramImports _ inner) = originalPurpose inner
originalPurpose (GeneratedScaffoldCompile _ inner) = originalPurpose inner
originalPurpose (ExactScopeCompile inner _) = originalPurpose inner
originalPurpose (GeneratedInstanceCheck _ inner) = originalPurpose inner
originalPurpose purpose = purpose

typedPurposePlan :: CompilePurpose -> Maybe TypedSegmentPlan
typedPurposePlan (TypedSegmentCompile plan _ _) = Just plan
typedPurposePlan (ParsedImportSelection _ inner) = typedPurposePlan inner
typedPurposePlan (CompletedProgramImports _ inner) = typedPurposePlan inner
typedPurposePlan (GeneratedInstanceCheck _ inner) = typedPurposePlan inner
typedPurposePlan (GeneratedScaffoldCompile _ inner) = typedPurposePlan inner
typedPurposePlan (ExactScopeCompile inner _) = typedPurposePlan inner
typedPurposePlan _ = Nothing

transformFor :: CompilePurpose -> ModuleName -> HscEnv -> ModSummary -> ParsedModule -> IO NativeParsedModule
transformFor GeneralCompile _ _ _ = pure . unannotatedModule
transformFor OriginalDeclarationCompile target env summary
  | ms_mod_name summary == target = fmap unannotatedModule . refineParsedDeclarationImports env
  | otherwise = pure . unannotatedModule
transformFor CertifyHomeProductsCompile _ _ _ = pure . unannotatedModule
transformFor LookupTypeCompile target _ summary
  | ms_mod_name summary == target = pure . unannotatedModule . normalizeLookupWildcards
  | otherwise = pure . unannotatedModule
transformFor (CheckedItemCompile annotations original _) target env summary
  | ms_mod_name summary == target = \parsed -> do
      annotated <- rewriteCheckedAnnotations env annotations parsed
      case original of
        Nothing -> pure annotated
        Just (owner, fingerprint) -> do
          inventory <- hydratePlannedDeclarationInventory owner fingerprint env >>= either fail pure
          mapNativeModule (transformPlannedDeclarationImports inventory env) annotated
  | otherwise = pure . unannotatedModule
transformFor (HostActivationPreviewCompile signature) target env summary
  | ms_mod_name summary == target = rewriteHostInputType env 1 signature
  | otherwise = pure . unannotatedModule
transformFor (ProgramItemCompile original annotations originals _) target env summary
  | ms_mod_name summary == target = \parsed -> do
      annotated <- rewriteCheckedAnnotations env annotations parsed
      inventories <- mapM (\(owner,fingerprint) -> hydratePlannedDeclarationInventory owner fingerprint env >>= either fail pure) originals
      selected <- mapNativeModule (transformProgramDeclarationImports inventories Nothing env) annotated
      if original then mapNativeModule (refineParsedDeclarationImports env) selected else pure selected
  | otherwise = pure . unannotatedModule
transformFor (PlannedDeclarationCheck inventory _) target env summary
  | ms_mod_name summary == target = fmap unannotatedModule . transformPlannedDeclarationImports inventory env
  | otherwise = pure . unannotatedModule
transformFor (TypedSegmentCompile plan operations inner) target env summary
  | ms_mod_name summary == target = \parsed ->
      transformFor inner target env summary parsed >>= mapNativeModule (rewriteParsedSegmentRoot operations plan)
  | otherwise = transformFor inner target env summary
transformFor (ExactScopeCompile purpose _) target env summary = transformFor purpose target env summary
transformFor (GeneratedScaffoldCompile _ purpose) target env summary = transformFor purpose target env summary
transformFor (GeneratedInstanceCheck _ purpose) target env summary = transformFor purpose target env summary
transformFor (ParsedImportSelection _ purpose) target env summary = transformFor purpose target env summary
transformFor (CompletedProgramImports _ purpose) target env summary = transformFor purpose target env summary

transformWithCompletedValues :: Maybe CompletedValueImports -> CompilePurpose -> ModuleName
  -> HscEnv -> ModSummary -> ParsedModule -> IO NativeParsedModule
transformWithCompletedValues captured purpose target env summary = case purpose of
  TypedSegmentCompile plan operations inner
    | ms_mod_name summary == target -> \parsed ->
        transformWithCompletedValues captured inner target env summary parsed
          >>= mapNativeModule (rewriteParsedSegmentRoot operations plan)
    | otherwise -> transformWithCompletedValues captured inner target env summary
  ParsedImportSelection _ inner -> transformWithCompletedValues captured inner target env summary
  CompletedProgramImports _ inner -> transformWithCompletedValues captured inner target env summary
  ExactScopeCompile inner _ -> transformWithCompletedValues captured inner target env summary
  GeneratedScaffoldCompile _ inner -> transformWithCompletedValues captured inner target env summary
  GeneratedInstanceCheck _ inner -> transformWithCompletedValues captured inner target env summary
  CheckedItemCompile annotations original requested
    | ms_mod_name summary == target && not (null requested) -> \parsed -> do
        values <- maybe (fail "completed value interfaces were not installed in this request") pure captured
        annotated <- rewriteCheckedAnnotations env annotations parsed
        case original of
          Nothing -> mapNativeModule (transformCompletedValueImports values env) annotated
          Just (owner, fingerprint) -> do
            inventory <- hydratePlannedDeclarationInventory owner fingerprint env >>= either fail pure
            mapNativeModule (transformPlannedDeclarationImportsWithCompleted inventory values env) annotated
  ProgramItemCompile original annotations originals requested
    | ms_mod_name summary == target -> \parsed -> do
        values <- if null requested then pure Nothing else
          Just <$> maybe (fail "program completed interfaces were not installed") pure captured
        annotated <- rewriteCheckedAnnotations env annotations parsed
        inventories <- mapM (\(owner,fingerprint) -> hydratePlannedDeclarationInventory owner fingerprint env >>= either fail pure) originals
        selected <- mapNativeModule (transformProgramDeclarationImports inventories values env) annotated
        if original then mapNativeModule (refineParsedDeclarationImports env) selected else pure selected
  _ -> transformFor purpose target env summary

withNativeTypecheckRecovery :: PipelineVariant -> ModuleName -> HscEnv -> ModSummary -> ParsedModule -> IO a -> IO a
withNativeTypecheckRecovery variant target environment summary parsed action
  | ms_mod_name summary /= target || ms_hsc_src summary /= HsSrcFile = action
  | otherwise = case pvGeneratedInstanceCheck variant of
      Nothing -> action
      Just recipe -> withGeneratedInstanceRecovery recipe environment summary parsed action

data ResultBoundary = NativeMergeBoundary | CheckedReceiptBoundary

-- | The seam values for one run, derived from the downsweep graph.
data CompilePlan = CompilePlan
  { cpLoadGraph :: ModuleGraph
    -- ^ The graph handed to @load'@ (the skeleton applies @unpoison@ itself).
  , cpAfterLoad :: Ghc ()
    -- ^ Runs immediately after @load'@ and its @ghc_load@ phase emit, before
    -- summaries are taken, after the shared load barrier. The session path
    -- restores its module graph here; dependency-directed Val injection occurs at
    -- 'cpBeforeModule'.
  , cpSummaries :: Ghc [ModSummary]
    -- ^ The modules to compile, in compile ORDER, BEFORE the hs-boot filter
    -- (which is the skeleton's, at one site).
  , cpKeepPrivateResult :: Bool
  , cpResultBinders :: [String]
    -- ^ OccNames to try, in order, for 'prResultType' — the @result@ vs
    -- @__result@ convention, which differs by wrapper.
  , cpBeforeModule :: ModSummary -> Ghc ()
    -- ^ Runs immediately before one summary is reused or compiled. The
    -- session path injects value ifaces here, after their declaration-module
    -- dependencies have entered the HPT and before the first importer needs
    -- them.
  , cpBeforeMerge :: ResultBoundary -> Ghc ()
    -- ^ Runs after the compile loop and its phase emits, before the guts are
    -- merged. Native results validate the current scope here; metadata's
    -- terminal proof belongs to its checked receipt publication owner.
  , cpInjectedSessionInterfaces :: IO [CapturedSessionInterface]
  , cpFinalEnv :: HscEnv -> HscEnv
    -- ^ Applied to the post-loop session before it becomes 'prHscEnv'.
  }

-- | A deferred source's transient parse/typecheck/desugar handoff. It is
-- consumed immediately by finalization, never stored in a reusable product.
data ModuleFront = ModuleFront
  { mfSummary    :: ModSummary
  , mfHscEnv     :: HscEnv
  , mfTcGblEnv   :: TcGblEnv
  , mfPackageImports :: PackageImportEvidence
  , mfDesugared  :: ModGuts
  , mfCapturedType :: Maybe String
  , mfCheckedBinderPins :: [CheckedBinderPin]
  , mfResultType :: Maybe Type
  , mfReferencedModules :: Set.Set Module
  , mfQuasiQuoteUse :: !QuasiQuoteUse
    -- ^ Classified once from the parsed source in 'compileFront'; see
    -- 'classifyQuasiQuoteUse'.
  , mfHasDependentFiles :: Bool
    -- ^ Request-time inputs recorded by this module's typecheck.
  }

-- A request-local finalized frontend has no mutable compiler environment.
-- Site preparation reads instances from its canonical ModDetails.
data LoadedModule = LoadedModule
  { loadedSummary :: ModSummary
  , loadedFacts :: ModuleFacts
  , loadedOutput :: ModuleOutput
  , loadedFinalized :: FinalizedModule
  }

data CanonicalFrontendFailure
  = CustomLoadPhaseHook
  | CustomLoadFrontendHook
  | UnsupportedLoadBackend
  | CompilerProducerUnavailable
  | CompilerProducerScopeMismatch
  | LoadedFinalizationOwnerMismatch
  | MissingLoadedFrontend
  | MissingLoadedFinalization
  | UnfinishedLoadedFrontend
  | CandidateInterfaceBytesMismatch ModuleName
  | CandidateOriginalHomeMissing ModuleName
  | OriginalNativeHomeMissing Module
  | OriginalNativeInterfaceMismatch Module
  | OriginalNativeNameConflict Name
  | CandidateFrontendReplayRefused ModuleName
  | MissingFinalizedFacts ModuleName

instance Show CanonicalFrontendFailure where
  showsPrec precedence failure = case failure of
    CustomLoadPhaseHook -> showString "CustomLoadPhaseHook"
    CustomLoadFrontendHook -> showString "CustomLoadFrontendHook"
    UnsupportedLoadBackend -> showString "UnsupportedLoadBackend"
    CompilerProducerUnavailable -> showString "CompilerProducerUnavailable"
    CompilerProducerScopeMismatch -> showString "CompilerProducerScopeMismatch"
    LoadedFinalizationOwnerMismatch -> showString "LoadedFinalizationOwnerMismatch"
    MissingLoadedFrontend -> showString "MissingLoadedFrontend"
    MissingLoadedFinalization -> showString "MissingLoadedFinalization"
    UnfinishedLoadedFrontend -> showString "UnfinishedLoadedFrontend"
    CandidateInterfaceBytesMismatch owner -> argument "CandidateInterfaceBytesMismatch" (showsPrec 11 owner)
    CandidateOriginalHomeMissing owner -> argument "CandidateOriginalHomeMissing" (showsPrec 11 owner)
    OriginalNativeHomeMissing owner -> renderGhc "OriginalNativeHomeMissing" owner
    OriginalNativeInterfaceMismatch owner -> renderGhc "OriginalNativeInterfaceMismatch" owner
    OriginalNativeNameConflict name -> renderGhc "OriginalNativeNameConflict" name
    CandidateFrontendReplayRefused owner -> argument "CandidateFrontendReplayRefused" (showsPrec 11 owner)
    MissingFinalizedFacts owner -> argument "MissingFinalizedFacts" (showsPrec 11 owner)
    where
      argument label value = showParen (precedence > 10)
        (showString label . showChar ' ' . value)
      renderGhc :: Outputable a => String -> a -> ShowS
      renderGhc label value = argument label (showString (showSDocUnsafe (ppr value)))

instance Exception CanonicalFrontendFailure

-- The interpreter backend completes the same partial interface and tidy Core.
-- This handoff exists only between PostTc and Backend in one load operation.
data PendingFinalization = PendingFinalization
  { pendingSummary :: ModSummary
  , pendingFacts :: ModuleFacts
  , pendingOutput :: ModuleOutput
  , pendingTidyGuts :: CgGuts
  , pendingDetails :: ModDetails
  , pendingEnvironment :: HscEnv
  }

-- Exact artifact operations have no authored source target. They use the
-- extractor's same target/package flags, then create their own fresh lexical
-- scope before loading any interface.
withExactInterfaceTransaction :: [FilePath] -> (HscEnv -> IO a) -> IO a
withExactInterfaceTransaction includes use = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags (extractionDynFlags dflags includes)
    getSession >>= liftIO . use

runCompile :: PipelineSelection result -> Set.Set SymbolIdentity -> PipelineVariant -> FilePath -> [FilePath] -> Maybe FilePath -> IO result
runCompile selection retained variant path includes buildProductsDir = do
  timing <- readTimingEnabled
  requestIdentity <- newTimingRequestIdentity
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    sessionT0 <- monotonicTime
    setupResources <- beginResourceTiming timing
    dflags <- getSessionDynFlags
    -- Force x86_64-linux target platform regardless of host architecture.
    -- The Cranelift runtime has a single backend; prepared STG must use
    -- x86_64 primops on all hosts (including ARM/macOS).
    -- Use genericPlatform verbatim — mixing in host platform_constants causes
    -- GHC's specializer to produce Core with mismatched constructor tags on
    -- aarch64, leading to case-exhaustion SIGILL in the JIT.
    -- Platform spoofing happens HERE ONLY (session setup, before 'load'):
    -- GHC populates platform constants during session/unit initialization,
    -- so re-pinning bare genericPlatform later strips them
    -- ("Platform constants not available!" panic). Backend/opt pinning lives
    -- in canonicalizeDFlags and is also re-applied per-module below.
    -- Expose the (otherwise hidden) `ghc` package to the session so lib
    -- modules on the --include path can import the GHC API. The [fmt|]
    -- quasi-quoter's hole parser is the vendored Tidepool.QQ.HsMeta.*, which
    -- runs GHC's own expression parser inside the splice; those modules import
    -- GHC.Parser.* / GHC.Types.* etc. Without this, compiling Tidepool.QQ
    -- fails with "member of the hidden package ghc-9.12.2".
    paths <- liftIO (compileSearchPaths variant includes (importPaths dflags))
    let extracted = (extractionDynFlags dflags []) { importPaths = paths }
        dflags' = configureBuildProducts extracted buildProductsDir extracted
    setSessionDynFlags dflags'
    -- Withhold unfoldings for retained-generation symbols BEFORE 'load''
    -- runs: load and deferred finalization share the canonical simplifier,
    -- so the plugin must already be registered on the session's 'HscEnv'.
    -- See 'Tidepool.RetainedUnfoldings' for why a Core plugin is the seam
    -- that reaches both 'load'' and 'core2core' uniformly. The plugin reads
    -- immutable retained context owns each pass, so an empty retained set
    -- costs nothing on the compiled bytes (see
    -- 'withholdRetainedUnfoldings').
    let context = retainedContext retained
    hscForRetained <- getSession
    setSession (installRetainedUnfoldingsPlugin context hscForRetained)
    -- One cycle, no cache, no memo — 'sessionT0' is captured BEFORE this
    -- DynFlags bootstrap (above) so the default-on per-compile summary's
    -- wall-clock figure covers it too, exactly as it always has. See
    -- 'runCompileCycle''s haddock for what each argument controls.
    runCompileCycle selection StandaloneCycle context Nothing timing requestIdentity sessionT0 setupResources variant path

-- | Like 'runPipelineSelected'/'runPipelineSessionSelected', but also taking a
-- retained-generation set (see 'Tidepool.RetainedUnfoldings') to withhold
-- from GHC's own simplifier before it runs. This is the production one-shot
-- entry point 'app/Main.hs' calls for every request; a caller with no
-- retained-generation set to thread passes 'Set.empty', which
-- 'installRetainedUnfoldingsPlugin' makes a true no-op.
runPipelineSessionSelected
  :: PipelineSelection result -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope
  -> FilePath -> [FilePath] -> Maybe FilePath -> IO result
runPipelineSessionSelected selection retained purpose mscope path includes buildProductsDir = do
  producer <- captureCompilerProducerIdentity
  runPipelineSessionSelectedWithProducer producer selection retained purpose mscope path includes buildProductsDir

runPipelineSessionSelectedWithProducer
  :: Maybe CompilerProducerIdentity -> PipelineSelection result -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope
  -> FilePath -> [FilePath] -> Maybe FilePath -> IO result
runPipelineSessionSelectedWithProducer producer selection retained purpose mscope path includes buildProductsDir = do
  variant <- case mscope of
    Just scope | isSessionScopeActive scope -> sessionVariant purpose scope path
    _ -> normalVariant purpose path
  runCompile selection retained variant {pvCompilerProducer = producer} path includes buildProductsDir

-- ---------------------------------------------------------------------------
-- Resident compilation state
-- ---------------------------------------------------------------------------

-- | One dependency product in its owning compiler transaction. Reuse checks
-- the complete module owner as well as its source and dependency witnesses.
data MemoValidity = MemoValidity
  { memoSourceHash :: Fingerprint
  , memoRetained :: Set.Set SymbolIdentity
    -- A source hash does not change when one of its imports switches between
    -- a home module and a package module. Preserve the home-resolution shape
    -- that produced the body so removing a shadow cannot reuse stale Core.
  , memoHomeDependencies :: Map.Map HomeDependency HomeDependencyDigest
    -- | The session incarnation this entry was produced under (the Rust
    -- @SessionId@, decimal text), for a @Tidepool.Session.*@ module only --
    -- 'Nothing' for every ordinary library entry. A @Tidepool.Session.Val@/
    -- @Lib@ module name is per-incarnation-local (its generation counter
    -- resets to 0 on a fresh incarnation at the same session root, or names
    -- a wholly different session on the same warm daemon), so identical
    -- source bytes do not imply identical content: two incarnations' same-
    -- named hand-written carrier stub can be byte-for-byte equal while a
    -- real bind's same-named module differs in what it imports or the
    -- '.hi' it was compiled from. 'lookupValidMemo' requires this field to
    -- match the CURRENT request's incarnation, in addition to the source
    -- hash and home-dependency checks above, before reusing a session
    -- module's entry within its owning transaction.
  , memoIncarnation :: Maybe String
    -- Both positive and negative instance/family/orphan availability depend on
    -- the module's fixed imported lexical closure, including unused imports.
    -- GHC's actual interface usages additionally seal indirect type dependencies.
  , memoExactImportEnvironment :: Map.Map (String,String) HomeDependencyDigest
  , memoExactUsages :: Map.Map (String,String) HomeDependencyDigest
  } deriving (Eq, Ord)

-- The immutable ingress narrows version lookup before checking the saved
-- GHC usage set. Unrelated historical versions never participate in selection.
data MemoSelectionKey = MemoSelectionKey Fingerprint (Set.Set SymbolIdentity)
  (Map.Map HomeDependency HomeDependencyDigest) (Maybe String)
  (Map.Map (String,String) HomeDependencyDigest)
  deriving (Eq, Ord)

data MemoSelectionRefusal
  = MemoSelectionOwner | MemoSelectionUnsealedClosure | MemoSelectionUnsealedUsage
  | MemoSelectionSourceHash | MemoSelectionRetained | MemoSelectionHomeDependencies
  | MemoSelectionExactEnvironmentAndUsages | MemoSelectionIncarnation
  deriving (Eq, Show)

memoSelectionRefusalName :: MemoSelectionRefusal -> String
memoSelectionRefusalName reason = case reason of
  MemoSelectionOwner -> "owner"
  MemoSelectionUnsealedClosure -> "unsealed_closure"
  MemoSelectionUnsealedUsage -> "unsealed_usage"
  MemoSelectionSourceHash -> "source_hash"
  MemoSelectionRetained -> "retained"
  MemoSelectionHomeDependencies -> "home_dependencies"
  MemoSelectionExactEnvironmentAndUsages -> "exact_environment_and_usages"
  MemoSelectionIncarnation -> "incarnation"

memoSelectionKey :: MemoValidity -> MemoSelectionKey
memoSelectionKey validity = MemoSelectionKey (memoSourceHash validity)
  (memoRetained validity) (memoHomeDependencies validity) (memoIncarnation validity)
  (memoExactImportEnvironment validity)

data HomeSourceKind = OrdinaryHomeSource | BootHomeSource
  deriving (Eq, Ord, Show)

data HomeDependency = HomeDependency ModuleName HomeSourceKind
  deriving (Eq, Ord)

data HomeDependencyWitness = HomeDependencyWitness (Maybe FilePath) Fingerprint
  deriving (Eq, Show)

newtype HomeDependencyDigest = HomeDependencyDigest BS.ByteString
  deriving (Eq, Ord)

-- Hash each SCC in the typed home graph once. Entries retain only the digest
-- for each direct import, so chains use linear table work and linear aggregate
-- memo storage instead of copying a transitive witness map into every module.
homeDependencyDigests
  :: Map.Map HomeDependency (HomeDependencyWitness, [HomeDependency])
  -> Map.Map HomeDependency BS.ByteString
  -> (Map.Map HomeDependency HomeDependencyDigest, Int)
homeDependencyDigests graph policies = dependencyIdentityDigests ownFrame (Map.map snd graph)
  where
    -- Paths are freshly observed, but equal source bytes in another checkout
    -- retain the same compiled identity. Owner/kind and content remain sealed.
    ownFrame dependency = BS8.pack
      (moduleNameString name ++ "\0" ++ show kind ++ "\0" ++ show fingerprint ++ "\0")
        <> Map.findWithDefault BS.empty dependency policies
      where
        HomeDependency name kind = dependency
        HomeDependencyWitness _selectedPath fingerprint = fst (graph Map.! dependency)

reverseDependencyClosure :: Ord owner => Map.Map owner (Set.Set owner) -> Set.Set owner -> Set.Set owner
reverseDependencyClosure graph changed = visit Set.empty (Set.toList changed)
  where
    dependents = Map.fromListWith Set.union
      [(dependency,Set.singleton owner) | (owner,dependencies) <- Map.toList graph
        , dependency <- Set.toList dependencies]
    visit seen [] = seen
    visit seen (owner:rest)
      | owner `Set.member` seen = visit seen rest
      | otherwise = visit (Set.insert owner seen)
          (Set.toList (Map.findWithDefault Set.empty owner dependents) ++ rest)

sourceImportOwners :: ModSummary -> Set.Set Module
sourceImportOwners summary = Set.fromList (mapMaybe selected
  (ms_textual_imps summary ++ ms_srcimps summary))
  where
    selected (qualifier,locatedName) = case qualifier of
      NoPkgQual -> Just (mkModule (moduleUnit (ms_mod summary)) (unLoc locatedName))
      ThisPkg unit -> Just (mkModule (stringToUnit (unitIdString unit)) (unLoc locatedName))
      OtherPkg _ -> Nothing

sourceDependencyGraph :: ModuleGraph -> Map.Map Module (Set.Set Module)
sourceDependencyGraph graph = Map.fromListWith Set.union
  [(ms_mod summary,sourceImportOwners summary) | ModuleNode _ summary <- mgModSummaries' graph]

-- Exact originals retain their authenticated inputs. Legacy value injection
-- refuses only its importing closure, including recorded indirect home uses.
unsealedSourceInputs :: PipelineVariant -> HscEnv -> Set.Set Module
unsealedSourceInputs variant env = Set.fromList
  [mkModule home name | name <- pvDownsweepExcludes variant
    , mkModule home name `Set.notMember` sealed]
  where
    home = homeUnitAsUnit (hsc_home_unit env)
    sealed = Set.fromList
      [mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
        | scope <- maybe [] pure (pvExactScope variant)
        , artifact <- map (\(value,_,_) -> value) (scopeInterfaces scope)
          ++ [value | scopePurpose scope /= NoCheckedPurpose, value <- scopeValueInterfaces scope]]

usesUnsealedSourceInputs :: Set.Set Module -> HscEnv -> GutsMemoEntry -> Bool
usesUnsealedSourceInputs inputs _ _ | Set.null inputs = False
usesUnsealedSourceInputs inputs env entry = any (`Set.member` keys)
  (homeInterfaceUsageOwners env (hm_iface (finalizedHomeModInfo
    (payloadFinalized (gmePayload entry)))))
  where
    keys = Set.map (\owner -> (unitString (moduleUnit owner),moduleNameString (moduleName owner))) inputs

-- Hash strongly connected dependency identities once. Each node retains a
-- fixed-size digest rather than a copy of its complete transitive closure.
dependencyIdentityDigests
  :: Ord key => (key -> BS.ByteString) -> Map.Map key [key]
  -> (Map.Map key HomeDependencyDigest, Int)
dependencyIdentityDigests own graph =
  (Map.map (sccDigests Map.!) nodeSccs, length components)
  where
    components = zip [0 :: Int ..] $ map members $ Graph.stronglyConnComp
      [(node,node,children) | (node,children) <- Map.toAscList graph]
    members (Graph.AcyclicSCC node) = [node]
    members (Graph.CyclicSCC nodes) = sort nodes
    nodeSccs = Map.fromList
      [(node,sccId) | (sccId,nodes) <- components, node <- nodes]
    outgoing sccId nodes = Set.toAscList $ Set.fromList
      [childScc | node <- nodes, child <- Map.findWithDefault [] node graph
        , Just childScc <- [Map.lookup child nodeSccs], childScc /= sccId]
    componentMap = Map.fromList components
    compute memo sccId = case Map.lookup sccId memo of
      Just digest -> (digest,memo)
      Nothing ->
        let nodes = componentMap Map.! sccId
            (children,memo') = foldl'
              (\(digests,known) child -> let (digest,known') = compute known child
                in (digest:digests,known'))
              ([],memo) (outgoing sccId nodes)
            digest = HomeDependencyDigest $ SHA256.hash $ BS.concat
              (map (frame . own) nodes ++ map (frame . digestBytes) (sort children))
        in (digest,Map.insert sccId digest memo')
    (_,sccDigests) = foldl'
      (\(_,memo) (sccId,_) -> compute memo sccId)
      (HomeDependencyDigest BS.empty,Map.empty) components
    digestBytes (HomeDependencyDigest bytes) = bytes
    frame bytes = BS8.pack (show (BS.length bytes) ++ ":") <> bytes

-- A node's implementation requirements and lexical edges both affect imported
-- types and instance/family/orphan availability. Lexical changes outside its
-- reachable closure do not alter this identity.
exactModuleDigests :: Maybe ExactScope -> Map.Map (String,String) HomeDependencyDigest
exactModuleDigests Nothing = Map.empty
exactModuleDigests (Just scope) = fst (dependencyIdentityDigests own graph)
  where
    originals = Map.fromList [((exactUnit artifact,exactModule artifact),
        (artifact {exactPath=""},packages,normalizeInterfaceEvidence <$> Map.lookup
          (exactUnit artifact,exactModule artifact) (scopeInterfaceEvidence scope)))
      | (artifact,_,packages) <- scopeInterfaces scope]
    values = Map.fromList [((exactUnit artifact,exactModule artifact),
        (artifact {exactPath=""},"",Just CheckedValueEvidence))
      | artifact <- scopeValueInterfaces scope]
    rows = Map.union originals values
    lexical = Map.fromList (scopeLexical scope)
    graph = Map.mapWithKey (\owner (artifact,_,_) -> nubOrd
      (exactRequirements artifact ++ Map.findWithDefault [] owner lexical)) rows
    own owner = BS8.pack (show (owner,rows Map.! owner,Map.lookup owner lexical))

-- | Diagnostic only: hex-render a digest for a 'tidepool-memo-cycle-graph'
-- line. Not used by any validity comparison, which compares the raw
-- 'HomeDependencyDigest' bytes directly.
hexBytes :: BS.ByteString -> String
hexBytes = concatMap hexByte . BS.unpack
  where hexByte byte = let rendered = showHex byte "" in replicate (2 - length rendered) '0' ++ rendered

-- CPP, splices, and quasiquoters can consume inputs outside the downsweep
-- source graph. TemplateHaskellQuotes alone only constructs syntax and does
-- not execute a compiler-time provider, so it remains memoizable.
hasUntrackedCompileTimeExecution :: DynFlags -> Bool
hasUntrackedCompileTimeExecution flags =
  hasUnconditionallyUntrackedCompileTimeExecution flags || xopt LangExt.QuasiQuotes flags

-- | Diagnostic only: which option in 'hasUntrackedCompileTimeExecution'
-- fired, for an exact no-reuse reason rather than a boolean. The first match
-- in the same order that function checks; a module can enable more than one
-- of these, in which case the trace names the first.
untrackedExtensionName :: DynFlags -> Maybe String
untrackedExtensionName flags
  | issue : _ <- pluginInputIssues flags = Just issue
  | gopt Opt_Pp flags = Just "external-preprocessor"
  | otherwise = case filter (`xopt` flags)
      [LangExt.Cpp, LangExt.TemplateHaskell, LangExt.QuasiQuotes] of
    (ext : _) -> Just (show ext)
    []        -> Nothing

-- External preprocessing, CPP, and TemplateHaskell splices can run arbitrary compile-time code with
-- no static bound on what they read (files, environment, 'Name'-based
-- 'reify' against other modules) or produce, so a module enabling either
-- always misses the memo. Actual quasiquote occurrences are handled separately
-- by the parser fact below, including when the syntax extension is enabled by default.
hasUnconditionallyUntrackedCompileTimeExecution :: DynFlags -> Bool
hasUnconditionallyUntrackedCompileTimeExecution flags =
  not (null (pluginInputIssues flags))
    || gopt Opt_Pp flags || any (`xopt` flags) [LangExt.Cpp, LangExt.TemplateHaskell]

-- | Parser evidence for whether fresh source executes a quasiquoter. Module
-- names and exported occurrences do not authenticate an implementation or its
-- compile-time inputs. Even a shipped quoter remains untracked here.
-- An unchanged source hash lets memo lookup retain the no-occurrence proof.
data QuasiQuoteUse = NoQuasiQuotes | HasQuasiQuotes
  deriving (Eq, Show)

classifyQuasiQuoteUse :: ParsedModule -> QuasiQuoteUse
classifyQuasiQuoteUse parsed
  | null occurrences = NoQuasiQuotes
  | otherwise = HasQuasiQuotes
  where
    occurrences = quasiQuoteOccurrences (ms_hspp_opts (pm_mod_summary parsed)) (pm_parsed_source parsed)

-- GHC enables dependency codegen from the QuasiQuotes extension alone. The
-- harness enables that syntax for every input, including inputs without a
-- quote. In a graph without other compile-time inputs, the parser can prove
-- that none of those dependencies will execute. Keep the syntax and load
-- profile; only suppress the executable backend that downsweep enabled.
-- Both this proof and load parse the immutable downsweep buffers. Missing
-- buffers, preprocessing, splices and plugins retain GHC's ordinary plan.
elideUnusedQuasiQuoteCodegen :: Bool -> ModuleGraph -> Ghc ModuleGraph
elideUnusedQuasiQuoteCodegen timing graph
  | null quoteSummaries || null executable || any unsupported summaries = pure graph
  | any (backendWritesFiles . backend . ms_hspp_opts) executable = pure graph
  | any (isNothing . ms_hspp_buf) quoteSummaries = pure graph
  | otherwise = do
      parsed <- mapM parseModule quoteSummaries
      if any (not . null . occurrences) parsed
        then pure graph
        else do
          let narrowed = mapMG (\summary -> summary
                { ms_hspp_opts = (ms_hspp_opts summary) { backend = noBackend } }) graph
          current <- getSession
          setSession current { hsc_mod_graph = narrowed }
          liftIO (emitCount timing "quasiquote_codegen_elided_modules"
            (toInteger (length executable)))
          pure narrowed
  where
    summaries = [summary | ModuleNode _ summary <- mgModSummaries' graph]
    executable = filter (backendGeneratesCode . backend . ms_hspp_opts) summaries
    quoteSummaries = filter (xopt LangExt.QuasiQuotes . ms_hspp_opts) summaries
    unsupported summary =
      hasUnconditionallyUntrackedCompileTimeExecution (ms_hspp_opts summary)
        || xopt LangExt.StaticPointers (ms_hspp_opts summary)
    occurrences parsed = quasiQuoteOccurrences (ms_hspp_opts (pm_mod_summary parsed)) (pm_parsed_source parsed)

-- Diagnostic only; the typed parser fact owns reuse eligibility.
renderQuasiQuoteUse :: QuasiQuoteUse -> String
renderQuasiQuoteUse NoQuasiQuotes = "none"
renderQuasiQuoteUse HasQuasiQuotes = "untracked"

-- Facts needed even when a module contributes no executable body. Keeping
-- these separately lets an unchanged re-export or validation-only module
-- prove its dependents valid without retaining its compiler session graph.
data ModuleFacts = ModuleFacts
  { moduleFactTyCons :: [TyCon]
  , moduleFactReferences :: Set.Set Module
    -- ^ Keep defining units until the current home compiler view selects
    -- its relation. A package module with the same name is another owner.
  , moduleFactPackageImports :: PackageImportEvidence
  , moduleFactHasDependentFiles :: Bool
  , moduleFactQuasiQuoteUse :: !QuasiQuoteUse
    -- ^ Diagnostic and gate input: see 'QuasiQuoteUse'. 'lookupValidMemo'
    -- consults a *previous* entry's copy of this field (never today's fresh
    -- parse) to decide whether a 'QuasiQuotes'-only module can still be
    -- treated as memoizable.
  }

data ModuleOutput = ModuleOutput
  { moduleOutputModule :: Module
  , moduleOutputBinds :: [CoreBind]
  , moduleOutputCapturedType :: Maybe String
  , moduleOutputCheckedBinderPins :: [CheckedBinderPin]
  , moduleOutputResultType :: Maybe Type
  }

-- | Prepared STG and its canonical interface/Core owner remain paired.
data ModuleProduct = ModuleProduct
  { productFacts :: ModuleFacts
  , productOutput :: ModuleOutput
  , productPrepared :: PreparedModule
  , productFinalized :: FinalizedModule
  }

data MemoPayload
  = ValidationOnly ModuleFacts ModuleOutput FinalizedModule
  | ExecutableProduct ModuleProduct

data GutsMemoEntry = GutsMemoEntry
  { gmeValidity :: MemoValidity
    -- The retained set is part of validity because it changes simplified
    -- Core before the compact output below is derived.
  , gmePayload :: MemoPayload
  , gmeCycle :: Word64
    -- ^ Diagnostic only (TIDEPOOL_MEMO_TRACE): the compile-cycle id
    -- ('requestIdentity') that produced this entry. Never read by a
    -- validity check.
  , gmeDirectWitnesses :: !(Map.Map HomeDependency HomeDependencyWitness)
    -- ^ Diagnostic only (TIDEPOOL_MEMO_TRACE): this module's direct
    -- dependency witnesses (selected path, content fingerprint) at the
    -- cycle that produced this entry. Retained so a later miss can report
    -- exactly which witness changed, and whether the change was in path,
    -- fingerprint, or both. Validity itself continues to compare
    -- 'memoHomeDependencies' (an opaque digest), never this map. Empty when
    -- tracing was disabled for the cycle that stored the entry.
  }

data ModuleObservation
  = CachedObservation ModSummary GutsMemoEntry
  | LoadedObservation LoadedModule
  | HydratedObservation ModSummary HomeModInfo AdmittedSourceCandidate

observationSummary :: ModuleObservation -> ModSummary
observationSummary (CachedObservation summary _) = summary
observationSummary (LoadedObservation loaded) = loadedSummary loaded
observationSummary (HydratedObservation summary _ _) = summary

observationFacts :: ModuleObservation -> IO ModuleFacts
observationFacts (CachedObservation _ entry) = pure (payloadFacts (gmePayload entry))
observationFacts (LoadedObservation loaded) = pure (loadedFacts loaded)
observationFacts (HydratedObservation _ hmi admitted) = pure
  (hydratedModuleFacts hmi (admittedCandidateRoots admitted))
    { moduleFactReferences = Set.fromList
        [ mkModule (stringToUnit (Text.unpack (symbolUnit identity)))
            (mkModuleName (Text.unpack (symbolModule identity)))
        | group <- candidateGroups (admittedCandidateOriginal admitted)
        , global <- candidateGroupGlobals group
        , let (identity, required) = candidateOriginalGlobalDemand global
        , required ] }

-- Checked HPT metadata alone has no admitted native outline. Executable
-- observations add their already authenticated candidate demand above.
hydratedModuleFacts :: HomeModInfo -> PackageImportEvidence -> ModuleFacts
hydratedModuleFacts hmi roots = ModuleFacts
  { moduleFactTyCons = typeEnvTyCons (md_types (hm_details hmi))
  , moduleFactReferences = Set.empty
  , moduleFactPackageImports = roots
  , moduleFactHasDependentFiles = False
  , moduleFactQuasiQuoteUse = NoQuasiQuotes
  }

-- Original native groups can construct values from implementation interfaces
-- outside the lexical graph. Their verified HPT details still belong in the
-- runtime constructor metadata; adding those facts does not add lexical edges.
exactInterfaceTyCons :: HscEnv -> Maybe ExactScope -> IO [TyCon]
exactInterfaceTyCons _ Nothing = pure []
exactInterfaceTyCons env (Just scope) = concat <$> forM originals
  (\(artifact, _, _) -> case lookupHpt (hsc_HPT env) (mkModuleName (exactModule artifact)) of
    Just hmi
      | unitString (moduleUnit (mi_module (hm_iface hmi))) == exactUnit artifact
      , moduleNameString (moduleName (mi_module (hm_iface hmi))) == exactModule artifact ->
          pure (typeEnvTyCons (md_types (hm_details hmi)))
    _ -> ioError (userError "exact constructor metadata owner is absent from the hydrated HPT"))
  where
    paired = Set.fromList
      [ (executionUnit owner, executionModule owner, executionIfaceSha256 owner)
      | owner <- scopeExecutionNativeOwners scope ]
    originals =
      [ entry | entry@(artifact, _, _) <- scopeInterfaces scope
      , (exactUnit artifact, exactModule artifact, exactSha256 artifact) `Set.member` paired ]

payloadFacts :: MemoPayload -> ModuleFacts
payloadFacts (ValidationOnly facts _ _) = facts
payloadFacts (ExecutableProduct moduleProduct) = productFacts moduleProduct

payloadProduct :: MemoPayload -> Maybe ModuleProduct
payloadProduct (ValidationOnly _ _ _) = Nothing
payloadProduct (ExecutableProduct moduleProduct) = Just moduleProduct

payloadLoaded :: ModSummary -> MemoPayload -> LoadedModule
payloadLoaded summary (ValidationOnly facts output finalized) =
  LoadedModule summary facts output finalized
payloadLoaded summary (ExecutableProduct product') =
  LoadedModule summary (productFacts product') (productOutput product') (productFinalized product')

payloadFinalized :: MemoPayload -> FinalizedModule
payloadFinalized (ValidationOnly _ _ finalized) = finalized
payloadFinalized (ExecutableProduct product') = productFinalized product'

payloadOwner :: MemoPayload -> Module
payloadOwner (ValidationOnly _ output _) = moduleOutputModule output
payloadOwner (ExecutableProduct product') = moduleOutputModule (productOutput product')

requireProduct :: ModuleFacts -> ModuleOutput -> Maybe PreparedModule
  -> FinalizedModule -> Ghc ModuleProduct
requireProduct facts output (Just prepared) finalized = pure ModuleProduct
  { productFacts = facts
  , productOutput = output
  , productPrepared = prepared
  , productFinalized = finalized
  }
requireProduct _ _ Nothing _ = liftIO $ ioError $ userError
    "executable compilation produced no prepared module"

-- Resolved direct imports include unused, instance-only and compiler-inserted
-- package imports; interface dependencies alone do not retain that boundary.
directPackageImports :: HscEnv -> TcGblEnv -> IO PackageImportEvidence
directPackageImports env tcg = do
  let imported = Map.keys (imp_mods (tcg_imports tcg))
      selected = [owner | owner <- imported
        , not (isHomeUnit (hsc_home_unit env) (moduleUnit owner))
        , owner /= gHC_PRIM]
  roots <- forM selected $ \owner -> do
    witness <- packageImportRoot env owner
    either (ioError . userError . ("direct package import unavailable: " ++)) pure witness
  pure (PackageImportEvidence (Set.toAscList (Set.fromList roots))
    [CompilerPrimitive | gHC_PRIM `elem` imported])

frontFacts :: ModuleFront -> IO ModuleFacts
frontFacts front = pure ModuleFacts
    { moduleFactTyCons = mg_tcs (mfDesugared front)
    , moduleFactReferences = mfReferencedModules front
    , moduleFactPackageImports = mfPackageImports front
    , moduleFactHasDependentFiles = mfHasDependentFiles front
    , moduleFactQuasiQuoteUse = mfQuasiQuoteUse front
    }

type GutsMemo = Map.Map ModuleName GutsMemoEntry

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

-- | One compile cycle in an already-open 'Ghc' session. Loaded sources
-- finalize in the typed phase hooks; deferred sources finalize in dependency
-- order before their importers. The caller owns session bootstrap and decides
-- whether the immutable module memo survives this cycle.
--
-- 'CycleState' carries immutable completed versions from its universe owner.
-- Each attempt has a private EPS/finder/home view and selects only versions
-- validated against the current source and exact imported environment.
-- 'sessionT0' includes bootstrap only for standalone compilation.
--
-- 'retained' is the immutable per-request index used by both the plugin's
-- recompilation fingerprint and the prepared memo's module validity check.

runCompileCycle
  :: PipelineSelection result -> CycleState
  -> RetainedContext -> Maybe String -> Bool -> Word64 -> Double -> Maybe ResourceTimingStart -> PipelineVariant -> FilePath -> Ghc result
runCompileCycle selection cycleState retained incarnation timing requestIdentity sessionT0 setupResources variant path = withCycleHooks $ withCompilerViewDirectory $ \compilerViewDirectory -> do
    packageFinder <- case cycleState of
      StandaloneCycle -> getSession >>= liftIO . newPackageFinderFacts
      TransactionCycle _ _ _ _ _ _ facts _ -> pure facts
    forM_ (pvExactScope variant) $ \scope ->
      case compilerProducerFor variant of
        Nothing -> liftIO (throwIO CompilerProducerUnavailable)
        Just producer -> unless (producer == scopeProducerSha256 scope)
          (liftIO (throwIO CompilerProducerScopeMismatch))
    unless (typedPlanFor selection == typedPurposePlan (pvPurpose variant)) $
      liftIO (throwIO SegmentPlanPurposeMismatch)
    when (isJust (typedPlanFor selection) && isNothing (typedPreparationFor selection)) $
      liftIO (throwIO MissingSegmentPreparation)
    memoTrace <- liftIO readMemoTraceEnabled
    sourceReuseDisabled <- liftIO ((== Just "1") <$> lookupEnv "TIDEPOOL_DISABLE_SOURCE_REUSE")
    disabledSourceOwnersRef <- liftIO (newIORef Set.empty)
    bodyReuseDisabled <- liftIO ((== Just "1") <$> lookupEnv "TIDEPOOL_DISABLE_BODY_REUSE")
    disabledPreparedOwnersRef <- liftIO (newIORef Set.empty)
    interpreterAttempt <- case cycleState of
      StandaloneCycle -> liftIO (newIORef InterpreterConfirmed)
      TransactionCycle _ _ _ _ _ _ _ attempt -> pure attempt
    let markInterpreterMutation = writeIORef interpreterAttempt InterpreterMutationPending
    let reuseContext = ReuseContext requestIdentity (compilePurposeLabel (pvPurpose variant))
        reuseOwner summary = Just (ReuseModule (unitString (moduleUnit (ms_mod summary)))
          (moduleNameString (ms_mod_name summary)) SourceFingerprint (show (ms_hs_hash summary)))
        reuseEvent stage decision reason summary = do
          disabled <- readIORef disabledSourceOwnersRef
          let actualReason
                | decision == ReuseWork
                , stage `elem` [SourceFrontend,FinalizedCore,Interface]
                , ms_mod summary `Set.member` disabled = CacheDisabled
                | otherwise = reason
          emitReuse timing reuseContext stage decision actualReason (reuseOwner summary) 1 Nothing
    let executionGrant = executionGrantFor selection
        preparation = selectionKind selection
        captureProducts = capturesProductInterfaces selection
        candidateManifest = candidateManifestFor selection
        exactCycle = exactCompileCycle selection variant
        mCache = case cycleState of
          StandaloneCycle -> Nothing
          TransactionCycle cache _ _ _ _ _ _ _ -> Just cache
        mMemoRef = case cycleState of
          StandaloneCycle -> Nothing
          TransactionCycle _ memo _ _ _ _ _ _ -> Just memo
        capturedCandidates = case cycleState of
          StandaloneCycle -> Nothing
          TransactionCycle _ _ captured _ _ _ _ _ -> captured
    when (exactCycle && case cycleState of StandaloneCycle -> True; _ -> False) $ do
      current <- getSession
      fresh <- liftIO (freshExactState current)
      setSession fresh
    originalMetaHook <- runMetaHook . hsc_hooks <$> getSession
    -- Observe the actual splice evaluator, including audited quasiquoters,
    -- without adding an authored effect that would change cacheability.
    -- withCycleHooks restores the delegate on every exit from this request.
    when timing $ do
      current <- getSession
      let delegate = fromMaybe defaultRunMeta originalMetaHook
          observe request expression = do
            owner <- tcg_mod <$> getGblEnv
            let unit = unitString (moduleUnit owner)
                name = moduleNameString (moduleName owner)
                ownerSha = hexBytes (SHA256.hash (TextEncoding.encodeUtf8 (Text.pack (show (unit,name)))))
            liftIO $ hPutStrLn stderr ("tidepool-meta-execution request=" ++ show requestIdentity
              ++ " owner_sha256=" ++ ownerSha
              ++ " unit=" ++ show (take 128 unit) ++ " module=" ++ show (take 128 name)
              ++ " owner_truncated=" ++ show (length unit > 128 || length name > 128))
            delegate request expression
      setSession current {hsc_hooks=(hsc_hooks current) {runMetaHook=Just observe}}
    forM_ (pvExactScope variant) $ \scope -> do
      env <- getSession
      verified <- liftIO (revalidateExactScope env scope)
      either (liftIO . ioError . userError) pure verified
    capturedTarget <- forM (pvGeneratedScaffold variant) $ \recipe ->
      liftIO (captureGeneratedScaffoldTarget recipe path) >>= either (liftIO . fail) pure
    target <- guessTarget path Nothing Nothing
    capturedTargetInput <- case capturedTarget of
      Nothing -> pure target
      Just bytes -> do
        now <- liftIO (getModificationTime path)
        pure target {targetContents=Just
          (stringToStringBuffer (Text.unpack (TextEncoding.decodeUtf8 bytes)),now)}
    setTargets [capturedTargetInput]
    -- Install diagnostic capture before load/typecheck. Target warnings become
    -- part of a successful result; all source errors remain available if 'load'' reports
    -- only a 'Failed' flag rather than throwing a 'SourceError'.
    warnRef <- liftIO (newIORef [])
    errorRef <- liftIO (newIORef [])
    preparedSiblingsRef <- liftIO (newIORef Map.empty)
    productInterfacesRef <- liftIO (newIORef Map.empty)
    finalizedModulesRef <- liftIO (newIORef Map.empty)
    loadedModulesRef <- liftIO (newIORef Map.empty)
    targetEnvironmentRef <- liftIO (newIORef Nothing)
    targetTypedSegmentRef <- liftIO (newIORef Nothing)
    typedSessionInterfacesRef <- liftIO (newIORef [])
    typedSessionEnvironmentRef <- liftIO (newIORef Nothing)
    typedFailureRef <- liftIO (newIORef Nothing)
    typedPreparationFailureRef <- liftIO (newIORef Nothing)
    pushLogHookM (diagnosticCollectorHook path warnRef errorRef)
    -- Downsweep may select the interpreter for splice dependencies and enable
    -- IgnoreInterfacePragmas. Canonical summaries restore the interface policy
    -- before any shared package interface is demanded. Package unfoldings and
    -- home type identities remain available without an EPS flush.
    previous <- getSession
    modGraphDownsweep <- depanalSourceModules (pvDownsweepExcludes variant)
    forM_ (pvExactScope variant) $ \scope -> do
      let exactNames = Set.fromList [mkModuleName (exactModule iface)
            | (iface, _, _) <- scopeInterfaces scope]
      when (any (\node -> case node of
          ModuleNode _ summary -> ms_mod_name summary `Set.member` exactNames
          _ -> False) (mgModSummaries' modGraphDownsweep)) $
        liftIO $ ioError $ userError "fresh source collides with an admitted exact owner"
    modGraphRaw <- elideUnusedQuasiQuoteCodegen timing modGraphDownsweep
    targetName <- liftIO (targetModuleNameFor path)
    let targetOwner = mkModule (homeUnitAsUnit (hsc_home_unit previous)) targetName
        prepareTypedTarget summary env tcg = case typedPlanFor selection of
          Just typedPlan | ms_mod_name summary == targetName -> do
            prepare <- maybe (throwIO MissingSegmentPreparation) pure (typedPreparationFor selection)
            segment <- captureTypedSegment typedPlan env
              (maybe Map.empty scopeCanonicalInterfaces (pvExactScope variant)) tcg
            -- GHC Make can turn a hook exception into a failed load and a
            -- diagnostic. This request owns the preparation callback's exact
            -- refusal or cancellation, independently of source diagnostics.
            (hydrated, globals, interfaces) <- (prepare env
              (maybe Map.empty scopeCanonicalInterfaces (pvExactScope variant)) segment)
              `catch` \(failure :: SomeException) -> do
                writeIORef typedPreparationFailureRef (Just failure)
                throwIO failure
            closed <- closeTypedSegment hydrated globals segment
            writeIORef typedSessionInterfacesRef interfaces
            writeIORef typedSessionEnvironmentRef (Just hydrated)
            modifyIORef' (tcg_keep tcg) (`extendNameSetList`
              (map (idName . typedItemRoot) (pendingSegmentItems closed)
                ++ map idName (pendingSegmentSupportRoots closed)))
            pure (hydrated, Just closed)
          _ -> pure (env, Nothing)
        installSegment _ Nothing guts = pure guts
        installSegment env (Just pending) guts = do
          (segment, installed) <- installTypedSegmentRoots env pending guts
          writeIORef targetTypedSegmentRef (Just segment)
          pure installed
        includeTypedSession final = do
          hydrated <- readIORef typedSessionEnvironmentRef
          interfaces <- readIORef typedSessionInterfacesRef
          pure $ case hydrated of
            Nothing -> final
            Just env -> hscUpdateHPT (\home -> foldl (\table (owner,_) ->
              maybe table (\entry -> addToHpt table (moduleName owner) entry)
                (lookupHpt (hsc_HPT env) (moduleName owner))) home
              (map capturedSessionInterface interfaces)) final

    -- Module names do not identify generated content across independent
    -- requests. A memo hit therefore requires the current source hash and
    -- the selected path/fingerprint closure of every home import. The
    -- closure matters for SOURCE imports because boot summaries are not in
    -- the executable memo walk: an import inside a .hs-boot must still
    -- invalidate its ordinary importer. Ordinary dependencies additionally
    -- propagate compile validity in summary order below.
    let exactIdentities = exactModuleDigests (pvExactScope variant)
        unsealedInputs = unsealedSourceInputs variant previous
        unsealedClosure
          | Set.null unsealedInputs = Set.empty
          | otherwise = reverseDependencyClosure (sourceDependencyGraph modGraphRaw) unsealedInputs
        homeImportKey modSum (qualifier,locatedName) = case qualifier of
          NoPkgQual -> Just (unitString (moduleUnit (ms_mod modSum)),moduleNameString (unLoc locatedName))
          ThisPkg unit -> Just (unitIdString unit,moduleNameString (unLoc locatedName))
          OtherPkg _ -> Nothing
        exactImportEnvironment modSum = Map.restrictKeys exactIdentities (Set.fromList
          (catMaybes (map (homeImportKey modSum) (ms_textual_imps modSum ++ ms_srcimps modSum))))
        incarnationFor modSum
          | isJust (parseSessionModule (moduleNameString (ms_mod_name modSum))) = incarnation
          | otherwise = Nothing
        summaryDependency summary = HomeDependency (ms_mod_name summary)
              (if ms_hsc_src summary == HsBootFile
                then BootHomeSource else OrdinaryHomeSource)
        summaryByDependency = Map.fromList
          [ (summaryDependency summary, summary)
          | ModuleNode _ summary <- mgModSummaries' modGraphRaw
          ]
        summaryFingerprints = Map.map
          (\summary -> HomeDependencyWitness
            (normalise <$> ml_hs_file (ms_location summary))
            (ms_hs_hash summary))
          summaryByDependency
        importedDependencies kind imports =
          [ dependency
          | (_, locatedName) <- imports
          , let dependency = HomeDependency (unLoc locatedName) kind
          , dependency `Map.member` summaryFingerprints
          ]
        summaryDependencies summary =
             importedDependencies OrdinaryHomeSource (ms_textual_imps summary)
          ++ importedDependencies BootHomeSource (ms_srcimps summary)
        dependencyGraph = Map.mapWithKey
          (\dependency summary ->
            (summaryFingerprints Map.! dependency, summaryDependencies summary))
          summaryByDependency
        -- Request rewrites are target-only and targets are never published.
        -- Reusable dependencies keep their authored import closure; imported
        -- exact versions include indirect type, instance and orphan owners.
        summaryPolicies = Map.map (\summary -> BS8.pack (show
          (retainedDefinedBy (ms_mod summary) retained,incarnationFor summary,
           [(owner,hexBytes bytes) | (owner,HomeDependencyDigest bytes) <-
             Map.toAscList (exactImportEnvironment summary)]))) summaryByDependency
        (dependencyDigests, digestComputations) =
          homeDependencyDigests dependencyGraph summaryPolicies
        directDependencyKeys modSum = Set.fromList
          ( importedDependencies OrdinaryHomeSource (ms_textual_imps modSum)
         ++ importedDependencies BootHomeSource (ms_srcimps modSum))
        homeDependencyWitnesses modSum = Map.restrictKeys dependencyDigests
          (directDependencyKeys modSum)
        directHomeDeps modSum = Set.fromList
          [ mn
          | (_, lmn) <- ms_textual_imps modSum
          , let mn = unLoc lmn
          , HomeDependency mn OrdinaryHomeSource `Map.member` summaryFingerprints
          ]
        -- Diagnostic only (TIDEPOOL_MEMO_TRACE): the raw per-dependency
        -- witnesses (path, fingerprint) behind 'homeDependencyWitnesses'
        -- opaque digest, retained on the memo entry so a later miss can
        -- name exactly which dependency's witness changed.
        directWitnesses modSum = Map.restrictKeys summaryFingerprints
          (directDependencyKeys modSum)
        -- The strict memo field must not retain a thunk over the graph
        -- when this cycle did not opt in to witness diagnostics.
        memoDiagnosticWitnesses modSum
          | memoTrace = directWitnesses modSum
          | otherwise = Map.empty
        dependencyEdgeCount = sum
          [ length children | (_, children) <- Map.elems dependencyGraph ]
    when timing $ liftIO $ hPutStrLn stderr $
      "tidepool-dependency-witness nodes=" ++ show (Map.size dependencyGraph)
        ++ " direct_edges=" ++ show dependencyEdgeCount
        ++ " digest_computations=" ++ show digestComputations
    validThisCycleRef <- liftIO (newIORef (Map.empty :: Map.Map ModuleName Bool))
    executableValidRef <- liftIO (newIORef (Map.empty :: Map.Map ModuleName Bool))
    dropMemoInterface <- liftIO (lookupEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE")
    -- The withholding pass can change only a module's own retained
    -- definitions; retained identities defined elsewhere reach it through
    -- a dependency, whose invalidity is already covered by 'depsValidSoFar'.
    let retainedFor modSum = retainedDefinedBy (ms_mod modSum) retained
        exactUsages finalized = Map.restrictKeys exactIdentities (Set.fromList
          (homeInterfaceUsageOwners previous (hm_iface (finalizedHomeModInfo finalized))))
        memoValidity modSum finalized = MemoValidity
          (ms_hs_hash modSum) (retainedFor modSum) (homeDependencyWitnesses modSum)
          (incarnationFor modSum) (exactImportEnvironment modSum) (exactUsages finalized)
        sameExactEnvironment modSum validity =
          memoExactImportEnvironment validity == exactImportEnvironment modSum
          && all (\(owner,digest) -> Map.lookup owner exactIdentities == Just digest)
            (Map.toList (memoExactUsages validity))
        sourceExecutable summary = CompilerExecutableContext
          (SourceExecutableVersion (MemoValidity (ms_hs_hash summary) (retainedFor summary)
            (homeDependencyWitnesses summary) (incarnationFor summary)
            (exactImportEnvironment summary) Map.empty))
          (Set.fromList
            ([mkModule (moduleUnit (ms_mod summary)) name
              | HomeDependency name _ <- Map.keys (homeDependencyWitnesses summary)]
             ++ [mkModule (stringToUnit unit) (mkModuleName name)
                | (unit,name) <- Map.keys (exactImportEnvironment summary)]))
        originalExecutables = Map.fromList
          [(owner,CompilerExecutableContext (OriginalExecutableVersion digest localIncarnation)
            (Set.fromList [mkModule (stringToUnit unit) (mkModuleName name)
              | (unit,name) <- nubOrd (exactRequirements artifact
                  ++ fromMaybe [] (lookup key (scopeLexical scope)))]))
          | scope <- maybe [] pure (pvExactScope variant)
          , artifact <- map (\(value,_,_) -> value) (scopeInterfaces scope) ++ scopeValueInterfaces scope
          , let key = (exactUnit artifact,exactModule artifact)
          , let owner = mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
          , Just digest <- [Map.lookup key exactIdentities]
          , let localIncarnation = if isJust (parseSessionModule (exactModule artifact)) then incarnation else Nothing]
        selectedExecutables = Map.union originalExecutables (Map.fromList
          [(ms_mod summary,sourceExecutable summary) | summary <- Map.elems summaryByDependency
            , ms_hsc_src summary == HsSrcFile])
    interpreterState <- case cycleState of
      StandaloneCycle -> liftIO (newIORef (CompilerInterpreterState 0 Map.empty))
      TransactionCycle _ _ _ _ _ retainedInterpreter _ _ -> pure retainedInterpreter
    selectedVersionsRef <- liftIO (newIORef Map.empty)
    executableObservationsRef <- liftIO $ if memoTrace
      then Just <$> newIORef Map.empty else pure Nothing
    -- 'null' forces only through the first refusal, preserving the selection
    -- predicate's short circuit. Rendering/full refusal enumeration is opt-in.
    let selectionRefusals :: ModSummary -> CompletedModuleVersion -> [MemoSelectionRefusal]
        selectionRefusals summary node =
          let entry = completedModuleEntry node
              validity = gmeValidity entry
          in [reason | (valid, reason) <-
               [(payloadOwner (gmePayload entry) == ms_mod summary, MemoSelectionOwner)
               ,(ms_mod summary `Set.notMember` unsealedClosure, MemoSelectionUnsealedClosure)
               ,(not (usesUnsealedSourceInputs unsealedInputs previous entry), MemoSelectionUnsealedUsage)
               ,(memoSourceHash validity == ms_hs_hash summary, MemoSelectionSourceHash)
               ,(memoRetained validity == retainedFor summary, MemoSelectionRetained)
               ,(memoHomeDependencies validity == homeDependencyWitnesses summary, MemoSelectionHomeDependencies)
               ,(sameExactEnvironment summary validity, MemoSelectionExactEnvironmentAndUsages)
               ,(not (isJust (parseSessionModule (moduleNameString (ms_mod_name summary))))
                   || (isJust incarnation && memoIncarnation validity == incarnation), MemoSelectionIncarnation)]
             , not valid]
    case cycleState of
      TransactionCycle _ memo _ versions _ _ _ _ -> liftIO $ do
        let select summary = do
              byIngress <- Map.lookup (ms_mod summary) versions
              let ingress = MemoSelectionKey (ms_hs_hash summary) (retainedFor summary)
                    (homeDependencyWitnesses summary) (incarnationFor summary)
                    (exactImportEnvironment summary)
              narrowed <- Map.lookup ingress byIngress
              find (null . selectionRefusals summary) (Map.elems narrowed)
            selected = Map.fromList [(ms_mod summary,node)
              | summary <- Map.elems summaryByDependency, ms_mod summary /= targetOwner
              , Just node <- [select summary]]
        writeIORef selectedVersionsRef selected
        writeIORef memo (Map.fromList
          [(moduleName owner,completedModuleEntry node) | (owner,node) <- Map.toList selected])
      StandaloneCycle -> pure ()
    let depsValidSoFar modSum = liftIO $ do
          validMap <- readIORef validThisCycleRef
          pure (all (\d -> Map.findWithDefault False d validMap)
            (Set.toList (directHomeDeps modSum)))
        recordValidity modSum isValid =
          liftIO (modifyIORef' validThisCycleRef (Map.insert (ms_mod_name modSum) isValid))
        recordExecutableValidity modSum isValid =
          liftIO (modifyIORef' executableValidRef (Map.insert (ms_mod_name modSum) isValid))
        cachedInterface modSum entry
          | dropMemoInterface == Just (moduleNameString (ms_mod_name modSum)) = Nothing
          | otherwise = Just (finalizedHomeModInfo
              (loadedFinalized (payloadLoaded modSum (gmePayload entry))))
        interfaceReady _ modSum entry = isJust (cachedInterface modSum entry)
    -- Under TIDEPOOL_TIMING, name why a memoized module was recompiled.
    let memoMiss modSum reason = when timing $ liftIO $ hPutStrLn stderr $
          "tidepool-memo-miss module=" ++ moduleNameString (ms_mod_name modSum)
            ++ " reason=" ++ reason
    -- Under TIDEPOOL_MEMO_TRACE, the same miss with the originating
    -- cycle and (when a prior entry exists) the exact per-dependency
    -- witness diff — added/removed/changed keys, each shown with old
    -- and new path/fingerprint separately so a path-only change is
    -- distinguishable from a real content change.
    let memoMissTrace modSum reason mEntry = liftIO $
          emitMemoMissTrace memoTrace requestIdentity
            (maybe "none" (show . gmeCycle) mEntry)
            (moduleNameString (ms_mod_name modSum))
            reason
            [ renderWitness d w | (d, w) <- Map.toList added ]
            [ renderWitness d w | (d, w) <- Map.toList removed ]
            [ renderWitnessChange d old new | (d, (old, new)) <- Map.toList changed ]
          where
            oldWitnesses = maybe Map.empty gmeDirectWitnesses mEntry
            newWitnesses = directWitnesses modSum
            added = Map.difference newWitnesses oldWitnesses
            removed = Map.difference oldWitnesses newWitnesses
            changed = Map.mapMaybe id $ Map.intersectionWith
              (\old new -> if old == new then Nothing else Just (old, new))
              oldWitnesses newWitnesses
            renderDependency (HomeDependency name kind) = moduleNameString name ++ "/" ++ show kind
            renderWitness d (HomeDependencyWitness witnessPath fp) =
              renderDependency d ++ ":path=" ++ maybe "<none>" id witnessPath ++ ",fingerprint=" ++ show fp
            renderWitnessChange d (HomeDependencyWitness op ofp) (HomeDependencyWitness np nfp) =
              renderDependency d
                ++ ":path=" ++ maybe "<none>" id op ++ "->" ++ maybe "<none>" id np
                ++ ",fingerprint=" ++ show ofp ++ "->" ++ show nfp
                ++ ",path_changed=" ++ show (op /= np)
                ++ ",fingerprint_changed=" ++ show (ofp /= nfp)
    let lookupValidMemo modSum
          | ms_mod modSum == targetOwner = pure Nothing
          | otherwise = do
              loaded <- liftIO (readIORef loadedModulesRef)
              if ms_mod modSum `Map.member` loaded
                then pure Nothing
                else case mMemoRef of
                  Nothing -> pure Nothing
                  Just ref -> lookupMemo ref modSum
        lookupMemo ref modSum = do
            depsOk <- depsValidSoFar modSum
            -- Cpp/TemplateHaskell gate here, before any entry lookup:
            -- unconditional. A 'QuasiQuotes'-only module may be memoizable
            -- when its previous parse proved that it contains no quotations.
            if ms_mod modSum `Set.member` unsealedClosure
              then memoMiss modSum "unsealed-input" >> pure Nothing
              else if not depsOk || hasUnconditionallyUntrackedCompileTimeExecution (ms_hspp_opts modSum)
              then do
                -- Only the deps this cycle actually marked invalid —
                -- not every direct dependency, which the always-on
                -- 'memoMiss' summary line above lists in full.
                invalidDeps <- if depsOk then pure [] else liftIO $ do
                  validMap <- readIORef validThisCycleRef
                  pure [ moduleNameString d
                       | d <- Set.toList (directHomeDeps modSum)
                       , not (Map.findWithDefault False d validMap) ]
                let traceReason
                      | not depsOk = "dependency-miss:" ++ unwords invalidDeps
                      | otherwise = "no-reuse:" ++ maybe "untracked-compile-time-execution"
                          ("extension=" ++) (untrackedExtensionName (ms_hspp_opts modSum))
                memoMiss modSum (if depsOk then "untracked-compile-time-execution" else "dependency-miss:" ++ unwords
                  [ moduleNameString d | d <- Set.toList (directHomeDeps modSum) ])
                memoMissTrace modSum traceReason Nothing
                pure Nothing
              else do
                m <- liftIO (readIORef ref)
                case Map.lookup (ms_mod_name modSum) m of
                  Nothing -> do
                    memoMiss modSum "absent"
                    memoMissTrace modSum "absent" Nothing
                    pure Nothing
                  Just entry -> do
                    let validity = gmeValidity entry
                        sameHash = memoSourceHash validity == ms_hs_hash modSum
                        sameOwner = payloadOwner (gmePayload entry) == ms_mod modSum
                        sameRetained = memoRetained validity == retainedFor modSum
                        sameHomeDependencies =
                          memoHomeDependencies validity == homeDependencyWitnesses modSum
                        sameExact = sameExactEnvironment modSum validity
                        -- Session generations are incarnation-local,
                        -- even within a multi-operation transaction.
                        -- Reuse requires both sides to name that owner.
                        -- Every
                        -- non-session module is exempt: its identity
                        -- is not incarnation-scoped.
                        sameIncarnation =
                          not (isJust (parseSessionModule (moduleNameString (ms_mod_name modSum))))
                            || (isJust incarnation && memoIncarnation validity == incarnation)
                        -- Unconditional compile-time execution was excluded
                        -- above. Only a recorded absence of quotations permits
                        -- reuse; equal source bytes preserve that parser fact.
                        compileTimeExecutionTracked =
                          moduleFactQuasiQuoteUse (payloadFacts (gmePayload entry)) == NoQuasiQuotes
                    -- Source hashes do not cover compile-time external inputs.
                    if not (moduleFactHasDependentFiles (payloadFacts (gmePayload entry)))
                        && compileTimeExecutionTracked
                        && sameHash
                        && sameOwner
                        && sameRetained
                        && sameHomeDependencies
                        && sameExact
                        && sameIncarnation
                      then pure (Just entry)
                      else if not compileTimeExecutionTracked
                        then do
                          memoMiss modSum "untracked-compile-time-execution"
                          memoMissTrace modSum
                            ("no-reuse:untracked-compile-time-execution quasiquotes="
                              ++ renderQuasiQuoteUse (moduleFactQuasiQuoteUse (payloadFacts (gmePayload entry))))
                            (Just entry)
                          pure Nothing
                        else do
                          memoMiss modSum $ unwords
                            [ "dependent-files=" ++ show (moduleFactHasDependentFiles (payloadFacts (gmePayload entry)))
                            , "same-hash=" ++ show sameHash
                            , "same-owner=" ++ show sameOwner
                            , "same-retained=" ++ show sameRetained
                            , "same-home-dependencies=" ++ show sameHomeDependencies
                            , "same-exact-environment=" ++ show sameExact
                            , "same-incarnation=" ++ show sameIncarnation
                            , "quasiquotes=" ++ renderQuasiQuoteUse (moduleFactQuasiQuoteUse (payloadFacts (gmePayload entry))) ]
                          memoMissTrace modSum (unwords
                            [ "dependent-files=" ++ show (moduleFactHasDependentFiles (payloadFacts (gmePayload entry)))
                            , "same-hash=" ++ show sameHash
                            , "same-owner=" ++ show sameOwner
                            , "same-retained=" ++ show sameRetained
                            , "same-home-dependencies=" ++ show sameHomeDependencies
                            , "same-exact-environment=" ++ show sameExact
                            , "same-incarnation=" ++ show sameIncarnation
                            , "quasiquotes=" ++ renderQuasiQuoteUse (moduleFactQuasiQuoteUse (payloadFacts (gmePayload entry))) ]) (Just entry)
                          pure Nothing
    let sourceOrder = [summary
          | ModuleNode _ summary <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing)
          , ms_hsc_src summary == HsSrcFile]
    sourceSelection <- case pvExactScope variant of
      Nothing -> pure Nothing
      Just scope -> withSourceSelectionRefusal
        (selectCurrentSourceOriginals scope (programSourceImports (pvPurpose variant))
          (pvSourceImportIntents variant) (pvGeneratedScaffold variant) modGraphRaw)
    selectedExact <- traverse (either (liftIO . fail) pure . extendSourceSelectedOriginals sourceSelection)
      (pvExactScope variant)
    let (freshGraph, exactImports) = sourceEvidenceGraph selectedExact modGraphRaw
        exactCompilation = (\scope -> ExactCompilation scope requestIdentity path exactImports sourceSelection)
          <$> pvExactScope variant
    when (any (\(_, imports) -> any (\(_, _, boot, _) -> boot) imports) exactImports) $
      liftIO $ ioError $ userError "exact scope does not admit SOURCE boot imports"
    let sourceFreeOwners = Set.fromList (pvDownsweepExcludes variant
          ++ [mkModuleName (exactModule iface)
             | scope <- maybe [] pure (pvExactScope variant)
             , iface <- scopeValueInterfaces scope])
    normalCandidates <- case candidateManifest of
      Nothing -> pure Map.empty
      Just manifest -> certifyModuleCandidates requestIdentity (forkExactContextWithPackageFacts packageFinder)
        compilerViewDirectory (compilerProducerFor variant) selectedExact
        sourceFreeOwners capturedCandidates manifest modGraphRaw path
    let acceptedCandidates = if sourceReuseDisabled then Map.empty else normalCandidates
    -- One dependency-order decision combines current admitted originals with
    -- source memo validation. An offered name or a previous HPT entry cannot
    -- substitute for this cycle's accepted candidate proof.
    validatedMemo <- do
        retainedEntries <- fmap catMaybes $ forM sourceOrder $ \summary ->
          case Map.lookup (ms_mod_name summary) normalCandidates of
            Just _ -> recordValidity summary True >> pure Nothing
            Nothing -> do
              cached <- lookupValidMemo summary
              let ready = cached >>= \entry -> case cachedInterface summary entry of
                    Nothing -> Nothing
                    Just _ -> Just entry
              recordValidity summary (isJust ready)
              pure ((\entry -> (ms_mod summary,entry)) <$> ready)
        liftIO (writeIORef validThisCycleRef Map.empty)
        current <- getSession
        selectedVersions <- liftIO (readIORef selectedVersionsRef)
        let normalEntries = Map.fromList retainedEntries
            validEntries = if sourceReuseDisabled then Map.empty else normalEntries
            restoredHomes = [if backendGeneratesCode (backend (ms_hspp_opts summary))
                  then hmi else hmi {hm_linkable=emptyHomeModInfoLinkable}
              | summary <- sourceOrder
              , let owner = ms_mod summary
              , Just entry <- [Map.lookup owner validEntries]
              , Just node <- [Map.lookup owner selectedVersions]
              , let hmi = fromMaybe (finalizedHomeModInfo (payloadFinalized (gmePayload entry)))
                      (completedModuleHome node <|> lookupHpt (hsc_HPT (completedModuleEnvironment node)) (moduleName owner))
              , isNothing (lookupHpt (hsc_HPT current) (ms_mod_name summary))
              , let finalized = hm_iface (finalizedHomeModInfo (loadedFinalized
                      (payloadLoaded summary (gmePayload entry))))
              , mi_module finalized == owner
              , mi_iface_hash (mi_final_exts (hm_iface hmi)) == mi_iface_hash (mi_final_exts finalized)]
        when sourceReuseDisabled $ liftIO $ do
          writeIORef disabledSourceOwnersRef (Set.union (Map.keysSet normalEntries)
            (Set.fromList [ms_mod summary | summary <- sourceOrder
              , ms_mod_name summary `Map.member` normalCandidates]))
          forM_ mMemoRef (`writeIORef` Map.empty)
        setSession (hscUpdateHPT (\table -> foldr
          (\hmi homes -> addToHpt homes (moduleName (mi_module (hm_iface hmi))) hmi)
          table restoredHomes) current)
        pure validEntries
    disabledSourceOwners <- liftIO (readIORef disabledSourceOwnersRef)
    forM_ sourceOrder $ \summary -> liftIO $ do
      let retainedVersion = ms_mod summary `Map.member` validatedMemo
          canonicalVersion = ms_mod_name summary `Map.member` acceptedCandidates
          reusable = retainedVersion || canonicalVersion
          disabled = ms_mod summary `Set.member` disabledSourceOwners
          decision | disabled = ReuseDisabled
                   | reusable = ReuseHit
                   | otherwise = ReuseMiss
          reason | disabled = CacheDisabled
                 | reusable = Matched
                 | ms_mod summary `Set.member` unsealedClosure = ChangedDependency
                 | hasUnconditionallyUntrackedCompileTimeExecution (ms_hspp_opts summary) = ThFresh
                 | otherwise = case cycleState of
                     StandaloneCycle -> CacheDisabled
                     TransactionCycle _ _ _ versions _ _ _ _ ->
                       case Map.lookup (ms_mod summary) versions of
                         Nothing -> Absent
                         Just previous ->
                           if any ((== ms_hs_hash summary) . memoSourceHash)
                               [validity | entries <- Map.elems previous, validity <- Map.keys entries]
                             then ChangedDependency else ChangedSource
      reuseEvent SourceFrontend decision reason summary
      when disabled $ do
        reuseEvent FinalizedCore ReuseDisabled CacheDisabled summary
        reuseEvent Interface ReuseDisabled CacheDisabled summary
      when reusable $ do
        reuseEvent FinalizedCore ReuseHit Matched summary
        reuseEvent Interface ReuseHit Matched summary
    case cycleState of
      StandaloneCycle -> pure ()
      TransactionCycle _ _ _ _ activateRecovery _ _ _ -> liftIO $ do
        selectedVersions <- readIORef selectedVersionsRef
        activateRecovery (Map.restrictKeys selectedVersions (Map.keysSet validatedMemo))
    let recompiledOwners = Set.fromList [ms_mod summary | summary <- sourceOrder
          , ms_mod summary `Map.notMember` validatedMemo
          , ms_mod_name summary `Map.notMember` acceptedCandidates]
    beforeEpoch <- liftIO (compilerInterpreterEpoch <$> readIORef interpreterState)
    liftIO markInterpreterMutation
    getSession >>= liftIO . activateCompilerInterpreter interpreterState selectedExecutables recompiledOwners
    liftIO (writeIORef interpreterAttempt InterpreterConfirmed)
    epoch <- liftIO (compilerInterpreterEpoch <$> readIORef interpreterState)
    when (epoch /= beforeEpoch) $ do
      liftIO (emitReuse timing reuseContext NativeImage ReuseEpochRotated Epoch Nothing 1 Nothing)
      current <- getSession
      setSession (hscUpdateHPT (\table -> foldr
        (\home homes -> addToHpt homes (moduleName (mi_module (hm_iface home))) (withoutBytecode home))
        emptyHomePackageTable (eltsHpt table)) current)
    case cycleState of
      StandaloneCycle -> pure ()
      TransactionCycle cache _ _ _ _ _ _ _ -> do
        selectedVersions <- liftIO (readIORef selectedVersionsRef)
        forM_ sourceOrder $ \summary -> forM_ (Map.lookup (ms_mod summary) validatedMemo) $ \entry -> do
          current <- getSession
          let finalized = payloadFinalized (gmePayload entry)
              original = fromMaybe (finalizedHomeModInfo finalized)
                (lookupHpt (hsc_HPT current) (ms_mod_name summary))
              nodeEpoch = completedModuleEpoch <$> Map.lookup (ms_mod summary) selectedVersions
              sameEpoch = nodeEpoch == Just epoch
              home = if sameEpoch then original else withoutBytecode original
              hasCode = isJust (homeMod_bytecode (hm_linkable home))
                || isJust (homeMod_object (hm_linkable home))
              needsBytecode = backendGeneratesCode (backend (ms_hspp_opts summary))
                && (not sameEpoch || not hasCode)
          -- Observe the actual capacity branch separately from source validity.
          forM_ executableObservationsRef $ \observations -> liftIO $
            modifyIORef' observations (Map.insert (ms_mod summary)
              (MemoExecutableTrace nodeEpoch epoch hasCode needsBytecode))
          refreshed <- if needsBytecode
            then do
              -- Executable demand can follow metadata-only preparation in the
              -- same epoch. A retired epoch also needs new RemotePtrs. Both
              -- consume the retained finalized Core and publish its complete
              -- current interface/linkable pair without a source frontend.
              let native = scopeRetainedSummaryHscEnv summary current
                  guts = (finalizedTidyGuts finalized) {cg_modBreaks=Nothing}
              liftIO markInterpreterMutation
              bytecode <- timePhase timing "retained_source_bytecode" (liftIO
                (generateFreshByteCode native (ms_mod_name summary) (mkCgInteractiveGuts guts) (ms_location summary)))
              pure home {hm_linkable=justBytecode bytecode}
            else pure home
          setSession (hscUpdateHPT (\table -> addToHpt table (ms_mod_name summary) refreshed) current)
          -- Make publishes empty linkables for metadata-only loads. This
          -- selected complete owner retains the exact current executable
          -- capacity, including across code-to-metadata-to-code transitions.
          liftIO (addHmiToCache cache refreshed)
    -- Observe the original ingress, not the already-pruned working memo.
    -- Failed checks inspect only versions under this exact immutable key.
    when memoTrace $ liftIO $ do
      observedSelections <- readIORef selectedVersionsRef
      observedExecutables <- maybe (pure Map.empty) readIORef executableObservationsRef
      forM_ (Map.toList summaryByDependency) $ \(dependency@(HomeDependency name kind), summary) -> do
        let HomeDependencyWitness selectedPath fingerprint = summaryFingerprints Map.! dependency
            resolvedPath = normalise <$> ml_hs_file (ms_location summary)
            directDeps = [moduleNameString d | d <- Set.toList (directHomeDeps summary)]
            digestHex = case Map.lookup dependency dependencyDigests of
              Just (HomeDependencyDigest bytes) -> hexBytes bytes
              Nothing -> "<none>"
            keyFacts = show (ms_hs_hash summary, retainedFor summary,
              [(moduleNameString child, show sourceKind, hexBytes bytes)
                | (HomeDependency child sourceKind, HomeDependencyDigest bytes) <-
                    Map.toAscList (homeDependencyWitnesses summary)],
              incarnationFor summary,
              [(owner, hexBytes bytes) | (owner, HomeDependencyDigest bytes) <-
                  Map.toAscList (exactImportEnvironment summary)])
            keySha = hexBytes (SHA256.hash (TextEncoding.encodeUtf8 (Text.pack keyFacts)))
            observation state narrowed originating failures omitted = MemoSelectionTrace
              state (unitString (moduleUnit (ms_mod summary))) keySha narrowed originating failures omitted
                (Map.lookup (ms_mod summary) observedExecutables)
            selection = case cycleState of
              StandaloneCycle -> observation MemoStandalone 0 Nothing [] 0
              TransactionCycle _ _ _ versions _ _ _ _
                | ms_mod summary == targetOwner -> observation MemoTargetExcluded 0 Nothing [] 0
                | otherwise -> case Map.lookup (ms_mod summary) versions of
                    Nothing -> observation MemoOwnerAbsent 0 Nothing [] 0
                    Just byIngress ->
                      let ingress = MemoSelectionKey (ms_hs_hash summary) (retainedFor summary)
                            (homeDependencyWitnesses summary) (incarnationFor summary)
                            (exactImportEnvironment summary)
                      in case Map.lookup ingress byIngress of
                        Nothing -> observation MemoIngressAbsent 0 Nothing [] 0
                        Just narrowed ->
                          let checked = [(node, map memoSelectionRefusalName (selectionRefusals summary node))
                                | node <- Map.elems narrowed]
                              selected = Map.lookup (ms_mod summary) observedSelections
                              rejected = [(gmeCycle (completedModuleEntry node), checks)
                                | (node, checks) <- checked, not (null checks)]
                          in observation (if isJust selected then MemoSelected else MemoMatchingRejected)
                            (Map.size narrowed) (gmeCycle . completedModuleEntry <$> selected)
                            (take 16 rejected) (max 0 (length rejected - 16))
        emitMemoCycleGraph memoTrace requestIdentity (moduleNameString name) (show kind)
          selectedPath resolvedPath fingerprint directDeps digestHex selection
    let bodyTier = nativeBodyTier (pvPurpose variant)
    -- 'ghc_setup' phase (TIDEPOOL_TIMING): 'guessTarget'/'setTargets' + this
    -- 'depanal' call, nothing else, on EVERY caller — a lone compile also
    -- includes its session bootstrap because 'runCompile' captures
    -- 'sessionT0' first. A resident request starts after the shared bootstrap.
    -- This phase is flat and non-overlapping with 'ghc_load'.
    setupT1 <- monotonicTime
    endResourceTiming setupResources "compile" "ghc_setup"
    liftIO (emitPhase timing "ghc_setup" (elapsedMs sessionT0 setupT1))
    observedPlan <- pvPlan variant compilerViewDirectory timing modGraphRaw selectedExact
    -- Returned environments outlive this request's observer, just as the
    -- resident session does. Keep all plan-owned hooks, restoring only ours.
    let originalPlan = observedPlan
          { cpFinalEnv = \env ->
              let final = cpFinalEnv observedPlan env
              in final {hsc_hooks=(hsc_hooks final) {runMetaHook=originalMetaHook}} }
    certifiedEnv <- getSession
    let acceptedNames = Map.keysSet acceptedCandidates
        loadRequired = any ((== CandidateLoadForExecution) . admittedCandidateLoading)
          (Map.elems acceptedCandidates)
        sourceSummaries = [summary | ModuleNode _ summary <- mgModSummaries' modGraphRaw]
        importedNames summary =
          [ unLoc name | (_, name) <- ms_textual_imps summary ++ ms_srcimps summary ]
        importerClosure seed =
          let grown = Set.union seed (Set.fromList
                [ ms_mod_name summary
                | summary <- sourceSummaries
                , any (`Set.member` seed) (importedNames summary) ])
          in if grown == seed then seed else importerClosure grown
        deferred = if loadRequired then Set.empty else importerClosure acceptedNames
        candidateLoadGraph = mkModuleGraph
          [ node | node <- mgModSummaries' (cpLoadGraph originalPlan)
          , case node of
              ModuleNode _ summary -> ms_mod_name summary `Set.notMember` deferred
              _ -> True ]
        plan | Set.null acceptedNames = originalPlan
             | otherwise = originalPlan
                 { cpLoadGraph = candidateLoadGraph
               , cpAfterLoad = do
                     cpAfterLoad originalPlan
                     current <- getSession
                     -- Exact hydration owns the lexical graph; installing
                     -- source-selected candidates preserve that authority.
                     when (isNothing (pvExactScope variant)) $
                       setSession current { hsc_mod_graph = modGraphRaw }
                     forM_ (Set.toAscList acceptedNames) $ \name ->
                       case lookupHpt (hsc_HPT certifiedEnv) name of
                         Nothing -> liftIO $ ioError $ userError
                           "certified candidate disappeared before HPT installation"
                         Just hmi -> do
                           forM_ (Map.lookup name acceptedCandidates) $ \candidate ->
                             case admittedCandidateLoading candidate of
                               CandidateInterfaceOnly -> pure ()
                               CandidateLoadForExecution -> do
                                 loaded <- getSession
                                 case lookupHpt (hsc_HPT loaded) name of
                                   Just executable
                                     | mi_module (hm_iface executable) == mi_module (hm_iface hmi)
                                     , let linkable = hm_linkable executable
                                     , isJust (homeMod_bytecode linkable) || isJust (homeMod_object linkable) ->
                                         pure ()
                                   _ -> liftIO $ ioError $ userError
                                     "source-selected native candidate lacks its current GHC executable"
                           installPreparedInterface name hmi
                 }
    liftIO (emitCount timing "candidate_executable_required"
      (toInteger (length [() | candidate <- Map.elems acceptedCandidates
        , admittedCandidateLoading candidate == CandidateLoadForExecution])))
    -- Restore the representation-affecting extraction flags before 'load''
    -- touches a home module. Its TH/QQ downgrade sets
    -- Opt_OmitInterfacePragmas, which disables automatic field unboxing, and
    -- leaves Opt_UnboxSmallStrictFields at -O0. A type graph can then intern
    -- the downgraded DataCon while executable STG from another program interns
    -- its canonical layout, giving one nominal constructor two physical
    -- declarations.
    --
    -- Keep the downgrade's interpreter backend and link settings intact:
    -- 'load'' needs those to provision splice bytecode. The bytecode choice is
    -- still session-wide in 'canonicalizeDFlags'
    -- (Opt_UseBytecodeRatherThanObjects).
    let canonicalizeLoadSummary summary =
          let original = ms_hspp_opts summary
              selected = maybe summary admittedCandidateView
                (Map.lookup (ms_mod_name summary) acceptedCandidates)
              flags = (canonicalizeDFlags original)
                { backend = backend original, ghcLink = ghcLink original }
              actualFlags
                | ms_mod summary `Set.member` disabledSourceOwners = gopt_set flags Opt_ForceRecomp
                | otherwise = flags
          in selected { ms_hspp_opts = actualFlags }
    let plannedLoadGraph = cpLoadGraph plan
        (loadGraph, loadHowMuch) = case preparation of
          CheckOnly ->
            -- GHC 9.12's LoadDependenciesOf build plan also compiles its root.
            -- An acyclic leaf target belongs to the checked frontend below;
            -- exclude it from load rather than checking it twice. Preserve a
            -- target needed by another node or its own hs-boot graph intact.
            case [ (node, ms)
                 | node@(ModuleNode _ ms) <- mgModSummaries' plannedLoadGraph
                 , ms_mod_name ms == targetName
                 , ms_hsc_src ms == HsSrcFile ] of
              (targetNode, summary) : _
                | let targetKey = mkNodeKey targetNode
                , not (any (\case
                    ModuleNode _ ms -> ms_mod ms == ms_mod summary && ms_hsc_src ms == HsBootFile
                    _ -> False) (mgModSummaries' plannedLoadGraph))
                , not (any (elem targetKey . nodeDependencies False) (mgModSummaries' plannedLoadGraph)) ->
                    (mkModuleGraph [node | node <- mgModSummaries' plannedLoadGraph
                      , mkNodeKey node /= targetKey], LoadAllTargets)
                | otherwise -> (plannedLoadGraph, LoadDependenciesOf
                    (mkModule (homeUnitId (hsc_home_unit previous)) targetName))
              [] -> (plannedLoadGraph, LoadAllTargets)
          _ -> (plannedLoadGraph, LoadAllTargets)
    targetLoadFailure <- liftIO (newIORef Nothing)
    targetInstanceFailure <- liftIO (newIORef Nothing)
    canonicalFailureRef <- liftIO (newIORef Nothing)
    frontendQuasiQuotesRef <- liftIO (newIORef Map.empty)
    pendingFinalizationsRef <- liftIO (newIORef Map.empty)
    beforeLoad <- getSession
    let originalPhaseHook = runPhaseHook (hsc_hooks beforeLoad)
        publishLoadedFinalization loaded = do
          let name = ms_mod_name (loadedSummary loaded)
              finalized = loadedFinalized loaded
          unless (name `Map.member` acceptedCandidates) $ do
            atomicModifyIORef' finalizedModulesRef (\known -> (Map.insert name finalized known, ()))
            when captureProducts $ atomicModifyIORef' productInterfacesRef
              (\known -> (Map.insert name (hm_iface (finalizedHomeModInfo finalized)) known, ()))
        runOriginalPhase :: TPhase a -> IO a
        runOriginalPhase phase = case originalPhaseHook of
          Nothing -> runPhase phase
          Just (PhaseHook hook) -> hook phase
        canonicalSummary summary = summary
          { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts summary) }
        canonicalEnvironment env summary = scopeRetainedSummaryHscEnv (canonicalSummary summary) env
        validateCandidateInterface env name iface =
          forM_ (Map.lookup name acceptedCandidates) $ \candidate -> do
            directory <- getTemporaryDirectory
            bytes <- serializeOriginalInterface env directory (set_mi_extra_decls Nothing iface)
            unless (hexBytes (SHA256.hash bytes) ==
                candidateInterfaceSha256 (admittedCandidateOriginal candidate)) $
              throwIO (CandidateInterfaceBytesMismatch name)
        validateCandidateHomeInterfaces env =
          forM_ (Map.keys acceptedCandidates) $ \name ->
            forM_ (lookupHpt (hsc_HPT env) name) $ \hmi ->
              validateCandidateInterface env name (hm_iface hmi)
        -- The full certified census describes availability, not selected roots.
        -- Resolve only those identities through the hydrated HMI's actual Names.
        originalInterfaceBindings env owner expectedSha nativeBinders = do
          home <- maybe (throwIO (OriginalNativeHomeMissing owner)) pure
            (lookupHpt (hsc_HPT env) (moduleName owner))
          unless (mi_module (hm_iface home) == owner) $
            throwIO (OriginalNativeInterfaceMismatch owner)
          directory <- getTemporaryDirectory
          bytes <- serializeOriginalInterface env directory (set_mi_extra_decls Nothing (hm_iface home))
          unless (hexBytes (SHA256.hash bytes) == expectedSha) $
            throwIO (OriginalNativeInterfaceMismatch owner)
          pure (Map.fromList
            [(idName identifier, identity)
            | identifier <- typeEnvIds (md_types (hm_details home))
            , nameModule_maybe (idName identifier) == Just owner
            , let identity = preparedRootIdentity identifier
            , identity `Set.member` nativeBinders])
        availableOriginalBindings env = do
          candidateBindings <- forM (Map.toAscList acceptedCandidates) $ \(name, admission) -> do
            home <- maybe (throwIO (CandidateOriginalHomeMissing name)) pure
              (lookupHpt (hsc_HPT env) name)
            let candidate = admittedCandidateOriginal admission
                owner = mkModule (stringToUnit (candidateUnit candidate))
                  (mkModuleName (candidateModule candidate))
                nativeBinders = Set.fromList
                  [binder | group <- candidateGroups candidate
                    , binder <- candidateGroupBinders group]
            unless (mi_module (hm_iface home) == owner) $
              throwIO (CandidateInterfaceBytesMismatch name)
            originalInterfaceBindings env owner (candidateInterfaceSha256 candidate) nativeBinders
          nativeBindings <- forM
            [original | compilation <- maybe [] pure exactCompilation
              , original <- scopeAvailableOriginalProducts (compilationScope compilation)] $ \original -> do
            let owner = mkModule (stringToUnit (originalUnit original)) (mkModuleName (originalModule original))
                nativeBinders = Set.fromList
                  [binder | group <- originalGroups original, binder <- originalBinders group]
            originalInterfaceBindings env owner (originalIfaceSha256 original) nativeBinders
          foldM (\known bindings -> do
            forM_ (Map.toAscList bindings) $ \(name,identity) ->
              forM_ (Map.lookup name known) $ \previous -> unless (previous == identity)
                (throwIO (OriginalNativeNameConflict name))
            pure (Map.union known bindings)) Map.empty (candidateBindings ++ nativeBindings)
        retainLoaded pending iface = do
          unless (mi_module iface == ms_mod (pendingSummary pending)) $
            throwIO LoadedFinalizationOwnerMismatch
          let skinny = set_mi_extra_decls Nothing iface
              finalized = FinalizedModule
                (HomeModInfo skinny (pendingDetails pending) emptyHomeModInfoLinkable)
                (pendingTidyGuts pending)
              loaded = LoadedModule (pendingSummary pending) (pendingFacts pending)
                (pendingOutput pending) finalized
              name = ms_mod_name (pendingSummary pending)
          -- A candidate's bytecode may be needed by a splice. Its recompilation
          -- may not supply a different interface to an importer while the old
          -- certified product remains selected downstream.
          validateCandidateInterface (pendingEnvironment pending) name skinny
          atomicModifyIORef' loadedModulesRef (\known ->
            (Map.insert (ms_mod (pendingSummary pending)) loaded known, ()))
          publishLoadedFinalization loaded
        canonicalLoadPhase :: TPhase a -> IO a
        canonicalLoadPhase (T_Hsc phaseEnv summary)
          | ms_hsc_src summary == HsSrcFile = do
              let summaryC = canonicalSummary summary
                  env = canonicalEnvironment phaseEnv summary
                  originalBackend = backend (hsc_dflags phaseEnv)
              unless (not (backendGeneratesCode originalBackend) || backendCanReuseLoadedCode originalBackend) $
                throwIO UnsupportedLoadBackend
              when (isJust (hscFrontendHook (hsc_hooks phaseEnv))) (throwIO CustomLoadFrontendHook)
              when (ms_mod_name summary `Map.member` acceptedCandidates) $
                throwIO (CandidateFrontendReplayRefused (ms_mod_name summary))
              validateCandidateHomeInterfaces env
              -- GHC make supplies the dependency HPT for this exact phase.
              -- Never mutate or reuse the outer Ghc Session from a load hook.
              session <- Session <$> newIORef env
              reflectGhc (do
                cpBeforeModule plan summaryC
                current <- getSession
                parsed <- parseModule summaryC
                let quotes = classifyQuasiQuoteUse parsed
                transformed <- liftIO (pvTransformParsed variant current summaryC parsed)
                ((tcg, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName current summaryC parsed
                  (typecheckNativeModuleWithDiagnostics current transformed)
                liftIO (validateCompilationFamilies current tcg)
                liftIO $ do
                  atomicModifyIORef' frontendQuasiQuotesRef (\known ->
                    (Map.insert (ms_mod summaryC) (quotes, current) known, ()))
                  when (ms_mod_name summaryC == targetName) (writeIORef targetEnvironmentRef (Just tcg))
                  reuseEvent SourceFrontend ReuseWork Absent summaryC
                  when timing $ hPutStrLn stderr $
                    "tidepool-canonical-frontend module=" ++ moduleNameString (ms_mod_name summaryC)
                pure (FrontendTypecheck tcg, warnings)) session
                `catch` \failure -> do
                  when (ms_mod_name summary == targetName) $
                    writeIORef targetInstanceFailure (Just (failure :: GeneratedInstanceRejection))
                  throwIO failure
        canonicalLoadPhase (T_Hsc phaseEnv summary) =
          runPhase (T_Hsc (exactHomeInstancesFor summary phaseEnv) summary)
        canonicalLoadPhase (T_HscPostTc phaseEnv summary (FrontendTypecheck tcg) tcWarnings oldHash)
          | ms_hsc_src summary == HsSrcFile = do
              (quotes, env) <- atomicModifyIORef' frontendQuasiQuotesRef (\known ->
                (Map.delete (ms_mod summary) known, Map.lookup (ms_mod summary) known))
                >>= maybe (throwIO MissingLoadedFrontend) pure
              let summaryC = canonicalSummary summary
              (typedEnv, typedSegment) <- prepareTypedTarget summaryC env tcg
              let flags = hsc_dflags typedEnv
                  resultRoots = [idName identifier | cpKeepPrivateResult plan
                    , ms_mod_name summaryC == targetName
                    , identifier <- typeEnvIds (tcg_type_env tcg)
                    , occNameString (nameOccName (idName identifier)) `elem` cpResultBinders plan
                    , nameModule_maybe (idName identifier) == Just (ms_mod summaryC)]
              modifyIORef' (tcg_keep tcg) (`extendNameSetList` resultRoots)
              (desugared0, dsWarnings) <- timeDetailPhase timing "ghc_load_desugar"
                (moduleNameString (ms_mod_name summaryC)) $
                runHsc' typedEnv (hscDesugar' (ms_location summaryC) tcg)
              desugared <- installSegment typedEnv typedSegment desugared0
              printOrThrowDiagnostics (hsc_logger env) (initPrintConfig flags) (initDiagOpts flags)
                (unionMessages tcWarnings dsWarnings)
              plugins <- readIORef (tcg_th_coreplugins tcg)
              simplified <- timeDetailPhase timing "ghc_load_simplify"
                (moduleNameString (ms_mod_name summaryC)) (hscSimplify typedEnv plugins desugared)
              (tidy, details) <- hscTidy typedEnv simplified
              roots <- directPackageImports env tcg
              files <- readIORef (tcg_dependent_files tcg)
              let partial = force (mkPartialIface typedEnv (cg_binds tidy) details summaryC
                    (tcg_import_decls tcg) simplified)
                  externalized = externalizeInternalTops simplified
                  output = ModuleOutput (mg_module externalized) (mg_binds externalized)
                    (capturedBindingDisplay evalUserBinder tcg) (capturedCellBinderPins env tcg)
                    (foldr (<|>) Nothing [capturedBindingType name tcg | name <- cpResultBinders plan])
                  facts = ModuleFacts (mg_tcs desugared) (moduleRefs desugared) roots
                    (not (null files)) quotes
                  pending = PendingFinalization summaryC facts output tidy details typedEnv
                  action = HscRecomp tidy (ms_location summaryC) partial oldHash
              reuseEvent FinalizedCore ReuseWork Absent summaryC
              reuseEvent Interface ReuseWork Absent summaryC
              when timing $ hPutStrLn stderr $
                "tidepool-canonical-finalization module=" ++ moduleNameString (ms_mod_name summaryC)
              if not (backendGeneratesCode (backend (hsc_dflags phaseEnv)))
                then do
                  iface <- mkFullIface typedEnv partial Nothing Nothing NoStubs []
                  hscMaybeWriteIface (hsc_logger env) flags True iface oldHash (ms_location summaryC)
                  retainLoaded pending iface
                  pure (HscUpdate (set_mi_extra_decls Nothing iface))
                else if backendCanReuseLoadedCode (backend (hsc_dflags phaseEnv))
                  then do
                    atomicModifyIORef' pendingFinalizationsRef (\known ->
                      (Map.insert (ms_mod summaryC) pending known, ()))
                    pure action
                  else throwIO UnsupportedLoadBackend
        canonicalLoadPhase (T_HscBackend pipe phaseEnv name HsSrcFile location action@HscRecomp{}) = do
          unless (backendCanReuseLoadedCode (backend (hsc_dflags phaseEnv))) $
            throwIO UnsupportedLoadBackend
          let owner = cg_module (hscs_guts action)
          pending <- atomicModifyIORef' pendingFinalizationsRef (\known ->
            (Map.delete owner known, Map.lookup owner known))
            >>= maybe (throwIO MissingLoadedFinalization) pure
          let env = hscUpdateFlags (\flags -> flags
                { backend = backend (hsc_dflags phaseEnv), ghcLink = ghcLink (hsc_dflags phaseEnv) })
                (pendingEnvironment pending)
          result@(_, iface, _, _) <- runOriginalPhase
            (T_HscBackend pipe env name HsSrcFile location action)
          retainLoaded pending iface
          let (files, _, linkable, output) = result
          pure (files, set_mi_extra_decls Nothing iface, linkable, output)
        canonicalLoadPhase phase = runOriginalPhase phase
        targetPhase :: TPhase a -> Bool
        targetPhase (T_Hsc _ summary) = ms_mod_name summary == targetName
        targetPhase (T_HscPostTc _ summary _ _ _) = ms_mod_name summary == targetName
        targetPhase (T_HscBackend _ _ name _ _ _) = name == targetName
        targetPhase _ = False
        captureCanonicalFailure :: TPhase a -> IO a
        captureCanonicalFailure phase = (observedCanonicalLoad phase
          `catch` (\failure -> do
            when (targetPhase phase) (writeIORef typedFailureRef (Just (failure :: TypedSegmentFailure)))
            throwIO failure))
          `catch` (\failure -> do
            when (targetPhase phase) (writeIORef targetLoadFailure (Just (failure :: SourceError)))
            throwIO failure)
          `catch` (\failure -> do
            writeIORef canonicalFailureRef (Just (failure :: CanonicalFrontendFailure))
            throwIO failure)
        observedCanonicalLoad :: TPhase a -> IO a
        observedCanonicalLoad phase@(T_Hsc _ summary)
          | ms_hsc_src summary == HsSrcFile = timeModuleDetailPhase timing
              "compile" "source_frontend_task_service" (ms_mod summary)
              (canonicalLoadPhase phase)
        observedCanonicalLoad phase@(T_HscPostTc _ summary (FrontendTypecheck _) _ _)
          | ms_hsc_src summary == HsSrcFile = timeModuleDetailPhase timing
              "compile" "source_finalization_task_service" (ms_mod summary)
              (canonicalLoadPhase phase)
        observedCanonicalLoad phase = canonicalLoadPhase phase
    when (isJust originalPhaseHook) (liftIO (throwIO CustomLoadPhaseHook))
    when (isJust (hscFrontendHook (hsc_hooks beforeLoad))) (liftIO (throwIO CustomLoadFrontendHook))
    -- The outer extraction flags use NoLink, so GHC Make's LinkInMemory
    -- unload branch is inactive. This owner already selected the compatible
    -- executable epoch before planning; per-file TH backends do not change it.
    -- Make prefers its cached interface over the current summary's staged
    -- interface. An admitted original owns that choice now; remove its prior
    -- interface and linkable together while preserving every other cache entry.
    let candidateOwners = Set.fromList
          [ms_mod (admittedCandidateView candidate) | candidate <- Map.elems acceptedCandidates]
        currentMemoInterfaces = Map.map (hm_iface . finalizedHomeModInfo .
          loadedFinalized . (\entry -> payloadLoaded (sourceSummaries Map.! payloadOwner (gmePayload entry)) (gmePayload entry))) validatedMemo
        sourceSummaries = Map.fromList [(ms_mod summary,summary) | ModuleNode _ summary <- mgModSummaries' modGraphRaw]
        keepCurrent iface = mi_module iface `Set.notMember` candidateOwners && case Map.lookup (mi_module iface) currentMemoInterfaces of
          Nothing -> True
          Just original -> mi_iface_hash (mi_final_exts iface) == mi_iface_hash (mi_final_exts original)
        loadCache = fmap (filterModIfaceCache keepCurrent) mCache
    -- Fresh providers compile after exact hydration under the same canonical
    -- frontend/finalization owner as GHC Make. Restore only this owner's
    -- temporary fields; the plan's Core-expression link hook remains in force
    -- until its existing cpFinalEnv boundary.
    let withCanonicalLoadHooks :: Ghc a -> Ghc a
        withCanonicalLoadHooks action = reifyGhc $ \session -> bracket
          (reflectGhc (getSession >>= \env -> setSession env
            { hsc_hooks = (hsc_hooks env)
                { runPhaseHook = Just (PhaseHook captureCanonicalFailure) }
            , hsc_dflags = (hsc_dflags env)
                { parMakeCount = Just (ParMakeThisMany (compilerModuleJobs executionGrant)) } }) session)
          (const (reflectGhc (getSession >>= \env -> setSession env
            { hsc_hooks = (hsc_hooks env) { runPhaseHook = originalPhaseHook }
            , hsc_dflags = (hsc_dflags env)
                { parMakeCount = parMakeCount (hsc_dflags beforeLoad) } }) session))
          (const (reflectGhc action session))
    withCanonicalLoadHooks $ do
      loadT0 <- monotonicTime
      loadResources <- beginResourceTiming timing
      liftIO markInterpreterMutation
      loadFlag <- load' loadCache loadHowMuch
        dependencyDiagnostic (Just batchMsg)
        (scopeRetainedModuleGraph (mapMG canonicalizeLoadSummary loadGraph))
      loadT1 <- monotonicTime
      endResourceTiming loadResources "compile" "ghc_load"
      -- 'ghc_load' phase (TIDEPOOL_TIMING): the 'load'' call alone, nothing
      -- else. FLAT — see 'ghc_setup' above; the two rows partition the work,
      -- they do not nest inside each other.
      liftIO (emitPhase timing "ghc_load" (elapsedMs loadT0 loadT1))
      when (timing && case preparation of CheckOnly -> True; _ -> False) $ do
        loadedEnv <- getSession
        forM_ (mgModSummaries' loadGraph) $ \node -> case node of
            ModuleNode _ summary
              | ms_hsc_src summary == HsSrcFile
              , ms_mod_name summary /= targetName
              , Just hmi <- lookupHpt (hsc_HPT loadedEnv) (ms_mod_name summary)
              , let linkable = hm_linkable hmi
              , isJust (homeMod_bytecode linkable) || isJust (homeMod_object linkable) ->
                  liftIO $ hPutStrLn stderr $
                    "tidepool-checked-dependency-executable module="
                      ++ moduleNameString (ms_mod_name summary)
                      ++ " bytecode=" ++ show (isJust (homeMod_bytecode linkable))
                      ++ " object=" ++ show (isJust (homeMod_object linkable))
            _ -> pure ()
      case loadFlag of
        Succeeded -> do
          getSession >>= liftIO . validateCandidateHomeInterfaces
          cpAfterLoad plan
          pending <- liftIO (readIORef pendingFinalizationsRef)
          quotes <- liftIO (readIORef frontendQuasiQuotesRef)
          unless (Map.null pending && Map.null quotes) $
            liftIO $ throwIO UnfinishedLoadedFrontend
        Failed -> do
          preparationFailure <- liftIO (readIORef typedPreparationFailureRef)
          forM_ preparationFailure (liftIO . throwIO)
          typedFailure <- liftIO (readIORef typedFailureRef)
          forM_ typedFailure (liftIO . throwIO)
          canonicalFailure <- liftIO (readIORef canonicalFailureRef)
          forM_ canonicalFailure (liftIO . throwIO)
          instanceFailure <- liftIO (readIORef targetInstanceFailure)
          forM_ instanceFailure (liftIO . throwIO)
          targetFailure <- liftIO (readIORef targetLoadFailure)
          forM_ targetFailure (liftIO . throwIO)
          diagnostics <- liftIO (nub . reverse <$> readIORef errorRef)
          liftIO $ throwIO $ if null diagnostics
            then DependencyWorkerFailure
            else DependencySourceFailure diagnostics
    -- Exclude hs-boot summaries in one shared site for both variants:
    -- a boot node shares its ModuleName with the real module, so its
    -- near-empty desugared guts would CLOBBER the real module's entry in
    -- the name-keyed 'gutsByMod' below — hiding every Core edge out of that
    -- module from 'reachableModuleClosure' and silently tiering its
    -- dependencies out of the optimized stage (observed live: the
    -- Even.hs-boot/Odd cycle baked a TypeMetadata sentinel for Odd.odd'),
    -- and merging boot guts as if they were the real module's on either
    -- path. Boot files exist for 'load''s loop-breaking only; any error in
    -- one already surfaced there, and their guts carry no bindings
    -- extraction could use.
    capturedModules <- liftIO (readIORef loadedModulesRef)
    let sourceSummariesByOwner = Map.fromList
          [(ms_mod summary,summary) | ModuleNode _ summary <- mgModSummaries' modGraphRaw]
        cachedModules = Map.mapMaybeWithKey (\owner entry ->
          (\summary -> payloadLoaded summary (gmePayload entry))
            <$> Map.lookup owner sourceSummariesByOwner) validatedMemo
        cachedOnly = Map.difference cachedModules capturedModules
        loadedModules = Map.union capturedModules cachedOnly
    liftIO (emitCount timing "transaction_reused_source_products" (toInteger
      (Map.size cachedOnly)))
    forM_ (Map.elems capturedModules) $ \loaded -> do
      let summary = loadedSummary loaded
      when (ms_mod summary /= targetOwner) $
        forM_ mMemoRef $ \ref -> liftIO $ modifyIORef' ref (Map.insert (ms_mod_name summary)
          (GutsMemoEntry (memoValidity summary (loadedFinalized loaded))
            (ValidationOnly (loadedFacts loaded) (loadedOutput loaded) (loadedFinalized loaded))
            requestIdentity (memoDiagnosticWitnesses summary)))
    forM_ (Map.elems cachedOnly) $ \loaded -> do
      liftIO (publishLoadedFinalization loaded)
      installPreparedInterface (ms_mod_name (loadedSummary loaded))
        (finalizedHomeModInfo (loadedFinalized loaded))
    summaries0 <- cpSummaries plan
    let summaries = [ ms | ms <- summaries0, ms_hsc_src ms == HsSrcFile ]
    when (null summaries) $
      liftIO $ ioError (userError (pvLabel variant ++ ": empty module graph"))
    let compileExecutable = do
          -- One shared outer logger lock protects the request's diagnostic
          -- hooks for every acquired lowering/recovery task. Per-task logger
          -- locks would still race the same underlying collector.
          currentLogger <- hsc_logger <$> getSession
          loweringLogger <- liftIO (makeThreadSafe currentLogger)
          -- These summed phases cover deferred finalization only. Canonical
          -- frontend work performed by the load hooks belongs to 'ghc_load'.
          tcMsRef <- liftIO (newIORef (0 :: Integer))
          loweringMsRef <- liftIO (newIORef (0 :: Integer))
          -- Deferred desugaring and simplification retain their summed wall
          -- counters; per-module detail spans identify the actual GHC work.
          dsMsRef  <- liftIO (newIORef (0 :: Integer))
          c2cMsRef <- liftIO (newIORef (0 :: Integer))
          frontCountRef <- liftIO (newIORef (0 :: Int))
          backCountRef <- liftIO (newIORef (0 :: Int))
          preparedCountRef <- liftIO (newIORef (0 :: Int))
          -- Per-module wall time: front (typecheck +
          -- desugar) and back (core2core) halves keyed by module name and SUMMED
          -- into one entry per module via 'Map.insertWith'. The compile summary uses
          -- the top three; detailed per-module output remains timing-gated.
          moduleMsRef <- liftIO (newIORef (Map.empty :: Map.Map String Integer))
          interfaceMsRef <- liftIO (newIORef (0 :: Integer))
          moduleInterfaceMsRef <- liftIO (newIORef (Map.empty :: Map.Map String Integer))
          targetModName' <- liftIO (targetModuleNameFor path)
          let targetModName = moduleNameString targetModName'
              recordInterface modSum = \case
                Nothing -> pure ()
                Just ms -> do
                  let name = moduleNameString (ms_mod_name modSum)
                  modifyIORef' interfaceMsRef (+ ms)
                  modifyIORef' moduleInterfaceMsRef (Map.insertWith (+) name ms)
                  modifyIORef' moduleMsRef (Map.insertWith (+) name ms)
              -- Deferred source modules use the same canonical policy. Load
              -- captures are consumed directly and never enter this frontend.
              compileFront modSum0 = do
                liftIO (modifyIORef' frontCountRef (+ 1))
                let modSum = canonicalSummary modSum0
                typecheckEnv <- getSession
                ((tcGblEnv, quasiQuoteUse), tcMs) <- timeSection $
                  timeDetailPhase timing "typecheck" (moduleNameString (ms_mod_name modSum)) $ do
                    parsed <- parseModule modSum
                    let quotes = classifyQuasiQuoteUse parsed
                    transformed <- liftIO (pvTransformParsed variant typecheckEnv modSum parsed)
                    ((tcg, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName typecheckEnv modSum parsed
                      (typecheckNativeModuleWithDiagnostics typecheckEnv transformed)
                    let flags = ms_hspp_opts modSum
                    liftIO (printOrThrowDiagnostics (hsc_logger typecheckEnv)
                      (initPrintConfig flags) (initDiagOpts flags) warnings)
                    familyEnvironment <- getSession
                    liftIO (validateCompilationFamilies familyEnvironment tcg)
                    pure (tcg, quotes)
                liftIO (reuseEvent SourceFrontend ReuseWork Absent modSum)
                when (ms_mod_name modSum == targetName) $
                  liftIO (writeIORef targetEnvironmentRef (Just tcGblEnv))
                liftIO (modifyIORef' tcMsRef (+ tcMs))
                hscEnv0 <- getSession
                (hscEnv, typedSegment) <- liftIO (prepareTypedTarget modSum
                  (scopeRetainedSummaryHscEnv modSum hscEnv0) tcGblEnv)
                setSession hscEnv
                let -- Capture the inferred type of the eval's top expression NOW,
                    -- before optimization can inline/rename @__user@ away. Types
                    -- live on the Id in the typechecked type env; the prepared wire program erases
                    -- them after compiler-owned decisions.
                    capturedType = capturedBindingDisplay evalUserBinder tcGblEnv
                    checkedBinderPins = capturedCellBinderPins hscEnv tcGblEnv
                    -- 'cpResultBinders' is the @result@-vs-@__result@ convention:
                    -- the one-shot eval wrapper names @result@ while resident-turn
                    -- templates use the scaffold-reserved @__result@.
                    mResTy   = foldr (<|>) Nothing
                                 [ capturedBindingType occ tcGblEnv
                                 | occ <- cpResultBinders plan ]
                packageRoots <- if captureProducts || isJust mMemoRef
                  then liftIO (directPackageImports hscEnv tcGblEnv)
                  else pure emptyPackageImports
                when (ms_mod_name modSum == targetModName') $ liftIO $ hPutStrLn stderr $
                  "tidepool-target phase=desugar module=" ++ targetModName
                dependentFiles <- liftIO (readIORef (tcg_dependent_files tcGblEnv))
                when (timing && not (null dependentFiles)) $ liftIO $ hPutStrLn stderr $
                  "tidepool-dependent-files module=" ++ moduleNameString (ms_mod_name modSum)
                    ++ " files=" ++ show dependentFiles
                let resultRoots = [idName identifier | cpKeepPrivateResult plan, ms_mod_name modSum == targetModName'
                      , identifier <- typeEnvIds (tcg_type_env tcGblEnv)
                      , occNameString (nameOccName (idName identifier)) `elem` cpResultBinders plan
                      , nameModule_maybe (idName identifier) == Just (ms_mod modSum)]
                liftIO $ modifyIORef' (tcg_keep tcGblEnv) (`extendNameSetList` resultRoots)
                (desugared0, dsMs) <- timeSection $
                  timeDetailPhase timing "deferred_desugar" (moduleNameString (ms_mod_name modSum)) $
                    liftIO (hscDesugar hscEnv modSum tcGblEnv >>= installSegment hscEnv typedSegment)
                let retainResult identifier = if idName identifier `elem` resultRoots
                      then setIdExported identifier else identifier
                    retainBinding (NonRec identifier rhs) = NonRec (retainResult identifier) rhs
                    retainBinding (Rec bindings) = Rec [(retainResult identifier,rhs) | (identifier,rhs) <- bindings]
                    desugared = desugared0 {mg_binds = map retainBinding (mg_binds desugared0)}
                liftIO (modifyIORef' loweringMsRef (+ dsMs))
                liftIO (modifyIORef' dsMsRef (+ dsMs))
                liftIO (modifyIORef' moduleMsRef
                          (Map.insertWith (+) (moduleNameString (ms_mod_name modSum)) (tcMs + dsMs)))
                pure ModuleFront { mfSummary    = modSum
                                 , mfHscEnv     = hscEnv
                                 , mfTcGblEnv   = tcGblEnv
                                 , mfPackageImports = packageRoots
                                 , mfDesugared  = desugared
                                 , mfCapturedType = capturedType
                                 , mfCheckedBinderPins = checkedBinderPins
                                 , mfResultType = mResTy
                                 , mfReferencedModules = moduleRefs desugared
                                 , mfQuasiQuoteUse = quasiQuoteUse
                                 , mfHasDependentFiles = not (null dependentFiles) }
              -- The per-module back half: the optimized-Core pass, the
              -- shared interface registration, then stable name externalization.
              -- Interface construction and prepared lowering share the same tidy
              -- result; the memo retains the interface alongside its prepared body.
              compileBack interfaceUse mf = do
                liftIO (modifyIORef' backCountRef (+ 1))
                (simplified, coreMs) <- timeSection $
                  timeDetailPhase timing "deferred_simplify" (moduleNameString (ms_mod_name (mfSummary mf))) $
                    liftIO $ do
                      plugins <- readIORef (tcg_th_coreplugins (mfTcGblEnv mf))
                      hscSimplify (mfHscEnv mf) plugins (mfDesugared mf)
                liftIO (modifyIORef' loweringMsRef (+ coreMs))
                liftIO (modifyIORef' c2cMsRef (+ coreMs))
                liftIO (modifyIORef' moduleMsRef
                          (Map.insertWith (+) (moduleNameString (ms_mod_name (mfSummary mf))) coreMs))
                liftIO (reuseEvent FinalizedCore ReuseWork Absent (mfSummary mf))
                liftIO (reuseEvent Interface ReuseWork Absent (mfSummary mf))
                let interfaceReuse = if isJust mMemoRef then MemoMiss else MemoDisabled
                (interfaceMs, registration) <- registerPreparedInterface timing requestIdentity interfaceReuse
                  interfaceUse mf simplified
                liftIO $ recordInterface (mfSummary mf) (Just interfaceMs)
                let externalized = externalizeInternalTops simplified
                pure
                  ( simplified
                  , ModuleOutput
                      { moduleOutputModule = mg_module externalized
                      , moduleOutputBinds = mg_binds externalized
                      , moduleOutputCapturedType = mfCapturedType mf
                      , moduleOutputCheckedBinderPins = mfCheckedBinderPins mf
                      , moduleOutputResultType = mfResultType mf
                      }
                  , registration
                  )
              acquireFinalized siteEnvironment loaded = do
                current <- getSession
                let summary = canonicalSummary (loadedSummary loaded)
                    env = (scopeRetainedSummaryHscEnv summary current)
                      { hsc_logger = loweringLogger }
                    cgGuts = finalizedTidyGuts (loadedFinalized loaded)
                    ownedSiblings = resolvePreparedSiblings (cg_binds cgGuts)
                    importedSiblings = resolvePreparedInterfaceSiblings env
                known <- liftIO (readIORef preparedSiblingsRef)
                let siblings = Map.unions [ownedSiblings, known, importedSiblings]
                liftIO (acquirePreparedModuleWithSiteEnvironment siteEnvironment env (ms_location summary) siblings
                  (loadedFinalized loaded))
              finalizeCurrent interfaceUse summary = do
                captured <- liftIO (Map.lookup (ms_mod summary) <$> readIORef loadedModulesRef)
                case captured of
                  Just loaded -> pure loaded
                  Nothing -> do
                    front <- compileFront summary
                    (_, output, finalized) <- compileBack interfaceUse front
                    facts <- liftIO (frontFacts front)
                    pure (LoadedModule (mfSummary front) facts output
                      finalized)
              rememberFinalized loaded = liftIO (publishLoadedFinalization loaded)
              observeCandidate modSum candidate = do
                let name = ms_mod_name modSum
                hmi <- case lookupHpt (hsc_HPT certifiedEnv) name of
                  Just value -> pure value
                  Nothing -> liftIO $ ioError $ userError
                    "certified candidate interface absent during module loop"
                recordValidity modSum True
                when captureProducts $ liftIO $
                  modifyIORef' productInterfacesRef (Map.insert name (hm_iface hmi))
                pure (HydratedObservation modSum hmi candidate)
              freezeSiblings observations = liftIO $ writeIORef preparedSiblingsRef (Map.unions
                [ resolvePreparedSiblings (cg_binds (finalizedTidyGuts finalized))
                | observation <- observations
                , finalized <- case observation of
                    LoadedObservation loaded -> [loadedFinalized loaded]
                    CachedObservation summary entry ->
                      [loadedFinalized (payloadLoaded summary (gmePayload entry))]
                    HydratedObservation{} -> [] ])
              acquirePreparationEnvironment observations = do
                freezeSiblings observations
                env <- getSession
                siteEnvironment <- liftIO (resolvePreparedSiteEnvironment env)
                siblings <- liftIO (readIORef preparedSiblingsRef)
                let preparedNormallyReusable _ product' = preparedSiteDependenciesMatch
                      siteEnvironment siblings (productPrepared product')
                    preparedReusable entry product' = not bodyReuseDisabled
                      && preparedNormallyReusable entry product'
                selectedVersions <- liftIO (readIORef selectedVersionsRef)
                let selectProduct observation = case observation of
                      CachedObservation summary entry ->
                        let matching = Map.lookup (ms_mod summary) selectedVersions >>= \node ->
                              find (preparedNormallyReusable entry) (completedModuleProducts node)
                        in case matching of
                          Just product' -> CachedObservation summary (entry {gmePayload=ExecutableProduct product'})
                          Nothing -> observation
                      _ -> observation
                    selectedObservations = map selectProduct observations
                forM_ selectedObservations $ \observation -> case observation of
                  CachedObservation summary entry -> forM_ (payloadProduct (gmePayload entry)) $ \product' ->
                    when (preparedUsesSiteAuthority (productPrepared product')) $ liftIO $ do
                      let verifiable = preparedSiteDependenciesEquivalent (productPrepared product') (productPrepared product')
                          decision | not verifiable = ReuseDisabled
                                   | preparedNormallyReusable entry product' = ReuseHit
                                   | otherwise = ReuseMiss
                          reason | not verifiable = CacheDisabled
                                 | preparedNormallyReusable entry product' = Matched
                                 | otherwise = ChangedAuthority
                      reuseEvent SiteWitness decision reason summary
                  _ -> pure ()
                liftIO (emitReuseComplete timing reuseContext SiteWitness)
                pure (siteEnvironment,preparedNormallyReusable,preparedReusable,selectedObservations)
              acquireCompletion observations tasks = case completionFactoryFor selection of
                Nothing -> pure (PreparedModuleObserver (\_ -> pure ()) (\_ -> pure ()))
                Just factory -> do
                  env <- getSession
                  interfaces <- liftIO (readIORef productInterfacesRef)
                  siblings <- liftIO (readIORef preparedSiblingsRef)
                  externalBindings <- liftIO (availableOriginalBindings env)
                  let taskNames = Set.fromList (map fst tasks)
                      sourceOwners = Set.fromList
                        [ ms_mod summary | observation <- observations
                        , let summary = observationSummary observation
                        , ms_mod_name summary `Set.member` taskNames ]
                      externalOriginals = Set.fromList
                        ([binder | scope <- maybe [] pure (pvExactScope variant)
                          , product' <- scopeAvailableOriginalProducts scope, group <- originalGroups product'
                          , binder <- originalBinders group]
                         ++ [binder | admitted <- Map.elems acceptedCandidates
                            , group <- candidateGroups (admittedCandidateOriginal admitted)
                            , binder <- candidateGroupBinders group])
                      inputs = PreparedModuleCompletionInputs sourceOwners siblings externalOriginals externalBindings reuseContext
                  liftIO (factory (env {hsc_logger = loweringLogger}) interfaces
                    targetOwner (pvExactScope variant) inputs)
              lowerTasks preparedNormallyReusable observer tasks = liftIO $ timePhase timing "prepared_graph" $ do
                let summariesByName = Map.fromList [(ms_mod_name summary,summary) | summary <- summaries]
                    lower input = do
                      forM_ (Map.lookup (fst input) summariesByName) $ \summary -> case snd input of
                        Right _ -> reuseEvent PreparedBody ReuseHit Matched summary
                        Left _ -> do
                          -- Each task's actual cache refusal is determined by
                          -- the frozen typed site witness or executable closure.
                          cached <- maybe (pure Nothing) (fmap (Map.lookup (ms_mod_name summary)) . readIORef) mMemoRef
                          disabled <- Set.member (ms_mod summary) <$> readIORef disabledPreparedOwnersRef
                          let reason | disabled = CacheDisabled
                                     | otherwise = case cached >>= payloadProduct . gmePayload of
                                         Just product' | Just entry <- cached
                                           , not (preparedNormallyReusable entry product') -> ChangedAuthority
                                         Just _ -> ChangedDependency
                                         Nothing -> Absent
                          reuseEvent PreparedBody (if disabled then ReuseDisabled else ReuseMiss) reason summary
                          reuseEvent PreparedBody ReuseWork reason summary
                      prepared <- either runPreparedModuleTask pure (snd input)
                      observePreparedModule observer prepared
                      pure prepared
                    lowerWith executor = runCompilerTasks executor lower
                      (\_ prepared -> completedPreparedModule observer prepared) tasks
                case executorFor selection of
                  Just executor -> lowerWith executor
                  Nothing -> withCompilerExecutor executionGrant lowerWith
          let interfaceUses = zipWith homeInterfaceUse summaries (homeInterfaceConsumers summaries)
          (observations, results, preparedModules, mReachable) <- case bodyTier of
            OptimizeEveryModule -> do
              finalizedObservations <- forM (zip summaries interfaceUses) $ \(modSum, interfaceUse) -> do
                cpBeforeModule plan modSum
                let name = ms_mod_name modSum
                case Map.lookup name acceptedCandidates of
                  Just candidate -> do
                    observation <- observeCandidate modSum candidate
                    recordExecutableValidity modSum True
                    pure observation
                  Nothing -> do
                    cached <- lookupValidMemo modSum
                    case cached of
                      Just entry | interfaceReady interfaceUse modSum entry -> do
                        recordValidity modSum True
                        rememberFinalized (payloadLoaded modSum (gmePayload entry))
                        when (needsPreparedInterface interfaceUse) $
                          forM_ (cachedInterface modSum entry) (installPreparedInterface name)
                        pure (CachedObservation modSum entry)
                      _ -> do
                        recordValidity modSum False
                        loaded <- finalizeCurrent interfaceUse modSum
                        rememberFinalized loaded
                        pure (LoadedObservation loaded)
              (siteEnvironment,preparedNormallyReusable,preparedReusable,observations') <- acquirePreparationEnvironment finalizedObservations
              tasks <- case preparation of
                CheckOnly -> pure []
                PrepareStg -> fmap catMaybes $ forM observations' $ \observation -> do
                  let name = ms_mod_name (observationSummary observation)
                  case observation of
                    HydratedObservation{} -> pure Nothing
                    CachedObservation summary entry -> case payloadProduct (gmePayload entry) of
                      Just product' | preparedReusable entry product' -> do
                        recordExecutableValidity summary True
                        pure (Just (name, Right (productPrepared product')))
                      _ -> do
                        when bodyReuseDisabled $ forM_ (payloadProduct (gmePayload entry)) $ \product' ->
                          when (preparedNormallyReusable entry product') $ liftIO $
                            modifyIORef' disabledPreparedOwnersRef (Set.insert (ms_mod summary))
                        recordExecutableValidity summary False
                        liftIO (modifyIORef' preparedCountRef (+ 1))
                        task <- acquireFinalized siteEnvironment (payloadLoaded summary (gmePayload entry))
                        pure (Just (name, Left task))
                    LoadedObservation loaded -> do
                      recordExecutableValidity (loadedSummary loaded) False
                      liftIO (modifyIORef' preparedCountRef (+ 1))
                      task <- acquireFinalized siteEnvironment loaded
                      pure (Just (name, Left task))
              completed <- acquireCompletion observations' tasks
              preparedResults <- lowerTasks preparedNormallyReusable completed tasks
              let preparedByName = Map.fromList (zip (map fst tasks) preparedResults)
                  collectPrepared loaded = do
                    let summary = loadedSummary loaded
                        output = loadedOutput loaded
                        finalized = loadedFinalized loaded
                        prepared = Map.lookup (ms_mod_name summary) preparedByName
                    moduleProduct <- requireProduct (loadedFacts loaded) output prepared finalized
                    forM_ mMemoRef $ \ref -> liftIO (modifyIORef' ref
                      (Map.insert (ms_mod_name summary) (GutsMemoEntry
                        (memoValidity summary finalized)
                        (ExecutableProduct moduleProduct) requestIdentity
                        (memoDiagnosticWitnesses summary))))
                    pure (Just output, prepared)
              pairs <- forM observations' $ \observation -> case observation of
                HydratedObservation{} -> pure (Nothing, Nothing)
                CachedObservation summary entry -> collectPrepared (payloadLoaded summary (gmePayload entry))
                LoadedObservation loaded -> collectPrepared loaded
              pure (observations', [output | (Just output, _) <- pairs],
                    [prepared | (_, Just prepared) <- pairs], Nothing)
            OptimizeCoreReachable -> do
              -- Install every valid finalized owner before checking any later
              -- importer. Reference facts select STG only after this pass;
              -- unprepared owners retain their same interface/Core pair.
              finalizedObservations <- forM (zip summaries interfaceUses) $ \(modSum, interfaceUse) -> do
                cpBeforeModule plan modSum
                case Map.lookup (ms_mod_name modSum) acceptedCandidates of
                  Just candidate -> observeCandidate modSum candidate
                  Nothing -> do
                    cached <- lookupValidMemo modSum
                    case cached of
                      Just entry
                        | interfaceReady interfaceUse modSum entry -> do
                        let loaded = payloadLoaded modSum (gmePayload entry)
                        recordValidity modSum True
                        rememberFinalized loaded
                        when (needsPreparedInterface interfaceUse) $
                          installPreparedInterface (ms_mod_name modSum)
                            (finalizedHomeModInfo (loadedFinalized loaded))
                        pure (CachedObservation modSum entry)
                      _ -> do
                        recordValidity modSum False
                        loaded <- finalizeCurrent interfaceUse modSum
                        rememberFinalized loaded
                        pure (LoadedObservation loaded)
              -- Sibling identities precede lowering even for native cycles.
              (siteEnvironment,preparedNormallyReusable,preparedReusable,observations') <- acquirePreparationEnvironment finalizedObservations
              facts <- liftIO (mapM observationFacts observations')
              -- Fresh Core and admitted native outlines retain defining owners,
              -- including dictionaries and reexports. All source owners have
              -- been checked; this closure selects only fresh STG work.
              forceValidationOnly <- liftIO (lookupEnv "TIDEPOOL_TEST_FORCE_VALIDATION_ONLY")
              let referencesByMod = Map.fromList
                    [ (ms_mod_name (observationSummary observation), Set.fromList
                        [moduleName owner | owner <- Set.toList (moduleFactReferences fact)
                          , isHomeUnit (hsc_home_unit certifiedEnv) (moduleUnit owner)])
                    | (observation, fact) <- zip observations' facts ]
                  -- Source validity still covers all imports. Prepared reuse
                  -- additionally needs the defining home bodies referenced by
                  -- the same Core graph that selects STG, including dictionaries
                  -- and names reached through reexports. Compile-time and type-only
                  -- imports need their canonical facts, but no prepared body.
                  -- Freeze reuse before tasks run. A native reference cycle
                  -- may be reusable as a whole; source boot/TH scheduling stays
                  -- with GHC. Removing locally invalid owners to a fixed point
                  -- prevents task completion order from deciding validity.
                  locallyReusable = Set.fromList
                    [ ms_mod_name (observationSummary observation)
                    | (observation, interfaceUse) <- zip observations' interfaceUses
                    , case observation of
                        HydratedObservation{} -> True
                        CachedObservation summary entry ->
                          case payloadProduct (gmePayload entry) of
                            Just product' -> preparedNormallyReusable entry product'
                              && interfaceReady interfaceUse summary entry
                            Nothing -> False
                        LoadedObservation{} -> False ]
                  normalReusableOwners = dependencyClosedReuse referencesByMod locallyReusable
                  reusableOwners
                    | bodyReuseDisabled = Set.fromList
                        [ms_mod_name (observationSummary observation) | observation@HydratedObservation{} <- observations']
                    | otherwise = normalReusableOwners
                  executableDepsValid modSum = pure $ case
                      Map.lookup (ms_mod_name modSum) referencesByMod of
                    Nothing -> False
                    Just referenced -> Set.delete (ms_mod_name modSum) referenced
                      `Set.isSubsetOf` reusableOwners
                  reachableMods0 = reachableModuleClosure targetModName' referencesByMod
                  -- A focused fault-injection test can omit one real reachable
                  -- owner from STG preparation without changing its finalization.
                  reachableMods = case forceValidationOnly of
                    Just m  -> Set.delete (mkModuleName m) reachableMods0
                    Nothing -> reachableMods0
              -- All live Session work finishes before independent lowering.
              -- Frozen reuse and reachability select exactly the required jobs.
              tasks <- case preparation of
                CheckOnly -> pure []
                PrepareStg -> fmap catMaybes $ forM observations' $ \observation -> do
                  let summary = observationSummary observation
                      name = ms_mod_name summary
                  if name `Set.notMember` reachableMods then pure Nothing
                    else case observation of
                      HydratedObservation{} -> pure Nothing
                      CachedObservation _ entry | name `Set.member` reusableOwners ->
                        case payloadProduct (gmePayload entry) of
                          Just product' -> pure (Just (name, Right (productPrepared product')))
                          Nothing -> liftIO (ioError (userError "reused executable product is absent"))
                      LoadedObservation loaded -> do
                        liftIO (modifyIORef' preparedCountRef (+ 1))
                        task <- acquireFinalized siteEnvironment loaded
                        pure (Just (name, Left task))
                      CachedObservation _ entry -> do
                        when (bodyReuseDisabled && name `Set.member` normalReusableOwners) $ liftIO $
                          modifyIORef' disabledPreparedOwnersRef (Set.insert (ms_mod summary))
                        liftIO (modifyIORef' preparedCountRef (+ 1))
                        task <- acquireFinalized siteEnvironment (payloadLoaded summary (gmePayload entry))
                        pure (Just (name, Left task))
              completed <- acquireCompletion observations' tasks
              preparedResults <- lowerTasks preparedNormallyReusable completed tasks
              let preparedByName = Map.fromList (zip (map fst tasks) preparedResults)
                  prepareReachable loaded = case preparation of
                    CheckOnly -> pure Nothing
                    PrepareStg -> case Map.lookup (ms_mod_name (loadedSummary loaded)) preparedByName of
                      Just prepared -> pure (Just prepared)
                      Nothing -> liftIO (ioError (userError "selected prepared module task is absent"))
                  rememberExecutable modSum output prepared finalized moduleFacts = do
                    moduleProduct <- requireProduct moduleFacts output prepared finalized
                    case mMemoRef of
                      Just ref -> liftIO (modifyIORef' ref
                        (Map.insert (ms_mod_name modSum)
                          (GutsMemoEntry
                            (memoValidity modSum finalized)
                            (ExecutableProduct moduleProduct)
                            requestIdentity
                            (memoDiagnosticWitnesses modSum))))
                      Nothing -> pure ()
                  compileReachable loaded moduleFacts = do
                    let modSum = loadedSummary loaded
                    rememberFinalized loaded
                    let r = loadedOutput loaded
                        finalized = loadedFinalized loaded
                    prepared <- prepareReachable loaded
                    rememberExecutable modSum r prepared finalized moduleFacts
                    pure [(r, prepared)]
                  validationOnly loaded moduleFacts = do
                    let modSum = loadedSummary loaded
                    case mMemoRef of
                      Just ref -> liftIO (modifyIORef' ref
                        (Map.insert (ms_mod_name modSum)
                          (GutsMemoEntry
                            (memoValidity modSum (loadedFinalized loaded))
                            (ValidationOnly moduleFacts (loadedOutput loaded) (loadedFinalized loaded))
                            requestIdentity
                            (memoDiagnosticWitnesses modSum))))
                      Nothing -> pure ()
              rs <- fmap concat $ forM (zip3 observations' facts interfaceUses) $ \(observation, moduleFacts, interfaceUse) -> do
                let modSum = observationSummary observation
                depsExecutable <- executableDepsValid modSum
                -- Source facts were validated in the first pass. Executable
                -- reuse also requires the exact dependency bodies and interfaces
                -- seen when this entry was prepared.
                recordExecutableValidity modSum False
                if ms_mod_name modSum `Set.member` reachableMods
                  then case observation of
                    -- Reuse a prepared body only with its current dependency
                    -- bodies and the canonical interface admitted above.
                    CachedObservation _ entry
                      | depsExecutable
                      , Just moduleProduct <- payloadProduct (gmePayload entry)
                      , preparedReusable entry moduleProduct
                      , interfaceReady interfaceUse modSum entry -> do
                        recordExecutableValidity modSum True
                        when captureProducts $ liftIO $
                          modifyIORef' productInterfacesRef (Map.insert (ms_mod_name modSum)
                            (hm_iface (finalizedHomeModInfo (productFinalized moduleProduct))))
                        when (needsPreparedInterface interfaceUse) $
                          forM_ (cachedInterface modSum entry) (installPreparedInterface (ms_mod_name modSum))
                        pure [(productOutput moduleProduct, Just (productPrepared moduleProduct))]
                    CachedObservation _ entry -> do
                      let reason
                            | not depsExecutable = "dependency-executable-regenerated"
                            | isNothing (payloadProduct (gmePayload entry)) =
                                "validation-only-promoted"
                            | Just product' <- payloadProduct (gmePayload entry)
                            , not (preparedReusable entry product') = "site-authority-request-changed"
                            | otherwise = "required-interface-not-retained"
                      memoMiss modSum reason
                      memoMissTrace modSum reason (Just entry)
                      compileReachable (payloadLoaded modSum (gmePayload entry)) moduleFacts
                    LoadedObservation loaded -> compileReachable loaded moduleFacts
                    HydratedObservation _ _ _ -> do
                      recordExecutableValidity modSum True
                      pure []
                  -- Keep an existing executable only while its dependencies
                  -- and required interface still match. An unreachable source
                  -- needs validation facts, so preparing a new body here would
                  -- run compiler actions without contributing to this request.
                  else case observation of
                    CachedObservation _ entry
                      | depsExecutable
                      , Just moduleProduct <- payloadProduct (gmePayload entry)
                      , preparedReusable entry moduleProduct
                      , interfaceReady interfaceUse modSum entry -> do
                        recordExecutableValidity modSum True
                        when (needsPreparedInterface interfaceUse) $
                          forM_ (cachedInterface modSum entry) (installPreparedInterface (ms_mod_name modSum))
                        pure []
                    CachedObservation _ entry ->
                      validationOnly (payloadLoaded modSum (gmePayload entry)) moduleFacts >> pure []
                    LoadedObservation loaded -> validationOnly loaded moduleFacts >> pure []
                    HydratedObservation _ _ _ -> do
                      recordExecutableValidity modSum True
                      pure []
              pure (observations', map fst rs, [p | (_, Just p) <- rs], Just reachableMods)
          totalTcMs   <- liftIO (readIORef tcMsRef)
          totalLoweringMs <- liftIO (readIORef loweringMsRef)
          liftIO (emitPhase timing "typecheck" totalTcMs)
          liftIO (emitPhase timing "lowering" totalLoweringMs)
          summaryT1 <- monotonicTime
          liftIO $ do
            frontCount <- readIORef frontCountRef
            backCount <- readIORef backCountRef
            moduleTimes <- readIORef moduleMsRef
            interfaceTotal <- readIORef interfaceMsRef
            moduleInterfaces <- readIORef moduleInterfaceMsRef
            let topModules = take 3 (sortOn (negate . snd) (Map.toList moduleTimes))
                topInterfaces = take 3 (sortOn (negate . snd) (Map.toList moduleInterfaces))
            emitPhase timing "module_interface" interfaceTotal
            emitCompileSummary (length summaries) (elapsedMs sessionT0 summaryT1)
              totalTcMs totalLoweringMs interfaceTotal frontCount backCount (Map.size moduleInterfaces) topModules topInterfaces
            emitModuleTiming timing (sortOn (negate . snd) (Map.toList moduleTimes))
              (sortOn (negate . snd) (Map.toList moduleInterfaces))
          -- These sums cover only deferred GHC passes. Canonical load passes
          -- have their own detail spans, and parallel detail wall time overlaps.
          liftIO $ do
            readIORef dsMsRef >>= emitPhase timing "deferred_desugar"
            readIORef c2cMsRef >>= emitPhase timing "deferred_simplify"
          case mReachable of
            Just reachableMods | timing -> liftIO $ do
              totalDsMs  <- readIORef dsMsRef
              totalC2cMs <- readIORef c2cMsRef
              frontCount <- readIORef frontCountRef
              backCount <- readIORef backCountRef
              preparedCount <- readIORef preparedCountRef
              let allModNames = map (ms_mod_name . observationSummary) observations
                  moduleCount = length allModNames
                  reachableCount = Set.size reachableMods
                  validationOnly = [ moduleNameString m | m <- allModNames, not (m `Set.member` reachableMods) ]
              hPutStrLn stderr $
                "e6-tier modules=" ++ show moduleCount
                ++ " reachable=" ++ show reachableCount
                ++ " desugar_ms=" ++ show totalDsMs
                ++ " core2core_ms=" ++ show totalC2cMs
                ++ " front_compiles=" ++ show frontCount
                ++ " core_compiles=" ++ show backCount
                ++ " prepared_compiles=" ++ show preparedCount
                ++ " validation_only=" ++ show validationOnly
                ++ " reachable_names=" ++ show (map moduleNameString (Set.toList reachableMods))
            _ -> pure ()
          timePhase timing "merge_barrier" $ cpBeforeMerge plan NativeMergeBoundary
          -- Merge: dependency module bindings first, target module last
          let isTargetMod output =
                moduleNameString (moduleName (moduleOutputModule output)) == targetModName
          (targetOutput, depOutputs, capturedType, checkedBinderPins, resultTy) <-
            case filter isTargetMod results of
            (targetResult:_) ->
              return
                ( targetResult
                , [output | output <- results
                    , moduleOutputModule output /= moduleOutputModule targetResult]
                , moduleOutputCapturedType targetResult
                , moduleOutputCheckedBinderPins targetResult
                , moduleOutputResultType targetResult
                )
            []      -> liftIO $ ioError $ userError $
              pvLabel variant ++ ": target module '" ++ targetModName
              ++ "' not found among compiled modules: "
              ++ show (map (moduleNameString . moduleName . moduleOutputModule) results)
          -- 'allTyCons' unconditionally covers EVERY compiled module, not just the
          -- tier's reachable set: TyCon/DataCon declarations are populated by the
          -- typechecker and are never touched by 'core2core' (which transforms
          -- 'mg_binds' only — the back half never re-derives 'mg_tcs'), so reading
          -- them off the DESUGARED guts costs nothing extra and keeps a
          -- validation-only module's data types available to metadata validation.
          -- Order is summary order, which on the session variant (topologically
          -- sorted over the target's own import closure, so the target is last) is
          -- the dependencies-then-target order it used to build by hand.
          moduleFacts <- liftIO (mapM observationFacts observations)
          hscFinal0 <- getSession
          hscFinal <- liftIO (includeTypedSession hscFinal0)
          setSession hscFinal
          exactTyCons <- liftIO (exactInterfaceTyCons hscFinal (pvExactScope variant))
          let allBinds  = concatMap moduleOutputBinds depOutputs
                ++ moduleOutputBinds targetOutput
              allTyCons = concatMap moduleFactTyCons moduleFacts ++ exactTyCons
          targetEnvironment <- liftIO (readIORef targetEnvironmentRef) >>= maybe
            (liftIO (ioError (userError (pvLabel variant ++ ": target frontend environment is absent")))) pure
          warnings <- liftIO (nub . reverse <$> readIORef warnRef)
          injectedBaseline <- liftIO (cpInjectedSessionInterfaces plan)
          injectedTyped <- liftIO (readIORef typedSessionInterfacesRef)
          let pipelineResult = PipelineResult
                { prBinds  = allBinds
                , prTyCons = allTyCons
                , prHscEnv = (cpFinalEnv plan hscFinal) {hsc_logger = loweringLogger}
                , prInjectedSessionInterfaces = injectedBaseline
                , prProducedSessionInterfaces = injectedTyped
                , prCanonicalInterfaceAdmissions = maybe Map.empty scopeCanonicalInterfaces
                    (pvExactScope variant)
                , prCapturedType = capturedType
                , prCheckedBinderPins = checkedBinderPins
                , prResultType   = resultTy
                , prWarnings     = nub (map fst warnings)
                , prTargetRdrEnv = tcg_rdr_env targetEnvironment
                , prTargetTcGblEnv = targetEnvironment
                }
          capturedSources <- liftIO (captureDependencySources modGraphRaw)
          dependencies <- liftIO (dependencyEvidenceFor hscFinal capturedSources freshGraph
            (zip (map (ms_mod_name . observationSummary) observations) moduleFacts))
          productInterfaces <- liftIO (readIORef productInterfacesRef)
          finalizedModules <- liftIO (readIORef finalizedModulesRef)
          let packageRoots = Map.union
                (Map.fromList [(ms_mod_name (observationSummary observation), moduleFactPackageImports facts)
                  | (observation, facts) <- zip observations moduleFacts])
                (Map.fromList [(ms_mod_name (loadedSummary loaded), moduleFactPackageImports (loadedFacts loaded))
                  | loaded <- Map.elems loadedModules])
          forM_ (Set.toAscList (Map.keysSet finalizedModules `Set.difference` Map.keysSet packageRoots)) $
            liftIO . throwIO . MissingFinalizedFacts
          pure (pipelineResult, preparedModules, dependencies, productInterfaces, finalizedModules, packageRoots)
    let compileChecked :: Ghc CheckedEnvironmentResult
        compileChecked = do
          -- Restore the full graph after any target deferral so checking sees
          -- every source instance, including dependencies loaded for TH.
          environment <- getSession
          when (isNothing (pvExactScope variant)) $
            setSession environment { hsc_mod_graph = modGraphRaw }
          -- The list is topologically ordered, so only an unprocessed source
          -- module can consume an interface we build here. Completed modules no
          -- longer consult the HPT, while the returned target environment owns
          -- its types and reader scope directly.
          unless (Map.null loadedModules) $ do
            hydrated <- getSession
            liftIO (validateEnvironmentFamilies hydrated)
          checkedFactsRef <- liftIO (newIORef [])
          checked <- forM (zip summaries (homeInterfaceConsumers summaries)) $ \(summary, laterConsumers) -> do
            cpBeforeModule plan summary
            current <- getSession
            let isTarget = ms_mod_name summary == targetName
                loaded = lookupHpt (hsc_HPT current) (ms_mod_name summary)
            let sourceFacts = loadedFacts <$> Map.lookup (ms_mod summary) loadedModules
            case sourceFacts of
              Just facts -> do
                liftIO (modifyIORef' checkedFactsRef ((ms_mod_name summary, facts) :))
                when timing $ liftIO $ hPutStrLn stderr $
                  (if ms_mod summary `Map.member` capturedModules
                    then "tidepool-checked-loaded-source module="
                    else "tidepool-checked-reused-source module=")
                    ++ moduleNameString (ms_mod_name summary)
                if isTarget
                  then do
                    tcg <- liftIO (readIORef targetEnvironmentRef) >>= maybe
                      (liftIO (ioError (userError "loaded checked target environment is absent"))) pure
                    pure (Just (tcg, capturedInspectionProbes tcg))
                  else pure Nothing
              Nothing -> if not isTarget && not (isNothing loaded)
                  && (isNothing (pvExactScope variant) || ms_mod_name summary `Map.member` acceptedCandidates)
                then do
                  forM_ loaded $ \hmi -> do
                    let facts = hydratedModuleFacts hmi (maybe emptyPackageImports admittedCandidateRoots
                          (Map.lookup (ms_mod_name summary) acceptedCandidates))
                    liftIO (modifyIORef' checkedFactsRef ((ms_mod_name summary, facts) :))
                  pure Nothing
                else do
                  liftIO $ hPutStrLn stderr $
                    "tidepool-checked module=" ++ moduleNameString (ms_mod_name summary)
                      ++ " target=" ++ show isTarget
                  (tcg, quotes) <- timeDetailPhase timing "checked_typecheck"
                    (moduleNameString (ms_mod_name summary)) $ do
                      parsed <- parseModule summary
                      let quotes = classifyQuasiQuoteUse parsed
                      transformed <- liftIO (pvTransformParsed variant current summary parsed)
                      ((checkedEnvironment, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName current summary parsed
                        (typecheckNativeModuleWithDiagnostics current transformed)
                      let flags = ms_hspp_opts summary
                      liftIO (printOrThrowDiagnostics (hsc_logger current)
                        (initPrintConfig flags) (initDiagOpts flags) warnings)
                      familyEnvironment <- getSession
                      liftIO (validateCompilationFamilies familyEnvironment checkedEnvironment)
                      pure (checkedEnvironment, quotes)
                  liftIO (reuseEvent SourceFrontend ReuseWork Absent summary)
                  let inspectionProbes = capturedInspectionProbes tcg
                      retainInterface reason = do
                        -- A later source module's normal home import resolves via
                        -- this HPT entry. A source-less Val interface injected before
                        -- a later module can also mention an earlier generated Lib
                        -- without importing it from source. SOURCE imports keep using
                        -- the boot iface installed by GHC's load phase, and no returned
                        -- metadata consumer reads the target back through HPT.
                        ambient <- getSession
                        let canonical = canonicalSummary summary
                            env = scopeRetainedSummaryHscEnv canonical ambient
                        details <- liftIO (mkBootModDetailsTc (hsc_logger env) tcg)
                        (iface, _ifaceMs) <- liftIO $ measureModuleInterface timing requestIdentity
                          (moduleNameString (ms_mod_name summary)) CheckedEnvironmentInterface HptMiss $
                            mkIfaceTc env Sf_None details canonical Nothing tcg
                        liftIO (reuseEvent Interface ReuseWork Absent summary)
                        let linkable = maybe emptyHomeModInfoLinkable hm_linkable
                              (lookupHpt (hsc_HPT env) (ms_mod_name summary))
                            hmi = HomeModInfo (set_mi_extra_decls Nothing iface) details linkable
                        setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name summary) hmi) ambient)
                        when timing $ liftIO $ hPutStrLn stderr $
                          "tidepool-checked-interface-retained module="
                            ++ moduleNameString (ms_mod_name summary) ++ reason
                  dependentFiles <- liftIO (readIORef (tcg_dependent_files tcg))
                  liftIO (modifyIORef' checkedFactsRef ((ms_mod_name summary, ModuleFacts
                    { moduleFactTyCons = typeEnvTyCons (tcg_type_env tcg)
                    , moduleFactReferences = Set.empty
                    , moduleFactPackageImports = emptyPackageImports
                    , moduleFactHasDependentFiles = not (null dependentFiles)
                    , moduleFactQuasiQuoteUse = quotes }) :))
                  case homeInterfaceUse summary laterConsumers of
                    HomeInterfaceLeaf -> when timing $ liftIO $ hPutStrLn stderr $
                      "tidepool-checked-interface-elided module="
                        ++ moduleNameString (ms_mod_name summary)
                        ++ " reason=no-later-home-importer"
                    HomeInterfaceNeededBy consumer ->
                      retainInterface (" consumer=" ++ moduleNameString consumer)
                    HomeInterfaceNeededForSessionInjection ->
                      retainInterface " reason=session-value-interface"
                  pure (if isTarget then Just (tcg, inspectionProbes) else Nothing)
          warnings <- liftIO (nub . reverse <$> readIORef warnRef)
          cpBeforeMerge plan CheckedReceiptBoundary
          case [(tcg, probes) | Just (tcg, probes) <- checked] of
            [(tcg, probes)] -> do
              env <- getSession
              valid <- liftIO (revalidateAcceptedCandidates (Map.elems acceptedCandidates))
              unless valid $ liftIO $ ioError $ userError
                "accepted metadata candidate changed before checked receipt"
              forM_ exactCompilation $ \compilation -> do
                captured <- liftIO (captureDependencySources modGraphRaw)
                facts <- liftIO (readIORef checkedFactsRef)
                evidence <- liftIO (dependencyEvidenceFor env captured freshGraph facts)
                liftIO (writeCheckedExactCompilation env compilation evidence)
              pure CheckedEnvironmentResult
                { crHscEnv = cpFinalEnv plan env
                , crTargetTcGblEnv = tcg
                , crTargetRdrEnv = tcg_rdr_env tcg
                , crInspectionProbes = probes
                , crResultType = foldr (<|>) Nothing [capturedBindingType name tcg | name <- cpResultBinders plan]
                , crCheckedBinderPins = capturedCellBinderPins (cpFinalEnv plan env) tcg
                , crWarnings = map snd warnings
                }
            _ -> liftIO $ ioError $ userError "metadata target missing from checked module graph"
    let finish :: PipelineSelection output -> Ghc output
        finish selected = case selected of
          PreparedStg -> do
            (result, modules, dependencies, productInterfaces, finalizedModules, packageRoots) <- compileExecutable
            capturedDependencies <- liftIO (preparedDependencies (prHscEnv result)
              (pvSourceImportIntents variant) dependencies exactCompilation)
            pure PreparedPipelineResult
              { pprPipelineResult = result
              , pprModules = modules
              , pprDependencies = capturedDependencies
              , pprProductInterfaces = productInterfaces
              , pprFinalizedModules = finalizedModules
              , pprPackageImports = packageRoots
              , pprAcceptedCandidates = []
              , pprOriginalBindings = Map.empty
              }
          PreparedProducts _ -> do
            (result, modules, dependencies, productInterfaces, finalizedModules, packageRoots) <- compileExecutable
            valid <- liftIO $ revalidateAcceptedCandidates (Map.elems acceptedCandidates)
            when (not valid) $ liftIO $ ioError $ userError
              "accepted module candidate changed before artifact publication"
            originalBindings <- liftIO (availableOriginalBindings (prHscEnv result))
            capturedDependencies <- liftIO (preparedDependencies (prHscEnv result)
              (pvSourceImportIntents variant) dependencies exactCompilation)
            pure PreparedPipelineResult
              { pprPipelineResult = result
              , pprModules = modules
              , pprDependencies = capturedDependencies
              , pprProductInterfaces = productInterfaces
              , pprFinalizedModules = finalizedModules
              , pprPackageImports = packageRoots
              , pprAcceptedCandidates = map admittedCandidateOriginal (Map.elems acceptedCandidates)
              , pprOriginalBindings = originalBindings
              }
          PreparedSegmentProducts _ candidatePath -> do
            products <- finish (PreparedProducts candidatePath)
            segment <- liftIO (readIORef targetTypedSegmentRef) >>= maybe
              (liftIO (throwIO MissingCaptureRoot)) pure
            pure (PreparedSegmentProductsResult products segment)
          CheckedEnvironment -> compileChecked
          CheckedEnvironmentProducts _ -> compileChecked
          WithCompilerExecution _ _ inner -> finish inner
          WithPreparedModuleCompletion _ inner -> finish inner
          WithTypedSegmentPreparation _ inner -> finish inner
    result <- finish selection
    liftIO $ do
      emitReuseComplete timing reuseContext SourceFrontend
      emitReuseComplete timing reuseContext Interface
      emitReuseComplete timing reuseContext FinalizedCore
      case preparation of
        PrepareStg | isNothing (completionFactoryFor selection) ->
          emitReuseComplete timing reuseContext PreparedBody
        PrepareStg -> pure ()
        CheckOnly -> emitCheckOnlyReuseApplicability timing reuseContext
    getSession >>= liftIO . recordCompilerInterpreter interpreterState selectedExecutables
    pure result

compilePurposeLabel :: CompilePurpose -> String
compilePurposeLabel purpose = case purpose of
  TypedSegmentCompile _ _ _ -> "typed_segment"
  GeneralCompile -> "general"
  LookupTypeCompile -> "lookup_type"
  CertifyHomeProductsCompile -> "certify_home_products"
  OriginalDeclarationCompile -> "original_declaration"
  CheckedItemCompile{} -> "checked_item"
  HostActivationPreviewCompile{} -> "activation_preview"
  ProgramItemCompile{} -> "program_item"
  PlannedDeclarationCheck{} -> "planned_declaration"
  ExactScopeCompile inner _ -> compilePurposeLabel inner
  GeneratedScaffoldCompile _ inner -> compilePurposeLabel inner
  GeneratedInstanceCheck _ inner -> compilePurposeLabel inner
  ParsedImportSelection _ inner -> compilePurposeLabel inner
  CompletedProgramImports _ inner -> compilePurposeLabel inner

-- | Hash every source and compare it with the fingerprint captured by GHC's
-- downsweep. A mismatch makes the evidence incomplete; publication re-hashes
-- the SHA-256 evidence once more before exposing the artifact bundle.
captureDependencySources :: ModuleGraph -> IO ([DependencySource], Bool)
captureDependencySources graph = do
  captured <- forM [summary | ModuleNode _ summary <- mgModSummaries' graph] $ \summary ->
    case ml_hs_file (ms_location summary) of
      Nothing -> pure (Nothing, False)
      Just source -> do
        absolute <- normalise <$> makeAbsolute source
        -- GHC's summary fingerprint covers the source read during downsweep.
        -- Both hashes come from this same read, so no race can associate the
        -- compiler fingerprint for one version with SHA-256 for another.
        (evidence, fingerprint) <- sourceEvidenceWithFingerprint absolute
        pure (Just evidence, fingerprint == ms_hs_hash summary)
  let complete = all (\(item, matchesSummary) -> isJust item && matchesSummary) captured
      unique = Map.fromList
        [ (dependencySourcePath evidence, evidence)
        | (Just evidence, _) <- captured
        ]
  pure (map snd (Map.toAscList unique), complete)

-- Candidates narrow work only. The downsweep chooses every current import;
-- source bytes, negative home selections, GHC's own recompilation decision,
-- and every interface in a closed home dependency set must agree in this
-- transaction before any source module can be skipped.
data CandidateLoading = CandidateInterfaceOnly | CandidateLoadForExecution
  deriving (Eq, Show)

data AdmittedSourceCandidate = AdmittedSourceCandidate
  { admittedCandidateOriginal :: ModuleCandidate
  , admittedCandidateRoots :: PackageImportEvidence
  , admittedCandidateLoading :: CandidateLoading
  , admittedCandidateProof :: CanonicalInterfaceProof
  , admittedCandidateView :: ModSummary
  }

data CandidateAdmissionReason
  = CandidateManifestDecode | CandidateMissingSummary | CandidateMissingNode
  | CandidateTarget | CandidateExactOwner | CandidateUnit
  | CandidateImportTuple | CandidateUntrackedExecution | CandidateExecutionBoot
  | CandidatePreprocessor | CandidateSourcePath | CandidateSourceRead
  | CandidateSourceSha | CandidateSummaryHash | CandidateNativeRead | CandidateNativeSha
  | CandidatePackageRead | CandidatePackageSelection | CandidateClosedHome
  | CandidateInterfaceRead | CandidateInterfaceTH | CandidateHydration | CandidateExecutionProof
  | CandidateCanonicalProof | CandidateProducerUnavailable | CandidateAccepted
  deriving (Eq, Ord, Show)

-- Keep the underlying refusal distinct from the coarse admission category.
-- Rendering is diagnostic only; candidate eligibility consumes this typed result.
data CandidateExecutionProofFailure
  = CandidateExecutionProducerMismatch String [(String, String)]
  | CandidateExecutionGraphBudget
  | CandidateExecutionReferenceBudget Int
  | CandidateExecutionSourceFailure ExecutionSourceFailure
  deriving (Eq, Show)

certifyModuleCandidates
  :: Word64 -> (HscEnv -> IO HscEnv) -> FilePath -> Maybe String -> Maybe ExactScope -> Set.Set ModuleName -> Maybe (Either String CapturedCandidateManifest) -> FilePath -> ModuleGraph -> FilePath
  -> Ghc (Map.Map ModuleName AdmittedSourceCandidate)
certifyModuleCandidates requestIdentity forkContext compilerViewDirectory expectedProducer exactScope sourceFreeOwners captured manifest graph targetPath = do
  timing <- liftIO readTimingEnabled
  observations <- liftIO (newIORef Map.empty)
  let boundedDetail detail = case splitAt 192 detail of
        (prefix, []) -> prefix
        (prefix, _) -> take 176 prefix ++ "...[truncated]"
      record owner reason detail = when timing $ liftIO $
        modifyIORef' observations (Map.insert owner (reason, fmap boundedDetail detail))
      recordCandidate candidate = record (candidateUnit candidate, candidateModule candidate)
  decoded <- liftIO $ case captured of
    Nothing -> readModuleCandidatesWithGraphs (maybe [] scopeExecutionGraphs exactScope) manifest
    Just (Left failure) -> pure (Left failure)
    Just (Right capturedManifest) -> readCapturedModuleCandidatesWithGraphs
      (maybe [] scopeExecutionGraphs exactScope) capturedManifest
  result <- case decoded of
    Left reason -> do
      record ("", "<manifest>") CandidateManifestDecode (Just reason)
      pure Map.empty
    Right candidates -> do
      env <- getSession
      targetName <- liftIO (targetModuleNameFor targetPath)
      current <- liftIO (dependencyEvidenceFor env ([], True) graph [])
      let summaries = Map.fromList
            [ (ms_mod_name summary, summary)
            | ModuleNode _ summary <- mgModSummaries' graph
            , ms_hsc_src summary == HsSrcFile ]
          currentModules = Map.fromList
            [ (dependencyModuleName node, node)
            | node <- dependencyModules current
            , not (dependencyModuleBoot node) ]
          bootSummaries = Map.fromList
            [ (ms_mod_name summary,summary)
            | ModuleNode _ summary <- mgModSummaries' graph
            , ms_hsc_src summary == HsBootFile ]
          bootModules = Map.fromList
            [ (mkModuleName (dependencyModuleName node),node)
            | node <- dependencyModules current, dependencyModuleBoot node ]
          originalOwners = maybe Set.empty (Set.fromList . map fst . scopeLexical) exactScope
          protectedRoots = Set.union sourceFreeOwners (Set.fromList
            [mkModuleName name | scope <- maybe [] pure exactScope
              , admission <- maybe [] pure (scopeCheckedCell scope)
              , name <- checkedReservedModules admission])
          offered = [candidate | candidate <- candidates
            , mkModuleName (candidateModule candidate) `Set.notMember` protectedRoots
            , mkModuleName (candidateModule candidate) /= targetName]
          parcels = [parcel | candidate <- offered, Just parcel <- [candidateExecutionSources candidate]]
          graphInventory = Map.fromList [(executionGraphSha256 graph',graph')
            | graph' <- maybe [] scopeExecutionGraphs exactScope ++ concatMap fst parcels]
          combinedGraphs = Map.elems graphInventory
          combinedReferences = maybe [] scopeExecutionOwners exactScope ++ map snd parcels
          combinedNative = maybe [] scopeExecutionNativeOwners exactScope ++ map candidateOriginalIdentity offered
          candidateProofs = Map.fromList [(candidateOriginalIdentity candidate,proveCandidate candidate)
            | candidate <- offered]
          candidateProof candidate = Map.findWithDefault (Right [])
            (candidateOriginalIdentity candidate) candidateProofs
          proveCandidate candidate = case candidateExecutionSources candidate of
            Nothing -> Right []
            Just (_,reference) -> do
              nodes <- executionSourceClosure combinedGraphs combinedReferences combinedNative
                [executionIdentityKey (executionRefIdentity reference)]
              let roots = [node | node <- nodes
                    , executionNodeIdentity node == candidateOriginalIdentity candidate]
              case roots of
                [root] | dependencyModuleSource (executionNodeModule root) == candidateSource candidate
                    && executionNodeSourceSha256 root == candidateSourceSha256 candidate -> Right nodes
                _ -> Left (ExecutionSourceChanged (candidateUnit candidate,candidateModule candidate))
          provenOriginals candidate = case candidateProof candidate of
            Left _ -> Map.empty
            Right nodes ->
              let direct = Set.fromList (concat
                    [executionNodeRequirements node | node <- nodes
                    , executionNodeIdentity node == candidateOriginalIdentity candidate])
              in Map.fromList [(key,node) | node <- nodes
                , let key = executionIdentityKey (executionNodeIdentity node)
                , key `Set.member` direct, key `Set.member` originalOwners]
          exactImportKey candidate qualifier name =
            let key = (candidateUnit candidate,name)
                local = case qualifier of
                  DependencyUnqualified -> True
                  DependencyThisUnit unit -> unit == candidateUnit candidate
                  DependencyOtherUnit _ -> False
            in if local && key `Set.member` originalOwners then Just key else Nothing
          -- A historical source path can become pathless only after the
          -- recipe resolves the complete dependency tuple to this original.
          normalizeImport candidate qualifier name boot selected =
            case exactImportKey candidate qualifier name of
              Nothing -> Just (qualifier,name,boot,selected)
              Just key -> do
                child <- Map.lookup key (provenOriginals candidate)
                unless (not boot && maybe True
                  (== dependencyModuleSource (executionNodeModule child)) selected) Nothing
                pure (qualifier,name,False,Nothing)
          candidateImportsMatch candidate node =
            let historical = mapM (\imported -> normalizeImport candidate
                  (candidateDependencyQualifier (candidateImportQualifier imported))
                  (candidateImportModule imported) (candidateImportBoot imported)
                  (candidateImportSelected imported)) (candidateImports candidate)
                currentImports = mapM (\imported -> normalizeImport candidate
                  (dependencyImportQualifier imported) (dependencyImportName imported)
                  (dependencyImportBoot imported) (dependencyImportSelected imported))
                  (dependencyModuleImports node)
                hidden imported = case candidateImportQualifier imported of
                  CandidateUnqualified -> local
                  CandidateThisUnit unit | unit == candidateUnit candidate -> local
                  _ -> False
                  where local = mkModuleName (candidateImportModule imported) `Set.member` sourceFreeOwners
                          && (candidateUnit candidate,candidateImportModule imported) `Set.notMember` originalOwners
            in not (any hidden (candidateImports candidate))
              && case (historical,currentImports) of
                (Just expected,Just actual) ->
                  let originalEdge (qualifier,name,_,_) = isJust (exactImportKey candidate qualifier name)
                      ordinary = sort . filter (not . originalEdge)
                      actualOriginals = Set.fromList [(candidateUnit candidate,name)
                        | edge@(_,name,_,_) <- actual, originalEdge edge]
                  -- Exact-produced evidence separates original imports from
                  -- ordinary source rows. Unchanged source bytes preserve the
                  -- authored qualifiers; the recipe binds their exact owners.
                  in ordinary expected == ordinary actual
                    && actualOriginals == Map.keysSet (provenOriginals candidate)
                _ -> False
          graphInventoryProof = do
            case exactScope of
              Nothing -> Right ()
              Just scope ->
                let mismatches = [(executionGraphSha256 graph',executionGraphProducer graph')
                      | graph' <- combinedGraphs
                      , executionGraphProducer graph' /= scopeProducerSha256 scope]
                in unless (null mismatches) (Left (CandidateExecutionProducerMismatch
                     (scopeProducerSha256 scope) mismatches))
            unless (executionSourceGraphsFit combinedGraphs) (Left CandidateExecutionGraphBudget)
            unless (length combinedReferences <= 4096)
              (Left (CandidateExecutionReferenceBudget (length combinedReferences)))
          candidateExecutionProof candidate = do
            graphInventoryProof
            either (Left . CandidateExecutionSourceFailure) Right (candidateProof candidate)
          candidateExecutionMatches candidate =
            either (const False) (const True) (candidateExecutionProof candidate)
          admissionDetail candidate CandidateExecutionProof =
            either (Just . show) (const Nothing) (candidateExecutionProof candidate)
          admissionDetail candidate CandidateImportTuple =
            case Map.lookup (candidateModule candidate) currentModules of
              Nothing -> Nothing
              Just node ->
                let historical = mapM (\imported -> normalizeImport candidate
                      (candidateDependencyQualifier (candidateImportQualifier imported))
                      (candidateImportModule imported) (candidateImportBoot imported)
                      (candidateImportSelected imported)) (candidateImports candidate)
                    currentImports = mapM (\imported -> normalizeImport candidate
                      (dependencyImportQualifier imported) (dependencyImportName imported)
                      (dependencyImportBoot imported) (dependencyImportSelected imported))
                      (dependencyModuleImports node)
                    limited label rows = label ++ "=" ++ show (take 2 rows)
                      ++ ";omitted=" ++ show (max 0 (length rows - 2))
                in Just $ case (historical, currentImports) of
                  (Just expected, Just actual) ->
                    let expectedCounts = Map.fromListWith (+) [(row, 1 :: Int) | row <- expected]
                        actualCounts = Map.fromListWith (+) [(row, 1 :: Int) | row <- actual]
                        changed = [(row, oldCount, newCount)
                          | row <- Set.toAscList (Map.keysSet expectedCounts `Set.union` Map.keysSet actualCounts)
                          , let oldCount = Map.findWithDefault 0 row expectedCounts
                          , let newCount = Map.findWithDefault 0 row actualCounts
                          , oldCount /= newCount]
                        actualOriginals = Set.toAscList (Set.fromList
                          [(candidateUnit candidate, name) | (qualifier, name, _, _) <- actual
                            , isJust (exactImportKey candidate qualifier name)])
                    in limited "normalized_changed_counts" changed
                      ++ ";" ++ limited "expected_originals" (Map.keys (provenOriginals candidate))
                      ++ ";" ++ limited "actual_originals" actualOriginals
                  _ -> "normalization_failed;" ++ limited "historical_raw" (candidateImports candidate)
                    ++ ";" ++ limited "current_raw"
                      [(dependencyImportQualifier imported, dependencyImportName imported,
                        dependencyImportBoot imported, dependencyImportSelected imported)
                        | imported <- dependencyModuleImports node]
          admissionDetail _ _ = Nothing
          candidateDependencyQualifier CandidateUnqualified = DependencyUnqualified
          candidateDependencyQualifier (CandidateThisUnit unit) = DependencyThisUnit unit
          candidateDependencyQualifier (CandidateOtherUnit unit) = DependencyOtherUnit unit
      preflight <- fmap catMaybes $ forM candidates $ \candidate ->
        case (Map.lookup (mkModuleName (candidateModule candidate)) summaries,
              Map.lookup (candidateModule candidate) currentModules) of
          (Just summary, Just node) -> case
              [reason | (rejected, reason) <-
                [ (ms_mod_name summary == targetName, CandidateTarget)
                , (not (candidateExecutionMatches candidate), CandidateExecutionProof)
                , (ms_mod_name summary `Set.member` protectedRoots, CandidateExactOwner)
                , (candidateUnit candidate /= unitString (moduleUnit (ms_mod summary)), CandidateUnit)
                , (not (candidateImportsMatch candidate node), CandidateImportTuple)
                , (hasUntrackedCompileTimeExecution (ms_hspp_opts summary), CandidateUntrackedExecution)
                , (backendGeneratesCode (backend (ms_hspp_opts summary))
                    && (Map.member (ms_mod_name summary) bootSummaries
                      || any candidateImportBoot (candidateImports candidate)), CandidateExecutionBoot)
                , (gopt Opt_Pp (ms_hspp_opts summary), CandidatePreprocessor) ]
              , rejected] of
              reason : _ -> recordCandidate candidate reason (admissionDetail candidate reason) >> pure Nothing
              [] -> do
                source <- liftIO $ traverse canonicalizePath
                  (ml_hs_file (ms_location summary))
                if source /= Just (candidateSource candidate)
                  then recordCandidate candidate CandidateSourcePath
                    (Just ("historical=" ++ show (candidateSource candidate) ++ ";current=" ++ show source))
                    >> pure Nothing
                  else do
                    inspected <- liftIO (try (sourceEvidenceWithFingerprint
                      (candidateSource candidate))
                      :: IO (Either IOException (DependencySource, Fingerprint)))
                    packageWitness <- liftIO $ readPackageImports
                      (candidatePackageImports candidate)
                      (candidatePackageImportsSha256 candidate)
                      (ExactIfaceArtifact (candidateUnit candidate)
                        (candidateModule candidate) (candidateInterface candidate)
                        (candidateInterfaceSha256 candidate) [])
                    originalProduct <- liftIO (try (BS.readFile (candidateProductPath candidate))
                      :: IO (Either IOException BS.ByteString))
                    packageSelected <- liftIO $ case packageWitness of
                      Left _ -> pure False
                      Right roots -> do
                        recorded <- and <$> mapM (fmap (either (const False) (const True))
                          . validatePackageImportRoot env) (packageInterfaces roots)
                        selectedImports <- forM (ms_textual_imps summary ++ ms_srcimps summary) $ \(qualifier, name) -> do
                          let exact = exactImportKey candidate (dependencyQualifier qualifier)
                                (moduleNameString (unLoc name))
                          resolved <- case exact >>= (`Map.lookup` provenOriginals candidate) of
                            Just _ -> pure Nothing
                            Nothing -> Just <$> findImportedModule env (unLoc name) qualifier
                          case resolved of
                            Nothing -> pure (Right Nothing)
                            Just (Found _ owner)
                              | isHomeUnit (hsc_home_unit env) (moduleUnit owner)
                                || owner == gHC_PRIM ->
                                  pure (Right Nothing)
                              | otherwise -> fmap Just <$> packageImportRoot env owner
                            _ -> pure (Left "direct import does not resolve")
                        pure $ recorded && case sequence selectedImports of
                          Left _ -> False
                          Right selected -> all (`elem` packageInterfaces roots) (catMaybes selected)
                    case (inspected, packageWitness, originalProduct) of
                      (Right (evidence, fingerprint), Right roots, Right productBytes)
                        | dependencySourceSha256 evidence /= candidateSourceSha256 candidate ->
                            recordCandidate candidate CandidateSourceSha Nothing >> pure Nothing
                        | fingerprint /= ms_hs_hash summary ->
                            recordCandidate candidate CandidateSummaryHash Nothing >> pure Nothing
                        | hexBytes (SHA256.hash productBytes) /= candidateProductSha256 candidate ->
                            recordCandidate candidate CandidateNativeSha Nothing >> pure Nothing
                        | not packageSelected ->
                            recordCandidate candidate CandidatePackageSelection Nothing >> pure Nothing
                        | otherwise -> pure (Just (ms_mod_name summary, (candidate, summary, node, roots)))
                      (Left _, _, _) -> recordCandidate candidate CandidateSourceRead Nothing >> pure Nothing
                      (_, Left reason, _) -> recordCandidate candidate CandidatePackageRead (Just reason) >> pure Nothing
                      (_, _, Left _) -> recordCandidate candidate CandidateNativeRead Nothing >> pure Nothing
          (Nothing, _) -> recordCandidate candidate CandidateMissingSummary Nothing >> pure Nothing
          (_, Nothing) -> recordCandidate candidate CandidateMissingNode Nothing >> pure Nothing
      let initial = Map.fromList preflight
          requiredHome node =
            [ mkModuleName (dependencyImportName imported)
            | imported <- dependencyModuleImports node
            , isJust (dependencyImportSelected imported) ]
          requiredBoot node =
            [ mkModuleName (dependencyImportName imported)
            | imported <- dependencyModuleImports node
            , isJust (dependencyImportSelected imported), dependencyImportBoot imported ]
          bootClosed selected name = Map.member name bootSummaries
            && case Map.lookup name bootModules of
              Just node -> all (`Map.member` selected) (requiredHome node)
                && all (`Map.member` bootSummaries) (requiredBoot node)
              Nothing -> False
          dependencyClosed selected candidate node =
            all (\imported -> case exactImportKey candidate
                (dependencyImportQualifier imported) (dependencyImportName imported) of
              Just key -> Map.member key (provenOriginals candidate)
              Nothing -> isNothing (dependencyImportSelected imported)
                || Map.member (mkModuleName (dependencyImportName imported)) selected)
              (dependencyModuleImports node)
          shrink selected = Map.filterWithKey (\name (candidate, _, node, _) ->
            dependencyClosed selected candidate node
              && all (bootClosed selected) (requiredBoot node)
              && (not (Map.member name bootSummaries) || bootClosed selected name)) selected
          closed selected = let smaller = shrink selected in
            if Map.keysSet smaller == Map.keysSet selected then smaller else closed smaller
          admitted = closed initial
          artifact (candidate, _, _, _) = ExactIfaceArtifact
            (candidateUnit candidate) (candidateModule candidate)
            (candidateInterface candidate) (candidateInterfaceSha256 candidate)
            (candidateInterfaceRequirements candidate)
      forM_ (Map.elems (Map.difference initial admitted)) $ \(candidate, _, _, _) ->
        recordCandidate candidate CandidateClosedHome Nothing
      let recordAdmitted reason detail = forM_ (Map.elems admitted) $ \(candidate, _, _, _) ->
            recordCandidate candidate reason detail
      if Map.null admitted
        then pure Map.empty
        else do
          let candidates' = map artifact (Map.elems admitted)
              originals = maybe [] (map (\(iface,_,_) -> iface) . scopeInterfaces) exactScope
              values = maybe [] scopeValueInterfaces exactScope
          let canonicalRows = [(artifact tuple, candidatePackageImports candidate,
                candidatePackageImportsSha256 candidate)
                | tuple@(candidate,_,_,_) <- Map.elems admitted]
                ++ maybe [] scopeInterfaces exactScope
          proofsResult <- case expectedProducer of
            Nothing -> do
              recordAdmitted CandidateProducerUnavailable Nothing
              pure (Left "candidate admission requires the owning compiler producer")
            Just producer -> liftIO $ fmap (fmap Map.fromList . sequence) $
              forM (Map.toAscList admitted) $ \(name,(candidate,_,_,_)) ->
                fmap (fmap (\proof -> (name,proof)))
                  (validateCandidateCanonicalInterfaceProof producer canonicalRows candidate)
          case proofsResult of
            Left reason -> recordAdmitted CandidateCanonicalProof (Just reason) >> pure Map.empty
            Right proofs -> do
              captured <- liftIO $ readVerifiedExactIfaceClosureWithCheckedValues env
                (candidates' ++ originals) values
              let loaded = captured >>= \verified -> selectVerifiedExactInterfaces verified candidates'
                  originalInterfaces = captured >>= \verified -> selectVerifiedExactInterfaces verified originals
                  installLexical candidateGraph hydrated = case (exactScope,captured,originalInterfaces) of
                    (Nothing,_,_) -> pure (Right hydrated)
                    (Just scope,Right verified,Right originalIfaces) -> case
                        checkedValueImportAuthorityFromVerified verified values of
                      Left reason -> pure (Left reason)
                      Right checked -> do
                        let byOwner = Map.fromList [((exactUnit iface,exactModule iface),iface)
                              | (iface,_) <- originalIfaces]
                            lexical = [(iface,imports) | (key,imports) <- scopeLexical scope
                              , Just iface <- [Map.lookup key byOwner]]
                        installExactLexicalGraphWithScaffold candidateGraph lexical checked
                          noGeneratedScaffoldImports hydrated
                    _ -> pure (Left "candidate original interface closure unavailable")
              case loaded of
                Left reason -> recordAdmitted CandidateInterfaceRead (Just reason) >> pure Map.empty
                Right interfaces
                  | any (mi_used_th . snd) interfaces ->
                      recordAdmitted CandidateInterfaceTH Nothing >> pure Map.empty
                  | otherwise -> do
                      liftIO $ forM_ interfaces $ \(artifact',iface) ->
                        either throwIO pure (validateCandidateInterfaceRequirements
                          (proofs Map.! mkModuleName (exactModule artifact')) iface)
                      let selectedBoots = [summary | (name,summary) <- Map.toList bootSummaries,
                            Map.member name admitted]
                      let canonicalLoadGraph = mapMG (\summary -> summary
                            { ms_hspp_opts = canonicalizeRepresentationFlags (ms_hspp_opts summary) }) graph
                          nativeGraph = mapMG (\summary -> summary
                            { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts summary) }) (hsc_mod_graph env)
                          nativeSummary summary = summary
                            { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts summary) }
                      -- Current source selection checks the original native
                      -- interface profile. GHC executable demand subsequently
                      -- consumes the same finalized Core without a frontend.
                      hydratedResult <- hydrateCandidateHomeProductsWithOriginalsUsing forkContext env { hsc_mod_graph = nativeGraph }
                        canonicalLoadGraph interfaces (either (const []) id originalInterfaces)
                        installLexical
                        [nativeSummary summary | (_,summary,_,_) <- Map.elems admitted] selectedBoots
                      case hydratedResult of
                        Left reason -> recordAdmitted CandidateHydration (Just reason) >> pure Map.empty
                        Right hydrated -> do
                          setSession hydrated { hsc_mod_graph = hsc_mod_graph env }
                          views <- fmap Map.fromList $ forM
                            (zip [0..] (Map.toAscList admitted)) $ \(index,(name,(_,summary,_,_))) -> do
                              currentEnvironment <- getSession
                              let originalFlags = ms_hspp_opts summary
                                  executable = backendGeneratesCode (backend originalFlags)
                                  canonicalSummary' = summary {ms_hspp_opts =
                                    (canonicalizeDFlags originalFlags)
                                      {backend = backend originalFlags, ghcLink = ghcLink originalFlags}}
                              view <- liftIO $ materializeCandidateCompilerView compilerViewDirectory
                                index currentEnvironment (proofs Map.! name) canonicalSummary'
                              when executable $ liftIO $
                                emitCount timing "candidate_finalized_core_make_view" 1
                              pure (name,view)
                          recordAdmitted CandidateAccepted Nothing
                          pure (Map.mapWithKey (\name (candidate, summary, _, roots) -> AdmittedSourceCandidate
                            candidate roots (if backendGeneratesCode (backend (ms_hspp_opts summary))
                              then CandidateLoadForExecution else CandidateInterfaceOnly)
                            (proofs Map.! name) (views Map.! name)) admitted)
  when timing $ liftIO $ do
    observed <- readIORef observations
    let counts = Map.fromListWith (+)
          [(reason, 1 :: Integer) | (reason, _) <- Map.elems observed]
    forM_ (Map.toAscList counts) $ \(reason, count) ->
      emitCount timing ("candidate_admission." ++ show reason) count
    forM_ (take 128 (Map.toAscList observed)) $ \((unit, name), (reason, detail)) ->
      hPutStrLn stderr ("tidepool-candidate-admission owner=" ++ show (take 192 (unit ++ ":" ++ name))
        ++ " reason=" ++ show reason ++ " cycle=" ++ show requestIdentity
        ++ maybe "" ((" detail=" ++) . show) detail)
    emitCount timing "candidate_admission_rows_omitted" (toInteger (max 0 (Map.size observed - 128)))
  pure result

revalidateAcceptedCandidates :: [AdmittedSourceCandidate] -> IO Bool
revalidateAcceptedCandidates candidates = and <$> forM candidates (\admission -> do
  let candidate = admittedCandidateOriginal admission
      proof = admittedCandidateProof admission
  readBack <- try $ do
    (source, _) <- sourceEvidenceWithFingerprint (candidateSource candidate)
    interface <- BS.readFile (candidateInterface candidate)
    productBytes <- BS.readFile (candidateProductPath candidate)
    certificate <- bounded (canonicalCertificatePath proof) (4 * 1024 * 1024)
    coreValid <- case admittedCandidateLoading admission of
      CandidateInterfaceOnly -> pure True
      CandidateLoadForExecution -> case canonicalCoreArtifact proof of
        Nothing -> pure False
        Just core -> (== canonicalCoreSha256 core) . hexBytes . SHA256.hash
          <$> bounded (canonicalCorePath core) (32 * 1024 * 1024)
    packageImports <- readPackageImports (candidatePackageImports candidate)
      (candidatePackageImportsSha256 candidate)
      (ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
        (candidateInterface candidate) (candidateInterfaceSha256 candidate) [])
    pure (dependencySourceSha256 source == candidateSourceSha256 candidate
      && hexBytes (SHA256.hash interface) == candidateInterfaceSha256 candidate
      && hexBytes (SHA256.hash productBytes) == candidateProductSha256 candidate
      && hexBytes (SHA256.hash certificate) == canonicalCertificateSha256 proof
      && coreValid
      && either (const False) (const True) packageImports)
    :: IO (Either IOException Bool)
  pure (either (const False) id readBack))
  where
    bounded path' limit = do
      bytes <- readFileAtMost path' (limit + 1)
      unless (BS.length bytes <= limit) (ioError (userError "candidate artifact exceeds byte bound"))
      pure bytes

dependencyQualifier :: PkgQual -> DependencyQualifier
dependencyQualifier NoPkgQual = DependencyUnqualified
dependencyQualifier (ThisPkg unit) = DependencyThisUnit (unitString unit)
dependencyQualifier (OtherPkg unit) = DependencyOtherUnit (unitString unit)

-- | Capture import-resolution witnesses from the exact module graph. Package
-- imports have no selected home path; their ordered absent home candidates
-- remain evidence because creating one later would introduce shadowing.
-- Explicit exact imports have owner witnesses rather than source lookup
-- witnesses. The existing fresh-source evidence owner handles every remaining
-- import, including negative candidates for newly discovered dependencies.
sourceEvidenceGraph
  :: Maybe ExactScope -> ModuleGraph
  -> (ModuleGraph, [((String, String, Bool), [(DependencyQualifier, String, Bool, String)])])
sourceEvidenceGraph Nothing graph = (graph, [])
sourceEvidenceGraph (Just scope) graph =
  (mkModuleGraph (map strip nodes), map importsFor summaries)
  where
    nodes = mgModSummaries' graph
    summaries = [summary | ModuleNode _ summary <- nodes]
    exactOwners = Map.fromList [(mkModuleName (exactModule iface), exactUnit iface)
      | (iface, _, _) <- scopeInterfaces scope]
    selected qualifier imported = do
      unit <- Map.lookup (unLoc imported) exactOwners
      case qualifier of
        NoPkgQual -> Just unit
        ThisPkg requested | unitString requested == unit -> Just unit
        _ -> Nothing
    importsFor summary =
      ((unitString (moduleUnit (ms_mod summary)), moduleNameString (ms_mod_name summary),
        ms_hsc_src summary == HsBootFile),
       sort . nub $
         [(dependencyQualifier qualifier, moduleNameString (unLoc imported), boot, unit)
         | (boot, edges) <- [(False, ms_textual_imps summary), (True, ms_srcimps summary)]
         , (qualifier, imported) <- edges
         , Just unit <- [selected qualifier imported]])
    ordinary (qualifier, imported) = isNothing (selected qualifier imported)
    strip (ModuleNode edges summary) = ModuleNode edges (summary
      { ms_textual_imps = filter ordinary (ms_textual_imps summary)
      , ms_srcimps = filter ordinary (ms_srcimps summary) })
    strip node = node

dependencyEvidenceFor
  :: HscEnv -> ([DependencySource], Bool) -> ModuleGraph -> [(ModuleName, ModuleFacts)]
  -> IO DependencyEvidence
dependencyEvidenceFor env (sources, sourcesComplete) graph moduleFacts = do
  let graphSummaries = [summary | ModuleNode _ summary <- mgModSummaries' graph]
      factsByName = Map.fromList moduleFacts
  summarySources <- forM graphSummaries $ \summary ->
    case ml_hs_file (ms_location summary) of
      Nothing -> pure Nothing
      Just source -> Just . normalise <$> makeAbsolute source
  let selected = Map.fromList
        [ ((ms_mod_name summary, ms_hsc_src summary == HsBootFile), source)
        | (summary, Just source) <- zip graphSummaries summarySources
        ]
      homeSelection NoPkgQual name boot = Map.lookup (name, boot) selected
      homeSelection (ThisPkg unit) name boot
        | unit == homeUnitId (hsc_home_unit env) = Map.lookup (name, boot) selected
      homeSelection _ _ _ = Nothing
      allImports = sort . Set.toList . Set.fromList $
        [ (qualifier, unLoc imported, False)
        | summary <- graphSummaries
        , (qualifier, imported) <- ms_textual_imps summary
        ] ++
        [ (qualifier, unLoc imported, True)
        | summary <- graphSummaries
        , (qualifier, imported) <- ms_srcimps summary
        ]
      roots = nubOrd (concatMap (importPaths . ms_hspp_opts) graphSummaries)
      moduleRelative name =
        map (\c -> if c == '.' then pathSeparator else c) (moduleNameString name)
      homeLookup (OtherPkg _) = False
      homeLookup _ = True
  -- Root normalization belongs to this evidence capture. The worker holds its
  -- request's working directory until compilation returns; filesystem absence
  -- and selected-source checks still run freshly at their admission boundaries.
  absoluteRoots <- if any (\(qualifier, _, _) -> homeLookup qualifier) allImports
    then mapM (fmap normalise . makeAbsolute) roots
    else pure []
  let candidatesFor name isBoot =
        [ normalise (root </> moduleRelative name ++ extension)
        | root <- absoluteRoots
        , extension <- if isBoot then [".hs-boot", ".lhs-boot"]
            else [".hs", ".lhs", ".hsig", ".lhsig"]
        ]
  absoluteCandidates <- forM allImports $ \(qualifier, imported, isBoot) -> do
    let candidates = if homeLookup qualifier then candidatesFor imported isBoot else []
        chosen = homeSelection qualifier imported isBoot
        throughSelected = case chosen of
          Nothing -> candidates
          Just path -> case break (== path) candidates of
            (higher, _ : _) -> higher ++ [path]
            _ -> candidates ++ [path]
    pure DependencyResolution
      { dependencyResolutionQualifier = dependencyQualifier qualifier
      , dependencyResolutionModule = moduleNameString imported
      , dependencyResolutionBoot = isBoot
      , dependencyResolutionSelected = chosen
      , dependencyResolutionCandidates = nubOrd throughSelected
      }
  moduleNodes <- forM (zip graphSummaries summarySources) $ \(summary, sourcePath) -> do
    let name = ms_mod_name summary
        isBoot = ms_hsc_src summary == HsBootFile
        directImports = sort . Set.toList . Set.fromList $
          [ (qualifier, unLoc imported, False) | (qualifier, imported) <- ms_textual_imps summary ] ++
          [ (qualifier, unLoc imported, True) | (qualifier, imported) <- ms_srcimps summary ]
    pure DependencyModule
      { dependencyModuleUnit = unitString (moduleUnit (ms_mod summary))
      , dependencyModuleName = moduleNameString name
      , dependencyModuleBoot = isBoot
      , dependencyModuleSource = maybe "" id sourcePath
      , dependencyModuleImports =
          [ DependencyImport (dependencyQualifier qualifier) (moduleNameString imported) boot
              (homeSelection qualifier imported boot)
          | (qualifier, imported, boot) <- directImports
          ]
      , dependencyModuleProduct = if isBoot then ProductBoot else ProductInterfaceOnly
      }
  let untrackedExecution summary =
        let flags = ms_hspp_opts summary
        in hasUnconditionallyUntrackedCompileTimeExecution flags
          || if ms_hsc_src summary == HsBootFile
              -- Boot summaries have no parsed observation in the executable
              -- walk. Never borrow the regular module's quote evidence.
              then xopt LangExt.QuasiQuotes flags
              else case Map.lookup (ms_mod_name summary) factsByName of
                Nothing -> True
                Just facts -> moduleFactHasDependentFiles facts
                  || moduleFactQuasiQuoteUse facts == HasQuasiQuotes
      hasUntrackedExecution = any untrackedExecution graphSummaries
      complete = sourcesComplete && not (any (moduleFactHasDependentFiles . snd) moduleFacts)
        && not hasUntrackedExecution
        && all (\resolution -> not (null (dependencyResolutionCandidates resolution))
              || case dependencyResolutionQualifier resolution of
                  DependencyOtherUnit _ -> True
                  _ -> False)
             absoluteCandidates
      packages = sort
        [ moduleNameString imported
        | (qualifier, imported, isBoot) <- allImports
        , not isBoot
        , isNothing (homeSelection qualifier imported False)
        ]
  pure DependencyEvidence
    { dependencyCacheSafe = complete
    , dependencySelectionComplete = complete
    , dependencySources = sources
    , dependencyResolutions = absoluteCandidates
    , dependencyPackages = packages
    , dependencyModules = moduleNodes
    }


-- ---------------------------------------------------------------------------
-- Resident compiler owner: immutable module versions outlive request capabilities.
-- ---------------------------------------------------------------------------

withResidentPipelineSelected :: [FilePath] -> (ResidentCompiler -> IO a) -> IO a
withResidentPipelineSelected baseIncludes useCompiler =
  withResidentPipelineSelectedRequests baseIncludes $ \runRequest ->
    runRequest (pure ()) useCompiler

withResidentPipelineSelectedRequests :: [FilePath] -> (RequestRunner -> IO a) -> IO a
withResidentPipelineSelectedRequests baseIncludes useRequests =
  withResidentCompilerScopes baseIncludes $ \runScope ->
    useRequests $ \clearRecovery action ->
      runScope clearRecovery (\scope -> action (scopedCompile scope))

-- | Transaction-issued operations share one thread/phase guard. A released
-- capability cannot access contexts retained by the worker for later requests.
data CompilerScope = CompilerScope
  { scopedCompile :: ResidentCompiler
  , scopedRunGhc :: forall result. Ghc result -> IO result
  , scopedParserFlags :: DynFlags
  , scopedRecoveryCaches :: IO CompilerRecoveryCaches
  , scopedExecutor :: Maybe CompilerExecutor
  }

type ResidentCompiler = forall result.
  PipelineSelection result -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope
  -> FilePath -> [FilePath] -> Maybe FilePath -> IO result

type RequestRunner = forall requestResult.
  IO () -> (ResidentCompiler -> IO requestResult) -> IO requestResult

type CompilerScopeRunner = forall requestResult.
  IO () -> (CompilerScope -> IO requestResult) -> IO requestResult

withResidentCompilerScopes :: [FilePath] -> (CompilerScopeRunner -> IO a) -> IO a
withResidentCompilerScopes baseIncludes useRequests = do
  producer <- captureCompilerProducerIdentity
  timing <- readTimingEnabled
  (libdir,startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  -- Internal interpreters cannot retire a dynamic object epoch. The selected
  -- pinned GHC closure owns this executable alongside its libraries.
  let iserv = libdir </> ".." </> "bin" </> "ghc-iserv"
  available <- doesFileExist iserv
  unless available (fail "resident compiler GHC closure lacks ghc-iserv")
  runGhc (Just libdir) $ do
    parserFlags <- getSessionDynFlags
    let bootstrapFlags = gopt_set (extractionDynFlags parserFlags baseIncludes) Opt_ExternalInterpreter
    _ <- setSessionDynFlags bootstrapFlags
    baseEnv <- getSession
    -- GHC initializes the target's platform constants with its package universe.
    -- Retained views must reset to that initialized profile, not the bootstrap.
    let baseFlags = hsc_dflags baseEnv
    auxiliaryRecovery <- liftIO freshCompilerRecoveryCaches
    initialRecovery <- liftIO (RecoveryContext <$> RequestUnique.newUnique <*> pure auxiliaryRecovery)
    interpreter <- liftIO (newIORef (CompilerInterpreterState 0 Map.empty))
    packageFinder <- liftIO (newPackageFinderFacts baseEnv)
    universe <- liftIO (newIORef (CompilerUniverse baseEnv Map.empty Map.empty initialRecovery interpreter packageFinder))
    active <- liftIO (newIORef Nothing)
    availability <- liftIO (newIORef ResidentAvailable)
    ownerThread <- liftIO myThreadId
    reifyGhc $ \session ->
      let restoreBase = reflectGhc (setSession baseEnv) session
          runRequest :: CompilerScopeRunner
          runRequest clearRecovery action = bracket acquire release $ \() -> do
            phase <- newIORef CompilerReady
            interrupted <- newIORef False
            interpreterAttempt <- newIORef InterpreterConfirmed
            operationPurpose <- newIORef "request"
            operationIdentity <- newTimingRequestIdentity >>= newIORef
            let runGuarded :: forall result. IO result -> IO result
                runGuarded operation = mask $ \restore -> do
                  caller <- myThreadId
                  unless (caller == ownerThread) (throwIO CompilerTransactionWrongThread)
                  previous <- atomicModifyIORef' phase $ \state ->
                    (if state == CompilerReady then CompilerRunning else state,state)
                  case previous of
                    CompilerReady -> pure ()
                    CompilerRunning -> throwIO CompilerTransactionBusy
                    CompilerFailed -> throwIO CompilerTransactionFailed
                    CompilerClosed -> throwIO CompilerTransactionReleased
                  writeIORef interpreterAttempt InterpreterConfirmed
                  result <- restore operation `catch` \(failure :: SomeException) -> do
                    writeIORef phase CompilerFailed
                    writeIORef availability ResidentPoisoned
                    case fromException failure :: Maybe SomeAsyncException of
                      Just _ -> writeIORef interrupted True >> throwIO failure
                      Nothing -> do
                        case fromException failure of
                          Just CompilerTransactionPoisoned -> throwIO failure
                          _ -> pure ()
                        -- Failed home/finder/memo choices remain private.
                        -- Demand-loaded fixed package facts survive on the
                        -- same owner used by lazy completed interfaces.
                        clearRecovery
                        writeIORef active Nothing
                        mutation <- readIORef interpreterAttempt
                        recoverCompilerInterpreter interpreter baseEnv
                          (mutation == InterpreterMutationPending)
                        when (mutation == InterpreterMutationPending) $ do
                          purpose <- readIORef operationPurpose
                          identity <- readIORef operationIdentity
                          emitReuse timing (ReuseContext identity purpose)
                            NativeImage ReuseEpochRotated Recovery Nothing 1 Nothing
                        restoreBase
                        writeIORef phase CompilerReady
                        writeIORef availability ResidentBusy
                        throwIO failure
                  writeIORef interpreterAttempt InterpreterConfirmed
                  writeIORef phase CompilerReady
                  pure result
                compile :: ResidentCompiler
                compile selection retained purpose mscope path includes products = runGuarded $ do
                  requestIdentity <- newTimingRequestIdentity
                  writeIORef operationIdentity requestIdentity
                  writeIORef operationPurpose (compilePurposeLabel purpose)
                  reflectGhc
                    (residentCompileOne producer selection retained universe active interpreterAttempt baseEnv baseFlags
                      timing requestIdentity purpose mscope path includes products) session
                runOperation :: forall result. Ghc result -> IO result
                runOperation operation = runGuarded $ do
                  requestIdentity <- newTimingRequestIdentity
                  writeIORef operationIdentity requestIdentity
                  writeIORef operationPurpose "helper"
                  -- This capability admits arbitrary Ghc operations. A refusal
                  -- cannot prove that opaque loading left the interpreter intact.
                  writeIORef interpreterAttempt InterpreterMutationPending
                  bracket
                    (reflectGhc getSession session)
                    (\original -> reflectGhc (setSession original) session)
                    (\_original -> do
                      retainedUniverse <- readIORef universe
                      current <- readIORef active
                      borrowed <- forkExactContextWithPackageFacts (universePackageFinder retainedUniverse)
                        (maybe (universeEnvironment retainedUniverse) activeEnvironment current)
                      result <- reflectGhc (setSession (borrowed
                        {hsc_hooks=hsc_hooks baseEnv,hsc_logger=hsc_logger baseEnv}) >> operation) session
                      observed <- reflectGhc getSession session
                      -- A successful helper may demand package declarations. Retain
                      -- its EPS/finder cells without publishing its home visibility.
                      modifyIORef' universe (\state -> state {universeEnvironment =
                        (universeEnvironment state)
                          {hsc_FC=hsc_FC observed, hsc_unit_env=(hsc_unit_env (universeEnvironment state))
                            {ue_eps=ue_eps (hsc_unit_env observed)}}})
                      pure result)
                recovery = do
                  caller <- myThreadId
                  unless (caller == ownerThread) (throwIO CompilerTransactionWrongThread)
                  state <- readIORef phase
                  unless (state == CompilerReady || state == CompilerRunning)
                    (throwIO CompilerTransactionReleased)
                  current <- readIORef active
                  case current of
                    Just context -> pure (activeRecovery context)
                    Nothing -> recoveryContextCaches . universeRecovery <$> readIORef universe
                finish = do
                  writeIORef phase CompilerClosed
                  cancelled <- readIORef interrupted
                  when cancelled $ do
                    recoverCompilerInterpreter interpreter baseEnv True
                    purpose <- readIORef operationPurpose
                    identity <- readIORef operationIdentity
                    emitReuse timing (ReuseContext identity purpose)
                      NativeImage ReuseEpochRotated Recovery Nothing 1 Nothing
                    writeIORef availability ResidentBusy
                  writeIORef active Nothing
                  restoreBase
            action (CompilerScope compile runOperation parserFlags recovery Nothing) `finally` finish
          acquire = do
            caller <- myThreadId
            unless (caller == ownerThread) (throwIO CompilerTransactionWrongThread)
            previous <- atomicModifyIORef' availability $ \state ->
              (if state == ResidentAvailable then ResidentBusy else state,state)
            case previous of
              ResidentAvailable -> pure ()
              ResidentBusy -> throwIO CompilerTransactionBusy
              ResidentPoisoned -> throwIO CompilerTransactionPoisoned
              ResidentClosed -> throwIO CompilerTransactionReleased
          release () = do
            state <- readIORef availability
            when (state == ResidentBusy) $ do
              writeIORef availability ResidentPoisoned
              restoreBase
              writeIORef availability ResidentAvailable
      in useRequests runRequest `finally` do
        writeIORef availability ResidentClosed
        forM_ (hsc_interp baseEnv) retireCompilerInterpreter

-- | Auxiliary exact-interface operations borrow the same resident owner. Home
-- visibility starts empty; installed-package facts retain their matched closure.
withScopedExactInterfaceTransaction :: CompilerScope -> [FilePath] -> (HscEnv -> IO a) -> IO a
withScopedExactInterfaceTransaction compiler includes use = scopedRunGhc compiler $ do
  current <- getSession
  let env = hscUpdateHPT (const emptyHomePackageTable) current
        { hsc_mod_graph=mkModuleGraph [], hsc_targets=[], hsc_type_env_vars=emptyKnotVars }
  setSession (hscUpdateFlags (\flags -> flags {importPaths=includes}) env)
  getSession >>= liftIO . use

selectCompilerRecoveryCaches
  :: HscEnv -> RecoveryContext -> [(Module,RecoveryContext)] -> IO CompilerRecoveryCaches
selectCompilerRecoveryCaches env packages selected = do
  let grouped = Map.fromListWith combine
        [(recoveryContextIdentity context,(context,Set.singleton owner)) | (owner,context) <- selected]
      combine (context,owners) (_,more) = (context,Set.union owners more)
      contexts = [(recoveryContextCaches context,owners) | (context,owners) <- Map.elems grouped]
      packageCaches = recoveryContextCaches packages
      packageOwner owner = not (isHomeUnit (hsc_home_unit env) (moduleUnit owner))
  homes <- CompilerRecoveryCaches
    <$> selectFatIfaceCaches [(compilerFatIface caches,owners) | (caches,owners) <- contexts]
    <*> selectOwnerInterfaceCaches [(compilerOwnerIface caches,owners) | (caches,owners) <- contexts]
    <*> selectPreparedBodyCaches [(compilerPreparedBodies caches,owners) | (caches,owners) <- contexts]
    <*> selectOriginalProjectionCaches [(compilerOriginalProjections caches,owners) | (caches,owners) <- contexts]
  -- The fixed package universe is scanned once, after direct home-owner
  -- selection; historical request maps are never scanned per selected module.
  CompilerRecoveryCaches
    <$> mergeFatIfaceCaches [(compilerFatIface homes,const True),(compilerFatIface packageCaches,packageOwner)]
    <*> mergeOwnerInterfaceCaches [(compilerOwnerIface homes,const True),(compilerOwnerIface packageCaches,packageOwner)]
    <*> mergePreparedBodyCaches [(compilerPreparedBodies homes,const True),(compilerPreparedBodies packageCaches,packageOwner)]
    <*> mergeOriginalProjectionCaches [(compilerOriginalProjections homes,const True),(compilerOriginalProjections packageCaches,packageOwner)]

sourceReuseAdmitted :: PipelineVariant -> Maybe SessionScope -> Bool
sourceReuseAdmitted variant session = case pvExactScope variant of
  Just scope -> case exactReuseAdmission scope session of
    ScopeAuthenticatedValueInputs -> True
    UnsealedSessionValueInputs -> False
  Nothing -> null (pvDownsweepExcludes variant)

residentCompileOne
  :: Maybe CompilerProducerIdentity -> PipelineSelection result -> Set.Set SymbolIdentity
  -> IORef CompilerUniverse -> IORef (Maybe ActiveCompilerAttempt) -> IORef InterpreterAttemptState
  -> HscEnv -> DynFlags -> Bool -> Word64 -> CompilePurpose -> Maybe SessionScope
  -> FilePath -> [FilePath] -> Maybe FilePath -> Ghc result
residentCompileOne producer selection retained universeRef active interpreterAttempt baseEnv baseFlags timing requestIdentity purpose mscope path includes products = do
  sessionT0 <- monotonicTime
  resources <- beginResourceTiming timing
  selected <- liftIO $ case mscope of
    Just scope | isSessionScopeActive scope -> sessionVariant purpose scope path
    _ -> normalVariant purpose path
  let variant = selected {pvCompilerProducer=producer}
      policy = retainedContext retained
      admitted = sourceReuseAdmitted variant mscope
      incarnation = mscope >>= ssIncarnation
      localIncarnation owner
        | isJust (parseSessionModule (moduleNameString (moduleName owner))) = incarnation
        | otherwise = Nothing
      originals = exactModuleDigests (pvExactScope variant)
  paths <- liftIO (compileSearchPaths variant includes (importPaths baseFlags))
  candidates <- liftIO $ traverse captureCandidateManifest (candidateManifestFor selection)
  universe <- liftIO (readIORef universeRef)
  borrowed <- liftIO (forkExactContextWithPackageFacts (universePackageFinder universe)
    (universeEnvironment universe))
  initialEpoch <- liftIO (compilerInterpreterEpoch <$> readIORef (universeInterpreter universe))
  targetName <- liftIO (targetModuleNameFor path)
  let targetOwner = mkModule (homeUnitAsUnit (hsc_home_unit borrowed)) targetName
      -- Only currently admitted original nodes become initially visible. Fresh
      -- source nodes are installed after this graph's dependency validation.
      originalNodes = Map.fromList [(owner,node)
        | scope <- maybe [] pure (pvExactScope variant)
        , (artifact,_,_) <- scopeInterfaces scope
        , let owner = mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
        , Just digest <- [Map.lookup (exactUnit artifact,exactModule artifact) originals]
        , Just node <- [Map.lookup (owner,digest,localIncarnation owner) (universeOriginalVersions universe)]]
      initialHomes = foldr (\node homes ->
          let home =
                if completedOriginalEpoch node == initialEpoch then completedOriginalHome node
                  else withoutBytecode (completedOriginalHome node)
          in addToHpt homes (moduleName (mi_module (hm_iface home))) home)
        emptyHomePackageTable (Map.elems originalNodes)
      attempt = installRetainedUnfoldingsPlugin policy
        ((hscUpdateHPT (const initialHomes) borrowed)
          {hsc_hooks=hsc_hooks baseEnv,hsc_logger=hsc_logger baseEnv})
      flags = configureBuildProducts baseFlags products . (\df -> df {importPaths=paths})
  memo <- liftIO (newIORef Map.empty)
  cache <- liftIO newIfaceCache
  recoveryRef <- liftIO (newIORef (recoveryContextCaches (universeRecovery universe)))
  recoveryIdentity <- liftIO RequestUnique.newUnique
  let activateRecovery sourceNodes = do
        -- The graph has selected exactly one immutable version of each owner.
        -- Package facts have the fixed universe identity; home facts are copied
        -- only from the matching version's own completed recovery context.
        caches <- selectCompilerRecoveryCaches baseEnv (universeRecovery universe)
          ([(owner,completedOriginalRecovery node) | (owner,node) <- Map.toList originalNodes]
           ++ [(owner,completedModuleRecovery node) | (owner,node) <- Map.toList sourceNodes])
        writeIORef recoveryRef caches
        writeIORef active (Just (ActiveCompilerAttempt attempt caches))
  setSession (hscUpdateFlags flags attempt {hsc_targets=[]})
  result <- runCompileCycle selection
    (TransactionCycle cache memo candidates
      (universeSourceVersions universe)
      activateRecovery (universeInterpreter universe) (universePackageFinder universe) interpreterAttempt)
    policy incarnation timing requestIdentity sessionT0 resources variant path
  final <- getSession
  recovery <- liftIO (readIORef recoveryRef)
  let context = ActiveCompilerAttempt final recovery
      recoveryContext = RecoveryContext recoveryIdentity recovery
  snapshot <- liftIO (readIORef memo)
  let unsealed = unsealedSourceInputs variant final
      indirectRefusals = Set.fromList [payloadOwner (gmePayload entry)
        | entry <- Map.elems snapshot, usesUnsealedSourceInputs unsealed final entry]
      refused
        | Set.null unsealed = Set.empty
        | otherwise = reverseDependencyClosure (sourceDependencyGraph (hsc_mod_graph final))
            (Set.union unsealed indirectRefusals)
      admittedSnapshot = Map.filter
        ((`Set.notMember` refused) . payloadOwner . gmePayload) snapshot
  -- Commit after the complete compiler operation. Failed/cancelled targets or
  -- partial dependency additions never enter this inventory.
  epoch <- liftIO (compilerInterpreterEpoch <$> readIORef (universeInterpreter universe))
  let sourceNodes = Map.fromListWith Map.union
        [(owner,Map.singleton (memoSelectionKey (gmeValidity entry))
            (Map.singleton (gmeValidity entry) (CompletedModuleVersion entry final home recoveryContext epoch (maybe [] (\product' -> [product' | preparedSiteDependenciesEquivalent
                (productPrepared product') (productPrepared product')]) (payloadProduct (gmePayload entry))))))
        | entry <- Map.elems admittedSnapshot
        , let owner = payloadOwner (gmePayload entry)
        , owner /= targetOwner
        , let home = lookupHpt (hsc_HPT final) (moduleName owner)]
      originalVersions = Map.fromList [((owner,digest,localIncarnation owner),
          CompletedOriginalVersion final home recoveryContext epoch)
        | scope <- maybe [] pure (pvExactScope variant), (artifact,_,_) <- scopeInterfaces scope
        , let owner = mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
        , Just digest <- [Map.lookup (exactUnit artifact,exactModule artifact) originals]
        , Just home <- [lookupHpt (hsc_HPT final) (moduleName owner)]
        , mi_module (hm_iface home) == owner]
      -- Store only dependencies in the reusable view. A module target and its
      -- diagnostic/hooks cells belong to this successful attempt's result.
      retainedView = hscUpdateFlags (const baseFlags) $
        hscUpdateHPT (filterHpt (\home -> mi_module (hm_iface home) /= targetOwner)) final
          {hsc_targets=[],hsc_type_env_vars=emptyKnotVars}
  liftIO $ do
    evictCompilerRecovery recovery (== targetOwner)
    writeIORef active (Just context)
    modifyIORef' universeRef (\current -> current
      { universeEnvironment=retainedView, universeRecovery=recoveryContext
      , universeSourceVersions=Map.unionWith (Map.unionWith (Map.unionWith preferModuleVersion))
          sourceNodes (universeSourceVersions current)
      , universeOriginalVersions=if admitted then Map.unionWith preferOriginalVersion
          originalVersions (universeOriginalVersions current)
          else universeOriginalVersions current })
  pure result

-- Each key owns one immutable product. Upgrade incomplete acceleration once,
-- retaining the original environment on ordinary hits across changing views.
preferModuleVersion :: CompletedModuleVersion -> CompletedModuleVersion -> CompletedModuleVersion
preferModuleVersion incoming previous = selected
  {completedModuleRecovery=completedModuleRecovery incoming
  , completedModuleProducts=foldl' addProduct (completedModuleProducts previous)
    (completedModuleProducts incoming)}
  where
    selected
      | completedModuleEpoch incoming /= completedModuleEpoch previous = incoming
      | maybe False hasCode (completedModuleHome incoming)
          && not (maybe False hasCode (completedModuleHome previous)) = incoming
      | isJust (payloadProduct (gmePayload (completedModuleEntry incoming)))
          && isNothing (payloadProduct (gmePayload (completedModuleEntry previous))) = incoming
      | otherwise = previous
    addProduct existing product'
      | not (preparedSiteDependenciesEquivalent (productPrepared product') (productPrepared product')) = existing
      | any (preparedSiteDependenciesEquivalent (productPrepared product') . productPrepared) existing = existing
      | otherwise = existing ++ [product']
    hasCode home = isJust (homeMod_bytecode (hm_linkable home)) || isJust (homeMod_object (hm_linkable home))

preferOriginalVersion :: CompletedOriginalVersion -> CompletedOriginalVersion -> CompletedOriginalVersion
preferOriginalVersion incoming previous = selected
  {completedOriginalRecovery=completedOriginalRecovery incoming}
  where
    selected
      | completedOriginalEpoch incoming /= completedOriginalEpoch previous = incoming
      | hasCode (completedOriginalHome incoming) && not (hasCode (completedOriginalHome previous)) = incoming
      | otherwise = previous
    hasCode home = isJust (homeMod_bytecode (hm_linkable home)) || isJust (homeMod_object (hm_linkable home))

-- Keep all compatible loaded code, including inactive private graphs. Only a
-- same-owner executable replacement invalidates that owner and its retained
-- dependents. Foreign/object parts need an external epoch rotation because
-- GHC's dynamic unload leaves their symbols resident.
activateCompilerInterpreter
  :: IORef CompilerInterpreterState
  -> Map.Map Module CompilerExecutableContext -> Set.Set Module -> HscEnv -> IO ()
activateCompilerInterpreter versions selected recompiled env = forM_ (hsc_interp env) $ \interp -> do
  loaded <- Linker.getLoaderState interp
  state <- readIORef versions
  let known = compilerLoadedExecutables state
      linkables = maybe [] (\state -> moduleEnvElts (Linker.objs_loaded state)
        ++ moduleEnvElts (Linker.bcos_loaded state)) loaded
      homes = Map.fromList [(mi_module (hm_iface home),home) | home <- eltsHpt (hsc_HPT env)]
      wanted owner = maybe [] homeLinkables (Map.lookup owner homes)
      native linkable = not (null (linkableObjs linkable) && null (linkableLibs linkable))
      stampConflict linkable = case filter ((== native linkable) . native)
          (wanted (linkableModule linkable)) of
        [] -> False
        candidates -> all ((/= linkableTime linkable) . linkableTime) candidates
      versionConflict owner current = case Map.lookup owner known of
        Just previous -> executableVersion previous /= executableVersion current
        -- A loaded home symbol without a recorded completed seal cannot be
        -- authenticated by equal timestamps. Package code has a fixed universe.
        Nothing -> any ((== owner) . linkableModule) linkables
      changed = Set.fromList
        ([owner | (owner,current) <- Map.toList selected, versionConflict owner current]
         ++ [linkableModule linkable | linkable <- linkables
            , stampConflict linkable || linkableModule linkable `Set.member` recompiled])
      stale = reverseDependencyClosure (Map.map executableDependencies known) changed
      incompatible = any (\linkable -> linkableModule linkable `Set.member` stale && native linkable) linkables
      keep = filter ((`Set.notMember` stale) . linkableModule) linkables
  if incompatible
    then retireCompilerInterpreter interp >> writeIORef versions
      (CompilerInterpreterState (compilerInterpreterEpoch state + 1) Map.empty)
    else do
      -- An uninitialised or unchanged loader needs no unload. Calling
      -- unload on an empty state would initialise and preload the interpreter.
      when (isJust loaded && not (Set.null stale)) (Linker.unload interp env keep)
      modifyIORef' versions (\previous -> previous
        {compilerLoadedExecutables=Map.withoutKeys (compilerLoadedExecutables previous) stale})

withoutBytecode :: HomeModInfo -> HomeModInfo
withoutBytecode home = home
  {hm_linkable=(hm_linkable home) {homeMod_bytecode=Nothing}}

homeLinkables :: HomeModInfo -> [Linkable]
homeLinkables home = catMaybes
  [homeMod_bytecode (hm_linkable home),homeMod_object (hm_linkable home)]

recordCompilerInterpreter
  :: IORef CompilerInterpreterState
  -> Map.Map Module CompilerExecutableContext -> HscEnv -> IO ()
recordCompilerInterpreter versions selected env = forM_ (hsc_interp env) $ \interp -> do
  loaded <- Linker.getLoaderState interp
  let owners = Set.fromList (maybe [] (map linkableModule . (\state ->
        moduleEnvElts (Linker.objs_loaded state) ++ moduleEnvElts (Linker.bcos_loaded state))) loaded)
  -- No partial/failed owner enters this completed interpreter inventory.
  modifyIORef' versions (\state -> state
    {compilerLoadedExecutables=Map.union (Map.restrictKeys selected owners) (compilerLoadedExecutables state)})

-- Retiring an epoch also retires its process and pipe. GHC's stopInterp
-- sends Shutdown and clears status, but leaves process reaping to its caller.
retireCompilerInterpreter :: Interp -> IO ()
retireCompilerInterpreter interp = mask $ \restore -> do
  case interpInstance interp of
    ExternalInterp (ExtIServ server) -> do
      previous <- readMVar (interpStatus server)
      case previous of
        InterpPending -> pure ()
        InterpRunning instance' -> do
          let process = instProcess instance'
              pipe = interpPipe process
              close handle = hClose handle `catch` \(_ :: IOException) -> pure ()
              reap = do
                exited <- timeout 2000000 (waitForProcess (interpHandle process))
                case exited of
                  Just _ -> pure ()
                  Nothing -> do
                    terminateProcess (interpHandle process)
                    stopped <- timeout 2000000 (waitForProcess (interpHandle process))
                    unless (isJust stopped) (throwIO CompilerTransactionPoisoned)
              shutdown = do
                -- A broken protocol must still release this exact process.
                stopped <- try (restore (timeout 2000000 (stopInterp interp)))
                  :: IO (Either SomeException (Maybe ()))
                reap
                modifyMVar_ (interpStatus server) (const (pure InterpPending))
                case stopped of
                  Left failure | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
                  _ -> pure ()
          shutdown `finally` (close (pipeRead pipe) >> close (pipeWrite pipe))
    _ -> throwIO CompilerTransactionPoisoned
  modifyMVar_ (Linker.loader_state (interpLoader interp)) (const (pure Nothing))
  purgeLookupSymbolCache interp

recoverCompilerInterpreter :: IORef CompilerInterpreterState -> HscEnv -> Bool -> IO ()
recoverCompilerInterpreter _ _ False = pure ()
recoverCompilerInterpreter versions env True = do
  -- LoaderState can have rolled back after physical loading. Retire the
  -- process from the typed unsettled boundary, independent of its registry.
  forM_ (hsc_interp env) retireCompilerInterpreter
  modifyIORef' versions (\state -> CompilerInterpreterState
    (compilerInterpreterEpoch state + 1) Map.empty)

-- Protected requests use their complete admitted search order. GHC's boot
-- defaults (including the worker CWD) are not additional source authority.
compileSearchPaths :: PipelineVariant -> [FilePath] -> [FilePath] -> IO [FilePath]
compileSearchPaths variant requested ordinaryBase = case pvExactScope variant >>= scopeIncludePaths of
  Nothing -> pure (nub (ordinaryBase ++ requested))
  Just admitted -> do
    unless (requested == admitted) (throwIO SearchInputsChanged)
    pure admitted

-- | Record target-module warnings for successful results and all source errors
-- for the shared load barrier. GHC can report a fatal warning from
-- 'load'' only through the logger and return 'Failed'; preserving it here
-- prevents the barrier from losing the source diagnostic or treating it as a
-- worker failure. Dependency errors retain their original paths and spans.
--
-- Warning capture records only @targetPath@. Cell warnings also accept GHC's
-- @<cell>@ LINE-pragmas for authored input; dependency errors are captured
-- regardless of their physical or virtual source span.
-- Rendered with 'mkLocMessage', the same formatter GHC's default log action
-- uses, so the text carries the familiar @Expr.hs:<line>:<col>: warning:
-- ...@ shape callers already parse compile errors out of. Delegates to
-- `fallback` unconditionally so normal stderr printing is unaffected — this
-- only ADDS a capture, it never suppresses.
diagnosticCollectorHook
  :: FilePath -> IORef [(String, Diag)] -> IORef [Diag] -> LogAction -> LogAction
diagnosticCollectorHook targetPath warningRef errorRef fallback flags msgClass srcSpan msg = do
  case msgClass of
    MCDiagnostic SevWarning _ _ | inTarget srcSpan ->
      modifyIORef' warningRef ((rendered, structured DiagWarning) :)
    MCDiagnostic SevError _ _ ->
      modifyIORef' errorRef (structured DiagError :)
    _ -> pure ()
  fallback flags msgClass srcSpan msg
  where
    -- The target lives in a per-invocation temporary directory which is not
    -- part of either content-addressed key. Normalize every occurrence before
    -- metadata serialization so a cache hit cannot return another process's
    -- deleted temp path, while retaining the useful module filename and exact
    -- source coordinates.
    rendered = Text.unpack $ Text.replace (Text.pack targetPath)
      (Text.pack (takeFileName targetPath)) (Text.pack renderedRaw)
    renderedRaw = renderWithContext defaultSDocContext (mkLocMessage msgClass srcSpan msg)
    structured severity = Diag
      { dFile = spanOf srcSpan
      , dSeverity = severity
      , dMessage = renderWithContext defaultSDocContext msg
      }
    inTarget (RealSrcSpan rss _) = unpackFS (srcSpanFile rss) == targetPath
      || (takeFileName targetPath == "CellCheck.hs"
          && unpackFS (srcSpanFile rss) == "<cell>")
    inTarget _ = False

-- | The session-setup DynFlags transform, applied once by 'runCompile' and so
-- shared by BOTH variants: 'canonicalizeDFlags' + the genericPlatform spoof +
-- exposing the @ghc@ package + clearing host SIMD. The produced 'DynFlags' is
-- byte-for-byte what the normal path always built. See the long commentary at
-- the 'runCompile' call site for the rationale of each field.
extractionDynFlags :: DynFlags -> [FilePath] -> DynFlags
extractionDynFlags dflags includes = canonicalizeDFlags dflags
  { importPaths = importPaths dflags ++ includes
  -- Producer identity binds this libdir's complete pinned global database.
  -- GHC stores database flags in reverse command-line order. Clearing first
  -- removes both user databases and ambient GHC_PACKAGE_PATH entries before
  -- the initial setSessionDynFlags loads any package interfaces.
  , packageDBFlags = [PackageDB GlobalPkgDb, ClearPackageDBs]
  , packageFlags = packageFlags dflags
      ++ [ExposePackage "-package ghc" (PackageArg "ghc")
                        (ModRenaming True [])]
  , targetPlatform = genericPlatform
  , sseVersion = Nothing
  , bmiVersion = Nothing
  , avx = False
  , avx2 = False
  , avx512cd = False
  , avx512er = False
  , avx512f = False
  , avx512pf = False
  }

-- | Direct compiler outputs to a request-owned persistent directory so GHC's
-- recompilation checker can reuse unchanged interfaces. A missing directory
-- leaves the caller's baseline flags unchanged.
configureBuildProducts :: DynFlags -> Maybe FilePath -> DynFlags -> DynFlags
configureBuildProducts baseline mDir dflags = case mDir of
    Nothing  -> resetWriteInterface
      { hiDir = hiDir baseline
      , objectDir = objectDir baseline
      }
    Just dir -> (`gopt_set` Opt_WriteInterface) dflags
      { hiDir = Just dir
      , objectDir = Just dir
      }
  where
    resetWriteInterface
      | gopt Opt_WriteInterface baseline = gopt_set dflags Opt_WriteInterface
      | otherwise = gopt_unset dflags Opt_WriteInterface

-- | Whether a finalized interface must enter the current HPT for a later
-- importer. Every durable owner retains its interface even without an importer.
needsPreparedInterface :: HomeInterfaceUse -> Bool
needsPreparedInterface HomeInterfaceLeaf = False
needsPreparedInterface _ = True

registerPreparedInterface :: Bool -> Word64 -> InterfaceReuse -> HomeInterfaceUse
  -> ModuleFront -> ModGuts
  -> Ghc (Integer, FinalizedModule)
registerPreparedInterface timing requestId interfaceReuse interfaceUse front simplified = do
  let modSum = mfSummary front
      tcGblEnv = mfTcGblEnv front
      hscEnv = mfHscEnv front
  ((cgGuts, modDetails), tidyMs) <- timeSection $ liftIO $ hscTidy hscEnv simplified
  liftIO $ emitModuleInterfaceTiming timing (moduleNameString (ms_mod_name modSum))
    "module_interface" "tidy" tidyMs
  (iface0, ifaceMs) <- liftIO $ measureModuleInterface timing requestId
    (moduleNameString (ms_mod_name modSum)) SessionRegistrationInterface interfaceReuse $
      mkFullIface hscEnv (force (mkPartialIface hscEnv (cg_binds cgGuts) modDetails
        modSum (tcg_import_decls tcGblEnv) simplified)) Nothing Nothing NoStubs []
  let iface = set_mi_extra_decls Nothing iface0
      hmi = HomeModInfo iface modDetails emptyHomeModInfoLinkable
  when (needsPreparedInterface interfaceUse) $
    installPreparedInterface (ms_mod_name modSum) hmi
  pure (tidyMs + ifaceMs, FinalizedModule hmi cgGuts)

-- Keep request-local executable state out of the reusable prepared memo.
installPreparedInterface :: ModuleName -> HomeModInfo -> Ghc ()
installPreparedInterface name hmi = do
  current <- getSession
  let linkable = maybe emptyHomeModInfoLinkable hm_linkable
        (lookupHpt (hsc_HPT current) name)
      skinny = hmi { hm_iface = set_mi_extra_decls Nothing (hm_iface hmi)
                   , hm_linkable = linkable }
  setSession (hscUpdateHPT (\hpt -> addToHpt hpt name skinny) current)

-- | Ordinary compilation has no value injection and selects prepared STG
-- from the pre-optimization Core reference closure.
normalVariant :: CompilePurpose -> FilePath -> IO PipelineVariant
normalVariant purpose path = do
  let effectivePurpose = originalPurpose purpose
  case effectivePurpose of
    HostActivationPreviewCompile {} -> fail "activation preview requires its sealed session admission"
    _ -> pure ()
  targetModName' <- targetModuleNameFor path
  pure PipelineVariant
   { pvLabel = "runPipeline"
   , pvPurpose = purpose
   , pvExactScope = Nothing
   , pvCompilerProducer = Nothing
   , pvGeneratedScaffold = generatedRecipe purpose
   , pvGeneratedInstanceCheck = generatedInstanceRecipe purpose
   , pvSourceImportIntents = sourceImportIntents purpose
   , pvDownsweepExcludes = []
   , pvTransformParsed = transformFor purpose targetModName'
   , pvPlan = \_compilerViewDirectory _timing modGraphRaw _selectedExact -> pure CompilePlan
      { cpLoadGraph = modGraphRaw
      , cpAfterLoad = pure ()
        -- Consume load captures and finalize any deferred source in dependency
        -- order before selecting prepared STG.
      , cpSummaries = pure
          [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing) ]
        -- 'runPipeline' (single-shot eval) always compiles a target named
        -- @result@; a resident turn without prior bindings can also land on
        -- this variant while its template names @__result@. Try both, in that
        -- order.
      , cpKeepPrivateResult = effectivePurpose == OriginalDeclarationCompile
      , cpResultBinders = [scaffoldOutputBase, scaffoldTargetName]
      , cpBeforeModule = \_ -> pure ()
      , cpBeforeMerge = \_ -> pure ()
      , cpInjectedSessionInterfaces = pure []
      , cpFinalEnv = id
      }
  }

-- A retained compiler module owns a canonical artifact, not a native product
-- or permission to import it lexically. The graph exists only for GHC linking.
data RetainedCompilerModule = RetainedCompilerModule
  { retainedCompilerArtifact :: ExactIfaceArtifact
  , retainedCompilerAdmission :: CanonicalInterfaceAdmission
  , retainedCompilerSummary :: ModSummary
  , retainedCompilerDependencies :: [NodeKey]
  }

data FinalizedExecutionFailure
  = FinalizedExecutionOwnerMissing (String, String)
  | FinalizedExecutionDuplicateOwner (String, String)
  | FinalizedExecutionInterfaceMismatch (String, String)
  | FinalizedExecutionDependencyMismatch (String, String) (String, String)
  | FinalizedExecutionCycle (String, String)
  | FinalizedExecutionCoreMissing (String, String)
  | FinalizedExecutionHomeMissing (String, String)
  deriving (Eq, Show)

instance Exception FinalizedExecutionFailure

data ExactExecutionPlan = ExactExecutionPlan
  { executionLoadGraph :: ModuleGraph
  , executionOriginalModules :: [RetainedCompilerModule]
  , executionFreshProviders :: [ModSummary]
  , executionLinkGraph :: ModuleGraph
  }

-- Canonical interface/Core selection is independent of optional source replay
-- recipes. Dependencies are selected only through admitted interface seals.
data RetainedCompilerArtifact = RetainedCompilerArtifact
  { retainedCompilerInterface :: ExactIfaceArtifact
  , retainedArtifactAdmission :: CanonicalInterfaceAdmission
  , retainedArtifactDependencies :: [ExactIfaceArtifact]
  }

retainedCompilerArtifactClosure
  :: ExactScope -> [ExactIfaceArtifact] -> [(String, String)]
  -> Either FinalizedExecutionFailure [RetainedCompilerArtifact]
retainedCompilerArtifactClosure scope interfaces roots = do
  _ <- foldM uniqueOwner Set.empty interfaces
  (_, modules) <- foldM (visit Set.empty) (Set.empty, []) roots
  pure (reverse modules)
  where
    admissions = scopeSourceOriginalInterfaces scope
    admittedArtifacts = Map.fromList [((exactUnit artifact, exactModule artifact), artifact)
      | (artifact,_,_) <- scopeInterfaces scope]
    uniqueOwner owners artifact =
      let key = (exactUnit artifact,exactModule artifact)
      in if key `Set.member` owners then Left (FinalizedExecutionDuplicateOwner key)
         else Right (Set.insert key owners)
    artifacts = Map.fromList [((exactUnit artifact, exactModule artifact), artifact)
      | artifact <- interfaces]
    visit active state@(completed, _) key
      | isReservedSessionModuleName (snd key) = Left (FinalizedExecutionOwnerMissing key)
      | key `Set.member` active = Left (FinalizedExecutionCycle key)
      | key `Set.member` completed = Right state
      | otherwise = do
          admission <- maybe (Left (FinalizedExecutionOwnerMissing key)) Right (Map.lookup key admissions)
          artifact <- maybe (Left (FinalizedExecutionOwnerMissing key)) Right (Map.lookup key artifacts)
          admitted <- maybe (Left (FinalizedExecutionOwnerMissing key)) Right (Map.lookup key admittedArtifacts)
          unless (exactSha256 artifact == exactSha256 admitted) $
            Left (FinalizedExecutionInterfaceMismatch key)
          let requirements = Map.toAscList (admittedInterfaceRequirements admission)
          dependencyArtifacts <- forM requirements $ \(required, seal) -> case Map.lookup required artifacts of
            Just dependency | exactSha256 dependency == seal -> Right dependency
            _ -> Left (FinalizedExecutionDependencyMismatch key required)
          (closed, dependencies) <- foldM (visit (Set.insert key active)) state (map fst requirements)
          let selected = RetainedCompilerArtifact artifact admission dependencyArtifacts
          pure (Set.insert key closed, selected : dependencies)

-- Close only compiler execution demand. Every edge is an already admitted
-- canonical interface seal; no native reachability, source path or candidate
-- ordering can manufacture a missing compiler owner.
retainedCompilerClosure
  :: FilePath -> HscEnv -> ExactScope -> [(ExactIfaceArtifact, ModIface)]
  -> [(String, String)] -> Either FinalizedExecutionFailure [RetainedCompilerModule]
retainedCompilerClosure directory env scope interfaces roots = do
  selected <- retainedCompilerArtifactClosure scope (map fst interfaces) roots
  pure [stage index artifact | (index, artifact) <- zip [0 :: Int ..] selected]
  where
    stage index selected =
      let artifact = retainedCompilerInterface selected
          summary = exactInterfaceSummary env artifact
          prefix = directory </> ("retained-" ++ show index)
          location = (ms_location summary)
            { ml_hi_file = prefix ++ ".hi", ml_dyn_hi_file = prefix ++ ".dyn_hi"
            , ml_obj_file = prefix ++ ".o", ml_dyn_obj_file = prefix ++ ".dyn_o"
            , ml_hie_file = prefix ++ ".hie" }
          dependencyKeys = [mkNodeKey (ModuleNode [] (exactInterfaceSummary env dependency))
            | dependency <- retainedArtifactDependencies selected]
      in RetainedCompilerModule artifact (retainedArtifactAdmission selected)
          (summary {ms_location = location}) dependencyKeys


-- Linker hooks and diagnostic collectors belong to one cycle. Restore both
-- even when refusal or cancellation prevents the final environment handoff.
withCycleHooks :: Ghc a -> Ghc a
withCycleHooks action = reifyGhc $ \session -> bracket
  (reflectGhc ((\env -> (hsc_hooks env, hsc_logger env)) <$> getSession) session)
  (\(hooks, logger) -> reflectGhc
    (getSession >>= \env -> setSession env {hsc_hooks=hooks, hsc_logger=logger}) session)
  (const (reflectGhc action session))

-- Make owns temporary interface cleanup. Every candidate view is staged in
-- this private request directory, retained through deferred target checking,
-- then removed on success, refusal or cancellation.
withCompilerViewDirectory :: (FilePath -> Ghc a) -> Ghc a
withCompilerViewDirectory action = reifyGhc $ \session -> bracket
  (getTemporaryDirectory >>= \directory -> mkdtemp (directory </> "tidepool-compiler-view.XXXXXX"))
  removeDirectoryRecursive
  (\directory -> reflectGhc (action directory) session)

-- A denied original selection is an input refusal, distinct from authored
-- GHC errors and from optional compile-time execution failures.
withSourceSelectionRefusal :: Ghc a -> Ghc a
withSourceSelectionRefusal action = reifyGhc $ \session ->
  reflectGhc action session `catch` refuseOriginal `catch` refuseInput
  where
    refuseOriginal :: ExecutionSourceFailure -> IO a
    refuseOriginal = throwIO . OriginalSourceSelectionRejected
    refuseInput :: IOException -> IO a
    refuseInput = throwIO . OriginalSourceSelectionInputUnavailable . show

-- The original import is owned by a protected checked recipe, never by the
-- persistent lexical surface or the mere presence of its interface.
checkedRecipeOriginal :: ExactScope -> Either String (Maybe ((String,String),String))
checkedRecipeOriginal admitted = do
  let original = scopeCheckedItem admitted >>= itemPlannedDeclaration
  forM_ original $ \(owner,_) -> case Map.lookup owner (scopeInterfaceEvidence admitted) of
    Just (ModuleInterfaceEvidence proof) -> case canonicalOrigin proof of
      NativeAuthoredDeclaration _ -> Right ()
      _ -> Left "checked recipe original lacks native declaration origin"
    Just (LocalNativeDeclarationEvidence _) -> Right ()
    _ -> Left "checked recipe original lacks its admitted native declaration proof"
  pure original

-- Explicit authored imports demand current canonical source proof even for a captured
-- lexical owner. Generated imports and use of captured names keep that owner.
-- Available retained requirements alone never create a source demand.
selectCurrentSourceOriginals
  :: ExactScope -> Maybe ProgramSourceImports -> [ImportIntent] -> Maybe GeneratedScaffoldRecipe -> ModuleGraph
  -> Ghc (Maybe SourceSelectedOriginals)
selectCurrentSourceOriginals admitted completed intents recipe sourceGraph = do
  initial <- getSession
  forM_ completed (either (liftIO . fail) pure . validateProgramSourceImports admitted)
  let home = homeUnitId (hsc_home_unit initial)
      exactOwners = Map.fromList [((exactUnit artifact,exactModule artifact),artifact)
        | (artifact,_,_) <- scopeInterfaces admitted]
      lexicalOwners = Set.fromList (map fst (scopeLexical admitted))
      checkedOwners = Set.fromList [(exactUnit artifact,exactModule artifact)
        | artifact <- scopeValueInterfaces admitted]
      sourceSummaries = [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph]
      localOwner (qualifier,name) = case qualifier of
        NoPkgQual -> Just (unitString home,moduleNameString (unLoc name))
        ThisPkg unit | unit == home -> Just (unitString unit,moduleNameString (unLoc name))
        _ -> Nothing
      covered = case completed of
        Nothing -> []
        Just (ProgramSourceImports _ requests _) -> requests
      covers intent@(AuthoredSourceImport owner qualifier) = any (\(request,key) ->
        request == intent && localOwner
          (renameRawPkgQual (hsc_unit_env initial) owner qualifier,noLoc owner) == Just key
          && maybe False (Set.null . (`Set.intersection` scopeSourceSelectedOwners admitted) . Map.keysSet)
            (programImportClosure admitted key)) covered
      covers RetainedGeneratedImport = False
      currentIntents = filter (not . covers) intents
      requestedOwners = Set.fromList
        [key | AuthoredSourceImport owner qualifier <- currentIntents
        , Just key <- [localOwner (renameRawPkgQual (hsc_unit_env initial) owner qualifier, noLoc owner)]]
      freshImports = Map.fromList
        [((unitString (moduleUnit (ms_mod summary)),moduleNameString (ms_mod_name summary)),
          Set.fromList [key | imported <- ms_textual_imps summary ++ ms_srcimps summary
            , Just key <- [localOwner imported]])
        | summary <- sourceSummaries]
      -- A fresh authored provider's imports are current source demands too.
      -- Only its actual GHC downsweep edges propagate demand; retained lexical
      -- edges and generated target imports never seed this closure.
      sourceDemands selected =
        let grown = Set.unions (selected :
              [imports | key <- Set.toList selected, sourceProvider key
                , Just imports <- [Map.lookup key freshImports]])
         in if grown == selected then selected else sourceDemands grown
      authoredOwners = Set.filter currentProvider (sourceDemands requestedOwners)
      sourceProvider key = case Map.lookup key (scopeInterfaceEvidence admitted) of
        Nothing -> True -- A fresh provider can introduce an already retained dependency.
        Just (ModuleInterfaceEvidence proof) -> isSourceOriginal (canonicalOrigin proof)
        _ -> False
      currentProvider key = Map.member key exactOwners && sourceProvider key
      hiddenImports = [(summary,imported,key) | summary <- sourceSummaries
        , imported <- ms_textual_imps summary ++ ms_srcimps summary
        , Just key <- [localOwner imported], Map.member key exactOwners
        , (key `Set.notMember` lexicalOwners || key `Set.member` scopeSourceSelectedOwners admitted
          || key `Set.member` authoredOwners)
        , key `Set.notMember` checkedOwners]
  if null hiddenImports then pure Nothing else do
    closure' <- liftIO (readVerifiedExactIfaceClosureWithCheckedValues initial
      (Map.elems exactOwners) (scopeValueInterfaces admitted))
      >>= either (liftIO . fail) pure
    interfaces <- either (liftIO . fail) pure
      (selectVerifiedExactInterfaces closure' (Map.elems exactOwners))
    hydrated <- liftIO (hydrateExactScope initial interfaces)
    scaffold <- case recipe of
      Nothing -> pure noGeneratedScaffoldImports
      Just protected -> do
        candidates <- filterM (\summary -> case ml_hs_file (ms_location summary) of
          Nothing -> pure False
          Just source -> liftIO (captureGeneratedScaffoldTarget protected source)
            >>= pure . either (const False) (const True)) sourceSummaries
        case candidates of
          [summary] -> do
            parsed <- parseModule summary
            original <- either (liftIO . fail) pure (checkedRecipeOriginal admitted)
            liftIO (readGeneratedScaffoldImportAuthority closure'
              (scopeExecutionNativeOwners admitted) original protected parsed sourceGraph hydrated)
              >>= either (liftIO . fail) pure
          _ -> liftIO (fail "generated scaffold target summary is missing or duplicated")
    let roots = Set.toAscList (Set.fromList [key | (summary,imported,key) <- hiddenImports
          , key `Set.member` authoredOwners || not (permitsGeneratedScaffoldImport scaffold summary key imported)])
    case roots of
      [] -> pure Nothing
      root : _ -> do
        includes <- maybe (liftIO (throwIO (ExecutionSourceUnavailable root))) pure
          (scopeIncludePaths admitted)
        unless (all ((== includes) . importPaths . ms_hspp_opts) sourceSummaries) $
          liftIO (throwIO (ExecutionSourceUnsupported root))
        (validation,selectedKeys) <- validateCurrentCanonicalSources admitted interfaces sourceGraph roots
        let selectedGraph = mkModuleGraph [node | node@(ModuleNode _ summary) <-
              mgModSummaries' (validatedOriginalGraph validation)
              , (unitString (moduleUnit (ms_mod summary)),moduleNameString (ms_mod_name summary)) `Set.member` selectedKeys]
            current = validatedOriginalEvidence validation
            selectedModules = [node | node <- dependencyModules current
              , (dependencyModuleUnit node,dependencyModuleName node) `Set.member` selectedKeys]
            relevantImports = Set.fromList
              [(dependencyImportQualifier edge,dependencyImportName edge,dependencyImportBoot edge)
              | node <- selectedModules, edge <- dependencyModuleImports node]
              `Set.union` Set.fromList
                [(dependencyQualifier qualifier,moduleNameString (unLoc name),False)
                | (summary,imported@(qualifier,name),key) <- hiddenImports
                , key `Set.member` selectedKeys
                , key `Set.member` authoredOwners || not (permitsGeneratedScaffoldImport scaffold summary key imported)]
            relevant resolution = (dependencyResolutionQualifier resolution,dependencyResolutionModule resolution,
              dependencyResolutionBoot resolution) `Set.member` relevantImports
        unless (all ((== includes) . importPaths . ms_hspp_opts)
            [summary | ModuleNode _ summary <- mgModSummaries' selectedGraph]) $
          liftIO (throwIO (ExecutionSourceUnsupported root))
        (sources,complete) <- liftIO (captureDependencySources selectedGraph)
        unless complete $ liftIO (throwIO (ExecutionSourceChangedDuring root CurrentSourceSelectionIncomplete))
        forM_ (filter relevant (dependencyResolutions current)) $ \resolution -> do
          let negative = case dependencyResolutionSelected resolution of
                Nothing -> dependencyResolutionCandidates resolution
                Just chosen -> takeWhile (/= chosen) (dependencyResolutionCandidates resolution)
          absent <- liftIO (and <$> mapM (fmap not . doesFileExist) negative)
          unless absent $ liftIO (throwIO (ExecutionSourceResolutionChanged root))
        let evidence = current
              { dependencyCacheSafe=True, dependencySelectionComplete=True
              , dependencySources=sources, dependencyModules=selectedModules
              , dependencyResolutions=filter relevant (dependencyResolutions current)
              , dependencyPackages=Set.toAscList (Set.fromList
                  [dependencyImportName edge | node <- selectedModules, edge <- dependencyModuleImports node
                  , isNothing (dependencyImportSelected edge)]) }
            selected = SourceSelectedOriginals
              [(key,canonicalCertificateSha256 proof,exactSha256 (exactOwners Map.! key),canonicalSourceSha256 proof)
              | key <- Set.toAscList selectedKeys, let proof = scopeModuleInterfaceProofs admitted Map.! key] evidence
        setSession initial
        pure (Just selected)

data ValidatedOriginalSources = ValidatedOriginalSources
  { validatedOriginalGraph :: ModuleGraph
  , validatedOriginalEvidence :: DependencyEvidence
  }

-- Current source lookup authenticates canonical interfaces, never native
-- products or execution recipes. GHC's exact-interface compatibility check
-- retains usage/type requirements without making them lexical source imports.
validateCurrentCanonicalSources
  :: ExactScope -> [(ExactIfaceArtifact, ModIface)] -> ModuleGraph -> [(String,String)]
  -> Ghc (ValidatedOriginalSources,Set.Set (String,String))
validateCurrentCanonicalSources admitted interfaces sourceGraph roots = do
  initial <- getSession
  let ownerKey owner = (unitString (moduleUnit owner),moduleNameString (moduleName owner))
      proofs = Map.filter (isSourceOriginal . canonicalOrigin) (scopeModuleInterfaceProofs admitted)
      originals = Map.fromList [((exactUnit artifact,exactModule artifact),iface) | (artifact,iface) <- interfaces]
      excluded = [mkModuleName (exactModule artifact) | (artifact,_,_) <- scopeInterfaces admitted
        , Map.notMember (exactUnit artifact,exactModule artifact) proofs]
        ++ [mkModuleName (exactModule artifact) | artifact <- scopeValueInterfaces admitted]
  finder <- liftIO initFinderCache
  setSession initial {hsc_FC=finder}
  currentEnv <- getSession
  targets <- forM roots $ \key@(_,name) -> do
    unless (Map.member key proofs && not ("Tidepool.Session." `isPrefixOf` name)) $
      liftIO (throwIO (ExecutionSourceIncomplete key))
    found <- liftIO (findImportedModule currentEnv (mkModuleName name) NoPkgQual)
    source <- case found of
      Found location owner | ownerKey owner == key -> maybe
        (liftIO (throwIO (ExecutionSourceUnavailable key))) pure (ml_hs_file location)
      _ -> liftIO (throwIO (ExecutionSourceUnavailable key))
    target <- guessTarget source Nothing Nothing
    pure target {targetAllowObjCode=False}
  setTargets (hsc_targets initial ++ targets)
  graph <- depanalSourceModules excluded
  env <- getSession
  current <- liftIO (dependencyEvidenceFor env ([],True) graph [])
  let summaries = Map.fromList [(ownerKey (ms_mod summary),summary)
        | ModuleNode _ summary <- mgModSummaries' graph, ms_hsc_src summary == HsSrcFile]
      modules = Map.fromList [((dependencyModuleUnit node,dependencyModuleName node),node)
        | node <- dependencyModules current, not (dependencyModuleBoot node)]
      selectedClosure selectedOwners =
        let children = [key | owner <- Set.toList selectedOwners, Just node <- [Map.lookup owner modules]
              , edge <- dependencyModuleImports node, dependencyImportSelected edge /= Nothing
              , let key = (fst owner,dependencyImportName edge), Map.member key proofs]
            grown = Set.union selectedOwners (Set.fromList children)
         in if grown == selectedOwners then selectedOwners else selectedClosure grown
      selected = selectedClosure (Set.fromList roots)
      fresh = Map.fromList [(ownerKey (ms_mod summary),summary)
        | ModuleNode _ summary <- mgModSummaries' sourceGraph, Map.notMember (ownerKey (ms_mod summary)) originals]
  forM_ (Map.toList fresh) $ \(key,old) -> forM_ (Map.lookup key summaries) $ \now -> do
    oldPath <- liftIO (traverse canonicalizePath (ml_hs_file (ms_location old)))
    nowPath <- liftIO (traverse canonicalizePath (ml_hs_file (ms_location now)))
    unless (ms_mod old == ms_mod now && oldPath == nowPath && ms_hs_hash old == ms_hs_hash now) $
      liftIO (throwIO (ExecutionSourceIncomplete key))
  native <- liftIO (hydrateExactScope env interfaces)
  let nativeEnv = native {hsc_mod_graph=mapMG (\summary -> summary
        {ms_hspp_opts=canonicalizeDFlags (ms_hspp_opts summary)}) graph}
  forM_ (Set.toAscList selected) $ \key -> do
    proof <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key proofs)
    summary <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key summaries)
    node <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key modules)
    source <- liftIO (canonicalizePath (dependencyModuleSource node))
    (sourceProof,fingerprint) <- liftIO (sourceEvidenceWithFingerprint source)
    unless (dependencySourceSha256 sourceProof == canonicalSourceSha256 proof && fingerprint == ms_hs_hash summary) $
      liftIO (throwIO (ExecutionSourceChangedDuring key
        (OriginalSourceBytesChanged source (canonicalSourceSha256 proof) (dependencySourceSha256 sourceProof))))
    quotes <- if xopt LangExt.QuasiQuotes (ms_hspp_opts summary)
      then quasiQuoteOccurrences (ms_hspp_opts summary) . pm_parsed_source <$> parseModule summary
      else pure []
    unless (not (hasUnconditionallyUntrackedCompileTimeExecution (ms_hspp_opts summary))
        && not (gopt Opt_Pp (ms_hspp_opts summary)) && not (xopt LangExt.StaticPointers (ms_hspp_opts summary))
        && null quotes && not (any dependencyImportBoot (dependencyModuleImports node))) $
      liftIO (throwIO (ExecutionSourceUnsupported key))
    originalImports <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (canonicalSourceImports proof)
    currentImports <- forM [(boot,edge) | (boot,edges) <-
        [(False,ms_textual_imps summary),(True,ms_srcimps summary)], edge <- edges] $ \(boot,(qualifier,name)) -> do
      resolved <- liftIO (findImportedModule env (unLoc name) qualifier)
      home <- case resolved of
        Found _ owner | isHomeUnit (hsc_home_unit env) (moduleUnit owner) -> pure (Just (unitString (moduleUnit owner)))
        Found _ _ -> pure Nothing
        _ -> liftIO (throwIO (ExecutionSourceResolutionChanged key))
      pure (dependencyQualifier qualifier,moduleNameString (unLoc name),boot,home)
    unless (Set.toAscList (Set.fromList currentImports) == originalImports) $
      liftIO (throwIO (ExecutionSourceResolutionChanged key))
    packageProof <- case [(artifact,path,sha) | (artifact,path,sha) <- scopeInterfaces admitted
        , (exactUnit artifact,exactModule artifact) == key] of
      [(artifact,path,sha)] -> liftIO (readPackageImports path sha artifact)
        >>= either (const (liftIO (throwIO (ExecutionSourcePackageChanged key)))) pure
      _ -> liftIO (throwIO (ExecutionSourceIncomplete key))
    forM_ (ms_textual_imps summary) $ \(qualifier,name) -> do
      resolved <- liftIO (findImportedModule env (unLoc name) qualifier)
      case resolved of
        Found _ owner | isHomeUnit (hsc_home_unit env) (moduleUnit owner) -> pure ()
        Found _ owner | owner == gHC_PRIM -> unless (CompilerPrimitive `elem` compilerProvided packageProof) $
          liftIO (throwIO (ExecutionSourcePackageChanged key))
        Found _ owner -> do
          package <- liftIO (packageImportRoot env owner) >>= either
            (const (liftIO (throwIO (ExecutionSourcePackageChanged key)))) pure
          unless (package `elem` packageInterfaces packageProof) $
            liftIO (throwIO (ExecutionSourcePackageChanged key))
        _ -> liftIO (throwIO (ExecutionSourcePackageChanged key))
    iface <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key originals)
    let canonical = summary {ms_hspp_opts=canonicalizeDFlags (ms_hspp_opts summary)}
    decision <- liftIO (checkOldIface (scopeRetainedSummaryHscEnv canonical nativeEnv) canonical (Just iface))
    case decision of
      UpToDateItem _ -> pure ()
      OutOfDateItem reason _ -> liftIO (throwIO (ExecutionSourceChangedDuring key
        (OriginalInterfaceRecompileRequired (ExecutionSourceInterfaceReason reason))))
  setSession env {hsc_targets=hsc_targets initial}
  pure (ValidatedOriginalSources graph current,selected)


planExactExecutionLoad
  :: FilePath -> ExactScope -> [(ExactIfaceArtifact, ModIface)] -> [(ExactIfaceArtifact, ModIface)] -> ModuleName
  -> ModuleGraph -> ModuleGraph -> Ghc ExactExecutionPlan
planExactExecutionLoad directory admitted interfaces checkedInterfaces targetName sourceGraph ordinaryLoad = do
  initial <- getSession
  parsed <- mapM parseModule [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph
    , xopt LangExt.QuasiQuotes (ms_hspp_opts summary)]
  let quoted = [(parsedModule, occurrences)
        | parsedModule <- parsed
        , let occurrences = quasiQuoteOccurrences (ms_hspp_opts (pm_mod_summary parsedModule)) (pm_parsed_source parsedModule)
        , not (null occurrences)]
      home = homeUnitId (hsc_home_unit initial)
      ownerKey owner = (unitString (moduleUnit owner), moduleNameString (moduleName owner))
      originalByOwner = Map.fromList [((exactUnit artifact,exactModule artifact),iface)
        | (artifact,iface) <- interfaces]
      demandByOwner = Map.union originalByOwner (Map.fromList
        [((exactUnit artifact,exactModule artifact),iface) | (artifact,iface) <- checkedInterfaces])
      localImport (qualifier, name) = case qualifier of
        NoPkgQual -> Just (unitString home,moduleNameString (unLoc name))
        ThisPkg unit | unit == home -> Just (unitString unit,moduleNameString (unLoc name))
        _ -> Nothing
      candidates imports rdr = case rdr of
        Qual qualifier occurrence -> (occurrence,
          [decl | L _ decl <- imports
            , fromMaybe (unLoc (ideclName decl)) (fmap unLoc (ideclAs decl)) == qualifier])
        Unqual occurrence -> (occurrence,
          [decl | L _ decl <- imports
            , ideclQualified decl == NotQualified])
        _ -> (rdrNameOcc rdr,[])
      defining parsedModule rdr =
        let summary = pm_mod_summary parsedModule
            (occurrence,declarations) = candidates (hsmodImports (unLoc (pm_parsed_source parsedModule))) rdr
        in fmap concat $ forM [(importedKey,decl,iface) | imported <- ms_textual_imps summary
            , decl <- declarations
            , getLoc (snd imported) == getLocA (ideclName decl)
            , Just importedKey <- [localImport imported]
            , Just iface <- [Map.lookup importedKey demandByOwner]] $ \(importedKey,decl,iface) -> do
              names <- either (const (liftIO (throwIO (ExecutionSourceUnsupported importedKey)))) pure
                (selectedImportNames (mi_exports iface) (ideclImportList decl))
              pure [key | name <- names, nameOccName name == occurrence
                , Just owner <- [nameModule_maybe name]
                , let key = ownerKey owner, Map.member key demandByOwner]
      providers = [key | ModuleNode _ summary <- mgModSummaries' sourceGraph
        , ms_mod_name summary /= targetName
        , backendGeneratesCode (backend (ms_hspp_opts summary))
        , imported <- ms_textual_imps summary, Just key <- [localImport imported]
        , Map.member key demandByOwner]
  definers <- concat <$> mapM (\(parsedModule,occurrences) -> concat <$> mapM (defining parsedModule) occurrences) quoted
  let roots = if null quoted then [] else Set.toAscList (Set.fromList (providers ++ definers))
  modules <- either (liftIO . throwIO) pure
    (retainedCompilerClosure directory initial admitted interfaces roots)
  let moduleNodes = [ModuleNode (retainedCompilerDependencies selected)
          (retainedCompilerSummary selected) | selected <- modules]
      ordinaryNames = Set.fromList [ms_mod_name summary | ModuleNode _ summary <- mgModSummaries' ordinaryLoad]
      freshProviders = [summary | not (null roots)
        , ModuleNode _ summary <- flattenSCCs (topSortModuleGraph True sourceGraph Nothing)
        , ms_mod_name summary `Set.notMember` ordinaryNames
        , ms_mod_name summary /= targetName
        , backendGeneratesCode (backend (ms_hspp_opts summary))]
      admittedNames = Set.fromList [ms_mod_name (retainedCompilerSummary selected) | selected <- modules]
      excluded = [mkModuleName (exactModule artifact) | (artifact,_,_) <- scopeInterfaces admitted
        , mkModuleName (exactModule artifact) `Set.notMember` admittedNames]
        ++ [mkModuleName (exactModule artifact) | artifact <- scopeValueInterfaces admitted]
      deferred names =
        let grown = names `Set.union` Set.fromList [ms_mod_name summary
              | ModuleNode _ summary <- mgModSummaries' sourceGraph
              , any ((`Set.member` names) . unLoc . snd) (ms_textual_imps summary)]
        in if grown == names then names else deferred grown
      blocked = deferred (Set.fromList (targetName : excluded))
  forM_ freshProviders $ \summary -> unless
      (ms_mod_name summary `Set.notMember` blocked
        && null (ms_srcimps summary)
        && not (any (\case ModuleNode _ boot -> ms_hsc_src boot == HsBootFile && ms_mod boot == ms_mod summary
                           _ -> False) (mgModSummaries' sourceGraph))
        && not (xopt LangExt.StaticPointers (ms_hspp_opts summary))) $
      liftIO (throwIO (ExecutionSourceUnsupported (ownerKey (ms_mod summary))))
  pure (ExactExecutionPlan ordinaryLoad modules freshProviders (mkModuleGraph moduleNodes))

sessionVariant :: CompilePurpose -> SessionScope -> FilePath -> IO PipelineVariant
sessionVariant purpose scope path = do
  targetModName' <- targetModuleNameFor path
  completedValuesRef <- newIORef Nothing
  let effectivePurpose = originalPurpose purpose
      completedValues = case effectivePurpose of
        CheckedItemCompile _ _ values -> values
        ProgramItemCompile _ _ _ values -> values
        _ -> []
  exact <- case purposeExactScope purpose of
    Just admitted -> do
      -- The compiler entry revalidates this manifest's current digest and
      -- entire closure before exposing it to compiler/plugin execution.
      -- Path equality also binds the carried scope to this SessionScope offer.
      unless (ssExactScope scope == Just (scopeManifestPath admitted))
        (ioError (userError "planned declaration leaves its original compiler offer"))
      pure (Just admitted)
    Nothing -> traverse (\manifest -> readExactScope manifest >>= either (ioError . userError) pure)
      (ssExactScope scope)
  let previewPurpose = case effectivePurpose of
        HostActivationPreviewCompile signature -> Just signature
        _ -> Nothing
      previewAdmission = previewInputSignature <$> (exact >>= scopeActivationPreview)
  unless (previewPurpose == previewAdmission)
    (fail "activation preview has another compiler purpose")
  -- The injected source-less @Val.G<g>@ modules: excluded from the
  -- downsweep (no source to summarise) — a deferred module's @import@ of
  -- them resolves from the HPT entry 'cpBeforeModule' registers immediately
  -- before the importing source module is compiled.
  let excludedVal = map renderSessionModule (ssValIfaces scope)
      excludedExact = [mkModuleName (exactModule iface)
        | admitted <- maybe [] pure exact, (iface, _, _) <- scopeInterfaces admitted]
      excludedOwners = excludedVal ++ excludedExact
  pure PipelineVariant
   { pvLabel = "runSessionPipeline"
   , pvPurpose = purpose
   , pvExactScope = exact
   , pvCompilerProducer = Nothing
   , pvGeneratedScaffold = generatedRecipe purpose
   , pvGeneratedInstanceCheck = generatedInstanceRecipe purpose
   , pvSourceImportIntents = sourceImportIntents purpose
   , pvDownsweepExcludes = excludedOwners
   , pvTransformParsed = \env summary parsed -> do
       captured <- readIORef completedValuesRef
       transformed <- transformWithCompletedValues captured purpose targetModName' env summary parsed
       case scopeRequestTypes =<< exact of
         Just (recipe, signatures) | ms_mod_name summary == targetModName' ->
           thenNativeModule transformed (rewriteRequestTypes env recipe signatures)
         _ -> pure transformed
   , pvPlan = \compilerViewDirectory timing modGraphRaw selectedExact -> do
      let directSummaries = [ ms | ModuleNode _ ms <- mgModSummaries' modGraphRaw ]
          importsOf ms = [ unLoc lmn | (_, lmn) <- ms_textual_imps ms ]
          -- Everything that (directly or transitively) imports an injected
          -- Val module can't go through the @load'@ below — its import can
          -- only resolve once dependency-directed injection has happened. This
          -- generalizes the old "just exclude the target" rule: a decl module
          -- (@Lib.G<g>@) that itself imports a Val module is ALSO a
          -- dependency needing deferral, not just the ultimate leaf target.
          -- Plain forward fixpoint over the (small, per-turn) module graph —
          -- no existing GHC utility does this specific reverse-reachability
          -- query, so this is a self-contained graph closure over data
          -- already on each 'ModSummary'.
          closure seed =
            let grown = seed `Set.union` Set.fromList
                  [ ms_mod_name ms
                  | ms <- directSummaries
                  , any (`Set.member` seed) (importsOf ms)
                  ]
            in if grown == seed then seed else closure grown
          deferredMods = closure (Set.fromList (targetModName' : excludedOwners))
          -- Exclude every deferred module (target ∪ transitive Val-importers)
          -- from the load' graph. A @load'@ that reaches one of them (e.g.
          -- @LoadDependenciesOf targetHUM@, whose @createBuildPlan@ includes
          -- ALL modules reachable from the root) compiles it BEFORE the Val
          -- iface is injected, so its @import Tidepool.Session.Val.G<g>@
          -- fails → GHC error-recovery emits "Could not find module" AND
          -- inserts a FAKE empty iface into the EPS PIT for the Val module.
          -- Filtering deferred modules out makes @load'@ compile ONLY the
          -- untouched source deps; each deferred module is compiled AND its
          -- interface registered back into the HPT in the post-injection
          -- loop, in dependency order.
          depGraph = mkModuleGraph
            [ node | node <- mgModSummaries' modGraphRaw
                   , case node of
                       ModuleNode _ ms -> not (ms_mod_name ms `Set.member` deferredMods)
                       _               -> True ]
      injectedRef <- liftIO (newIORef Set.empty)
      injectedInterfacesRef <- liftIO (newIORef [])
      originalHooks <- hsc_hooks <$> getSession
      verifiedClosureRef <- liftIO (newIORef Nothing)
      retainedOriginalHomesRef <- liftIO (newIORef [])
      scaffoldRef <- liftIO (newIORef noGeneratedScaffoldImports)
      injectMsRef <- liftIO (newIORef (0 :: Integer))
      executionPlan <- case selectedExact of
        Nothing -> pure (ExactExecutionPlan depGraph [] [] modGraphRaw)
        Just admitted -> do
          env <- getSession
          let originals = [iface | (iface,_,_) <- scopeInterfaces admitted]
              values = scopeValueInterfaces admitted
          closure' <- liftIO (readVerifiedExactIfaceClosureWithCheckedValues env originals values)
            >>= either (liftIO . ioError . userError) pure
          interfaces <- either (liftIO . ioError . userError) pure (selectVerifiedExactInterfaces closure' originals)
          checkedValues <- either (liftIO . ioError . userError) pure
            (checkedValueImportAuthorityFromVerified closure' values)
          checkedInterfaces <- either (liftIO . ioError . userError) pure
            (selectVerifiedValueInterfaces closure' values)
          hydrated <- liftIO (hydrateExactScope env interfaces)
          let originalOwners = Set.fromList [mi_module iface | (_,iface) <- interfaces]
          liftIO (writeIORef retainedOriginalHomesRef
            [home | home <- eltsHpt (hsc_HPT hydrated), mi_module (hm_iface home) `Set.member` originalOwners])
          liftIO (validateEnvironmentFamilies hydrated)
          let byOwner = Map.fromList [((exactUnit iface,exactModule iface),iface) | (iface,_) <- interfaces]
              lexical = [(iface,imports) | (owner,imports) <- scopeLexical admitted
                , Just iface <- [Map.lookup owner byOwner]]
          scaffold <- case generatedRecipe purpose of
            Nothing -> pure noGeneratedScaffoldImports
            Just recipe -> do
              targetSummary <- case [summary | ModuleNode _ summary <- mgModSummaries' modGraphRaw
                  , ms_mod_name summary == targetModName'] of
                [summary] -> pure summary
                _ -> liftIO (fail "generated scaffold target summary is missing or duplicated")
              parsedScaffold <- parseModule targetSummary
              original <- either (liftIO . fail) pure (checkedRecipeOriginal admitted)
              liftIO (readGeneratedScaffoldImportAuthority closure'
                (scopeExecutionNativeOwners admitted) original recipe parsedScaffold modGraphRaw hydrated)
                >>= either (liftIO . fail) pure
          liftIO (writeIORef scaffoldRef scaffold)
          preflight <- liftIO (installExactLexicalGraphWithScaffold modGraphRaw lexical checkedValues scaffold hydrated)
          _ <- either (liftIO . ioError . userError) pure preflight
          liftIO (writeIORef verifiedClosureRef (Just closure'))
          planExactExecutionLoad compilerViewDirectory admitted interfaces checkedInterfaces targetModName' modGraphRaw depGraph
      let executionModules = executionOriginalModules executionPlan
      liftIO $ do
        emitCount timing "exact_execution_original_load_owners" (toInteger (length executionModules))
        emitCount timing "exact_execution_fresh_provider_compiles" (toInteger (length (executionFreshProviders executionPlan)))
      pure CompilePlan
        -- Compile the turn's home-package SOURCE dependencies
        -- (@Tidepool.Prelude@, @Tidepool.Effects@, @Lib.G<g>@) into the HPT,
        -- but NOT the turn target itself. LoadAllTargets on depGraph (target
        -- filtered out above) — equivalent to the old @LoadDependenciesOf@
        -- but without compiling the target prematurely.
        { cpLoadGraph = executionLoadGraph executionPlan
        , cpAfterLoad = do
            -- Restore the FULL module graph (target included) so the
            -- per-module typecheck can see HPT instances from dep modules:
            -- @hptSomeThingsBelowUs@ walks @moduleGraphModulesBelow
            -- (hsc_mod_graph) target@, and @load'@ left @hsc_mod_graph =
            -- depGraph@ (target absent), which would yield an empty HPT
            -- instance env ("No instance for ToJSON …").
            do hscMG <- getSession
               case selectedExact of
                 Nothing -> setSession hscMG { hsc_mod_graph = modGraphRaw }
                 Just admitted -> do
                   let originals = [iface | (iface, _, _) <- scopeInterfaces admitted]
                       values = scopeValueInterfaces admitted
                   closure' <- liftIO (readIORef verifiedClosureRef) >>= maybe
                     (liftIO $ ioError $ userError "exact hydration lacks its verified closure") pure
                   interfaces <- either (liftIO . ioError . userError) pure
                     (selectVerifiedExactInterfaces closure' originals)
                   checkedValues <- either (liftIO . ioError . userError) pure
                     (checkedValueImportAuthorityFromVerified closure' values)
                   retainedHomes <- liftIO (readIORef retainedOriginalHomesRef)
                   let originalOwners = Set.fromList [mi_module iface | (_,iface) <- interfaces]
                       restored = hscUpdateHPT (\table -> foldr
                         (\home homes -> addToHpt homes (moduleName (mi_module (hm_iface home))) home)
                         table [home | home <- retainedHomes, mi_module (hm_iface home) `Set.member` originalOwners]) hscMG
                   hydrated <- liftIO (hydrateExactScope restored interfaces)
                   setSession hydrated
                   -- Make clears the HPT. Only after it finishes do retained
                   -- canonical artifacts supply compiler bytecode. This never
                   -- runs a source frontend or republishes a native product.
                   forM_ executionModules $ \selectedModule -> do
                     current <- getSession
                     let artifact = retainedCompilerArtifact selectedModule
                         key = (exactUnit artifact, exactModule artifact)
                         summary = retainedCompilerSummary selectedModule
                         name = ms_mod_name summary
                     home <- maybe (liftIO (throwIO (FinalizedExecutionHomeMissing key))) pure
                       (lookupHpt (hsc_HPT current) name)
                     liftIO (revalidateAdmittedCore (retainedCompilerAdmission selectedModule))
                     unless (isJust (homeMod_bytecode (hm_linkable home))) $ do
                       attached <- liftIO (admittedCompilerInterface current
                         (retainedCompilerAdmission selectedModule) (ms_mod summary))
                       compile <- maybe (liftIO (throwIO (FinalizedExecutionCoreMissing key))) pure
                         (loadIfaceByteCode current attached (ms_location summary) (md_types (hm_details home)))
                       bytecode <- timePhase timing "retained_finalized_bytecode" (liftIO compile)
                       setSession (hscUpdateHPT (\table -> addToHpt table name
                         home {hm_linkable = justBytecode bytecode}) current)
                   withExecutables <- getSession
                   let verifiedOwners = Map.fromList
                         [((exactUnit iface, exactModule iface), iface)
                         | (iface, _) <- interfaces]
                       selected =
                         [(iface, imports)
                         | (owner, imports) <- scopeLexical admitted
                         , Just iface <- [Map.lookup owner verifiedOwners]]
                   scaffold <- liftIO (readIORef scaffoldRef)
                   graph <- liftIO (installExactLexicalGraphWithScaffold modGraphRaw selected checkedValues scaffold withExecutables)
                   baseline <- either (liftIO . ioError . userError) pure graph
                   linked <- if null executionModules then pure baseline else do
                     unless (isNothing (hscCompileCoreExprHook (hsc_hooks baseline))) $
                       liftIO (throwIO (ExecutionSourceUnsupported ("", "retained compiler hook")))
                     let shadows = mgModSummaries' (executionLinkGraph executionPlan)
                         shadowKeys = Set.fromList (map mkNodeKey shadows)
                         link invocation location expression = hscCompileCoreExpr' invocation
                           {hsc_mod_graph=mkModuleGraph
                             (shadows ++ [node | node <- mgModSummaries' (hsc_mod_graph invocation)
                               , mkNodeKey node `Set.notMember` shadowKeys])} location expression
                     pure baseline {hsc_hooks=(hsc_hooks baseline) {hscCompileCoreExprHook=Just link}}
                   setSession linked
                   -- A fresh provider is authored in the current lexical
                   -- environment. Compile its whole GHC pipeline after
                   -- restoring that environment; hidden execution owners may
                   -- supply bytecode, never an orphan instance or family rule.
                   forM_ (executionFreshProviders executionPlan) $ \summary -> do
                     current <- getSession
                     hmi <- liftIO (compileOne' (Just batchMsg) (exactHomeInstancesFor summary current)
                       summary {ms_hspp_opts=canonicalizeRepresentationFlags (ms_hspp_opts summary)}
                       1 1 Nothing emptyHomeModInfoLinkable)
                     unless (mi_module (hm_iface hmi) == ms_mod summary
                         && (isJust (homeMod_bytecode (hm_linkable hmi)) || isJust (homeMod_object (hm_linkable hmi)))) $
                       liftIO (throwIO (ExecutionSourceLinkableMissing
                         (unitString (moduleUnit (ms_mod summary)),moduleNameString (ms_mod_name summary))))
                     setSession (hscUpdateHPT (\table -> addToHpt table (ms_mod_name summary) hmi) current)
            -- Dependency order matters now that MULTIPLE modules (not just
            -- one leaf target) may need deferred, post-injection compilation:
            -- a deferred module that itself depends on another deferred
            -- module (e.g. the target importing a Val-referencing
            -- @Lib.G<g>@) must see the latter ALREADY reinserted into the HPT
            -- by the time its own turn in the loop comes up.
            -- @mgModSummaries@/@mg_mss@ is not guaranteed topologically
            -- ordered (see its haddock); @topSortModuleGraph@ +
            -- @flattenSCCs@ (both re-exported by the umbrella 'GHC' module
            -- already imported here) give a real deps-before-dependents
            -- order.
        , cpSummaries = pure
            [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing) ]
          -- Reached only once a session has a prior binding to inject
          -- ('isSessionScopeActive'); every such turn's wrapper compiles a
          -- target literally named @__result@ (scaffold-reserved, never
          -- @result@).
        , cpKeepPrivateResult = case effectivePurpose of
            ProgramItemCompile original _ _ _ -> original
            _ -> effectivePurpose == OriginalDeclarationCompile
        , cpResultBinders = [scaffoldTargetName, scaffoldOutputBase]
        , cpBeforeModule = \modSum ->
            when (ms_mod_name modSum `Set.member` deferredMods) $ do
              injected <- liftIO (readIORef injectedRef)
              let directImports = Set.fromList (importsOf modSum)
                  needed =
                    [ valueModule
                    | valueModule <- ssValIfaces scope
                    , let moduleName = renderSessionModule valueModule
                    , moduleName `Set.member` directImports
                    , moduleName `Set.notMember` injected
                    ]
                  captureCompleted = ms_mod_name modSum == targetModName' && not (null completedValues)
              when (not (null needed) || captureCompleted) $ do
                -- Inject only the value modules this source module imports.
                -- Declaration and value generations form one chronological
                -- dependency DAG: an older Lib may need an older Val, while
                -- a newer Val's type may mention that Lib. Eagerly injecting
                -- every live Val before the first deferred Lib creates a
                -- false cycle and makes GHC reject the still-unloaded Lib.
                ((hscInjected, completedCap), injectMs) <- timeSection $ do
                  hsc0 <- getSession
                  case selectedExact of
                    Just admitted | scopePurpose admitted /= NoCheckedPurpose -> do
                      let wanted = map (moduleNameString . renderSessionModule) needed
                          artifacts = [value | value <- scopeValueInterfaces admitted, exactModule value `elem` wanted]
                      when (length artifacts /= length needed) $ liftIO $ ioError $ userError
                        "checked value injection lacks exact captured input bytes"
                      if captureCompleted
                        then do
                          -- The target's completed winners and remaining
                          -- direct dependencies share one verified read and
                          -- hydration knot. Only this cap's exact installed
                          -- allocations authorize thin-interface refinement.
                          let completedOwners = map completedValueModule completedValues
                              dependencies = filter ((`notElem` completedOwners) . exactModule) artifacts
                          closure' <- liftIO (readIORef verifiedClosureRef) >>= maybe
                            (liftIO $ ioError $ userError "completed value injection lacks its verified closure") pure
                          readback <- liftIO (hydrateCompletedValueImportsWithVerifiedDependencies
                            closure' dependencies completedValues hsc0)
                          (hydrated, captured) <- either (liftIO . ioError . userError) pure readback
                          pure (hydrated, Just captured)
                        else do
                          closure' <- liftIO (readIORef verifiedClosureRef) >>= maybe
                            (liftIO $ ioError $ userError "checked value injection lacks its verified closure") pure
                          captured <- either (liftIO . ioError . userError) pure
                            (selectVerifiedValueInterfaces closure' artifacts)
                          hydrated <- liftIO (hydrateExactScope hsc0 captured)
                          liftIO $ forM_ needed $ \owner -> case
                              [artifact | artifact <- artifacts,
                                exactModule artifact == moduleNameString (renderSessionModule owner)] of
                            [artifact] -> registerSessionInterfaceLocation (exactPath artifact) owner hydrated
                            _ -> fail "checked finder owner differs from captured interface"
                          pure (hydrated, Nothing)
                    _ | captureCompleted -> liftIO $ ioError $ userError
                      "completed value injection lacks its protected request"
                      | otherwise -> do
                          (hydrated, captured) <- injectSessionScopeWithCaptures
                            (scope { ssValIfaces = needed }) hsc0
                          liftIO (modifyIORef' injectedInterfacesRef (captured ++))
                          pure (hydrated, Nothing)
                setSession hscInjected
                liftIO $ do
                  forM_ completedCap $ \captured -> writeIORef completedValuesRef (Just captured)
                  modifyIORef' injectedRef
                    (`Set.union` Set.fromList (map renderSessionModule needed
                      ++ [mkModuleName (completedValueModule value) | captureCompleted, value <- completedValues]))
                  modifyIORef' injectMsRef (+ injectMs)
        , cpBeforeMerge = \boundary ->
            do liftIO (readIORef injectMsRef >>= emitPhase timing "inject")
               case boundary of
                 CheckedReceiptBoundary -> pure ()
                 NativeMergeBoundary -> forM_ selectedExact $ \admitted -> do
                   env <- getSession
                   verified <- liftIO (revalidateExactScope env admitted)
                   either (liftIO . fail) pure verified
        , cpInjectedSessionInterfaces = readIORef injectedInterfacesRef
        , cpFinalEnv = \env -> hscUpdateFlags canonicalizeDFlags env {hsc_hooks=originalHooks}
        }
   }

-- | Render the exact type held by the typechecked environment before the
-- executable pipeline can simplify the binding away.
capturedBindingDisplay :: String -> TcGblEnv -> Maybe String
capturedBindingDisplay occurrence tcg =
  renderWithContext defaultSDocContext . ppr <$> capturedBindingType occurrence tcg

-- | Compiler-only inspection bindings are not part of a module's public API.
-- GHC can omit them from the target reader/type environments after a session
-- interface registration, even though it accepted their typed syntax. The
-- typechecked source is the exact owner of those local generated binders;
-- retain only their Ids for the immediate inspection request. This includes
-- the effect-row sentinel, @:type@ probes, and the lookup type-search binder:
-- all are target-local and must not be resolved back through an intentionally
-- elided target interface.
capturedInspectionProbes :: TcGblEnv -> Map.Map String Id
capturedInspectionProbes tcg = Map.fromList
  [ (occurrence, identifier)
  | identifier <- typeEnvIds (tcg_type_env tcg)
      ++ collectDataIds (tcg_binds tcg)
  , let occurrence = occNameString (nameOccName (idName identifier))
  , occurrence == "__tidepool_lookup_row"
      || occurrence == "__tidepool_lookup_query"
      || "__tidepool_inspect_" `isPrefixOf` occurrence
  ]

-- | Harvest the compiler-reserved aliases that the whole-cell source builder
-- places immediately after statement binders. They are local to a @do@
-- expression, so unlike inspection probes they never appear in
-- 'tcg_type_env'. Walking the typechecked bind tree is the owning GHC
-- boundary: every occurrence of one alias carries the same zonked 'Id', and
-- the map removes repeated occurrences without relying on source spelling for
-- identity.
capturedCellBinderPins :: HscEnv -> TcGblEnv -> [CheckedBinderPin]
capturedCellBinderPins hsc tcg =
  [ CheckedBinderPin
      { checkedPinKey = occurrence
      , checkedPinType = renderCellPinType names stableType
      , checkedPinHeads = nominalHeadsOfType stableType
      }
  | (occurrence, identifier) <- Map.toAscList unique
  , let stableType = stabilizeEffectRows (idType identifier)
  ]
  where
    names = mkNamePprCtx (PromTickCtx True True) (hsc_unit_env hsc) (tcg_rdr_env tcg)
    unique = Map.fromList
      [ (occurrence, identifier)
      | identifier <- collectDataIds (tcg_binds tcg)
      , let occurrence = occNameString (nameOccName (idName identifier))
      , "__tidepool_cell_pin_" `isPrefixOf` occurrence
      ]

-- Diagnostic types retain their defining generation when another declaration
-- shadows the same occurrence. These strings never become signature authority.
renderCellPinType :: NamePprCtx -> Type -> String
renderCellPinType originalNames = renderWithContext context . ppr
  where
    context = defaultSDocContext
      { sdocStyle = mkUserStyle (cellPinNameContext originalNames) AllTheWay }

cellPinNameContext :: NamePprCtx -> NamePprCtx
cellPinNameContext originalNames = originalNames
  { queryQualifyName = \modu occurrence ->
      case parseSessionModule (moduleNameString (moduleName modu)) of
        Just _ -> NameQual (moduleName modu)
        Nothing -> queryQualifyName originalNames modu occurrence
  }

-- Stop at an 'Id': descending through its type/name graph is both unnecessary
-- and dramatically larger than the typechecked syntax tree that owns it.
collectDataIds :: Data value => value -> [Id]
collectDataIds value =
  case cast value of
    Just identifier -> [identifier]
    Nothing -> concat (gmapQ collectDataIds value)

-- | Read the GHC 'Type' (NOT a rendered string) of the named top-level binding
-- out of a module's typechecked type env. Binding mode uses it to grab
-- the @result@ binding's @Eff stack T@ type so 'stripMonadHead' can recover the
-- bound value's type @T@ for the thin session iface + the BoundBinder sidecar.
-- 'Nothing' when no such binder exists (every non-bind extraction).
capturedBindingType :: String -> TcGblEnv -> Maybe Type
capturedBindingType occ tcg =
  case [ i | i <- typeEnvIds (tcg_type_env tcg)
           , occNameString (nameOccName (idName i)) == occ ] of
    (i:_) -> Just (idType i)
    []    -> Nothing

-- | Strip a monadic head off a bind target's type: @Eff stack T@ → @T@,
-- @M a@ → @a@. Peel leading quantifiers and constraints only long enough to
-- reach the monadic body, then put them back around the result. Dropping them
-- produces an invalid thin interface whenever GHC legitimately generalizes a
-- returned value (a phantom protocol parameter on an 'ActorRef' exposed this
-- bug as an out-of-scope interface variable).
stripMonadHead :: Type -> Type
stripMonadHead ty =
  let (binders, constraints, body) = tcSplitSigmaTy ty
      result = case splitAppTy_maybe body of
        Just (_, res) -> res
        Nothing       -> body
  in mkInvisForAllTys
       (map (mkTyVarBinder SpecifiedSpec) binders)
       (mkInvisFunTys constraints result)

-- | Is the bound value a CLOSURE (Tier1) rather than first-order data (Tier0)?
-- True iff @T@ (after stripping its own foralls/context) IS a function type,
-- OR MENTIONS one anywhere in its structure — a type application argument, a
-- newtype's representation, or a data constructor field, walked transitively
-- (visited-set keyed on 'TyCon', so a recursive type terminates instead of
-- looping). The
-- wider check matters because Tier0 forces the bound value to normal form
-- before tenuring: a record with a function FIELD (e.g. a companion "mounted
-- value" carrying an applied handler) is not itself a
-- function type, but deep-forcing it would try to force through the
-- function field and crash — it needs the SAME store-as-is treatment a bare
-- function gets.
--
-- 'goTc' also special-cases the arrow TyCon itself. A HIGHER-KINDED field
-- instantiated at a partially-applied arrow (@data Box f = Box (f Int)@ at
-- @f = (->) Bool@) reaches 'goT' as one of @Box@'s own outer type
-- arguments — the CONCRETE @(->) Bool@, not @Box@'s abstract, unsubstituted
-- field declaration @f Int@ (which 'dataConRepArgTys' can never resolve to
-- a function regardless of what @f@ is instantiated to, and correctly so —
-- it is genuinely opaque without that instantiation). A SATURATED arrow
-- always normalizes to GHC's own @FunTy@ sugar (an invariant GHC itself
-- maintains — see "Representation of function types" in @GHC.Core.Type@)
-- and is already caught by 'splitFunTy_maybe' above; only a PARTIAL
-- application like @(->) Bool@ survives as a bare @TyConApp@ of the
-- primitive arrow TyCon, which has neither a newtype representation nor
-- DataCons — so before this case it fell through both 'goTc' checks to
-- 'False', misclassifying the whole @Box@ value as Tier0 and crashing the
-- same deep-force this function exists to prevent.
isClosureType :: Type -> Bool
isClosureType = goT emptyUniqSet
  where
    goT :: UniqSet TyCon -> Type -> Bool
    goT visited ty =
      let (_, constraints, body) = tcSplitSigmaTy ty
      -- Class dictionaries are value arguments even when the result is a
      -- scalar (@forall a. Num a => a@). Type binders and primitive equality
      -- evidence erase; they alone do not make a runtime closure. Inspect
      -- nested sigma types too, including polymorphic constructor fields.
      in any (not . isCoVarType) constraints || goBody visited body

    goBody :: UniqSet TyCon -> Type -> Bool
    goBody visited ty
      | Just{} <- splitFunTy_maybe ty = True
      | Just (tc, tyArgs) <- splitTyConApp_maybe ty = any (goT visited) tyArgs || goTc visited tc
      | otherwise = False

    goTc :: UniqSet TyCon -> TyCon -> Bool
    goTc visited tc
      | tc `elementOfUniqSet` visited = False
      | tyConUnique tc == fUNTyConKey || tyConUnique tc == unrestrictedFunTyConKey = True
      | otherwise =
          let visited' = addOneToUniqSet visited tc
              newtypeHit = case unwrapNewTyCon_maybe tc of
                Just (_tvs, reprTy, _coax) -> goT visited' reprTy
                Nothing -> False
              -- The worker fields include stored constructor dictionaries;
              -- source argument types omit them. Primitive coercion evidence
              -- has zero runtime width and does not need opaque retention.
              fieldHit = case tyConDataCons_maybe tc of
                Just dcs -> any (\dc -> any (\(Scaled _ ft) -> not (isCoVarType ft) && goT visited' ft)
                                             (dataConRepArgTys dc)) dcs
                Nothing -> False
          in newtypeHit || fieldHit

-- | Split a tuple type into its component types. @(T1, T2, ..., Tn)@ → @Just
-- [T1, T2, ..., Tn]@. Returns @Nothing@ for non-tuple types (constructors,
-- newtypes, function types, etc.). Used by the multi-binder bind path to verify
-- that an N-name bind has an N-tuple return type.
splitTupleType :: Type -> Maybe [Type]
splitTupleType ty =
  let (_, _, body) = tcSplitSigmaTy ty
  in case splitTyConApp_maybe body of
       Just (tc, args) | isTupleTyCon tc -> Just args
       _                                  -> Nothing

-- | Render a 'Type' to a display string (for the @typeDisplay@ field / @:t@),
-- the same 'ppr' pattern as 'capturedUserType'.
renderType :: Type -> String
renderType ty = renderWithContext defaultSDocContext (ppr ty)

-- | A home module needs prepared STG if it is @target@ or transitively occurs
-- as a resolved 'Var' in the target's desugared Core. Desugaring includes
-- resolved dictionaries, retaining instance-only dependencies that source
-- used-name tracking would miss. Optimization may erase these edges.
reachableModuleClosure :: ModuleName -> Map.Map ModuleName (Set.Set ModuleName) -> Set.Set ModuleName
reachableModuleClosure target referencesByMod = go (Set.singleton target) [target]
  where
    known = Map.keysSet referencesByMod
    go visited [] = visited
    go visited (m:ms) = case Map.lookup m referencesByMod of
      Nothing   -> go visited ms
      Just referenced ->
        let refs = referenced `Set.intersection` known
            new  = refs `Set.difference` visited
        in go (visited `Set.union` new) (ms ++ Set.toList new)

-- | Every defining module referenced by a module's top-level binding RHSs.
-- This graph-independent fact stays valid when a later request changes the
-- set of home modules; reachability intersects it with that request's graph.
moduleRefs :: ModGuts -> Set.Set Module
moduleRefs guts = Set.unions (map rhsModules (mg_binds guts))
  where
    rhsModules (NonRec _ rhs) = externalVarModules rhs
    rhsModules (Rec ps)       = Set.unions [ externalVarModules rhs | (_, rhs) <- ps ]

-- | Every defining module referenced by a real 'Var'
-- occurrence anywhere in a Core expression, at any binding depth. No
-- bound-variable tracking needed, unlike 'Translate.exprFreeVarKeys': a Core
-- 'Var' occurrence already points at its exact binder 'Id' (resolved by the
-- renamer/typechecker), so a local binder can never be confused with an
-- unrelated same-named import the way source text could — this walk cannot
-- under- OR over-count due to shadowing. Over-inclusion elsewhere (e.g. a
-- 'Var' for a DataCon worker, whose own defining module needs no -O2
-- unfoldings to be useful — its representation comes from static DataCon
-- info, always available regardless of tier) is harmless: the only failure
-- mode this item must avoid is EXCLUDING a module the target's Core
-- genuinely needs; including one too many only gives back some of the tier's
-- win, never correctness.
externalVarModules :: CoreExpr -> Set.Set Module
externalVarModules = go
  where
    go expr = case expr of
      Var v -> case nameModule_maybe (idName v) of
        Just m -> Set.singleton m
        _ -> Set.empty
      Lit _           -> Set.empty
      App f a         -> go f `Set.union` go a
      Lam _ e         -> go e
      Let b e         -> bindRefs b `Set.union` go e
      Case s _ _ alts -> go s `Set.union` Set.unions [ go rhs | Alt _ _ rhs <- alts ]
      Cast e _        -> go e
      Tick _ e        -> go e
      Type _          -> Set.empty
      Coercion _      -> Set.empty
    bindRefs (NonRec _ rhs) = go rhs
    bindRefs (Rec ps)       = Set.unions [ go rhs | (_, rhs) <- ps ]

-- | The canonical extraction DynFlags transformation, applied to the session
-- flags at startup AND re-applied per-module before extraction.
--
-- Interactive compilation uses -O0. GHC 9.12's @enableCodeGenForTH@ also
-- selects -O0 for home modules needed by splices, but its default interface
-- and unboxing flags differ from the prepared backend's contracts. Re-apply
-- those contracts to each ModSummary's @ms_hspp_opts@ before extraction.
--
-- Surgical: backend/opt-level/gopt only — exactly the fields the TH
-- downgrade touches. Per-module LANGUAGE pragmas already merged into
-- @ms_hspp_opts@ are preserved. Platform spoofing and @importPaths@ are
-- session-setup-only (see runPipeline): re-pinning bare genericPlatform
-- here would strip the platform constants populated at session init.
--
-- Flag contracts:
--   * Keep interface pragmas and exposed unfoldings: imported entry metadata
--     and exact original recovery must remain available independently of the
--     optimization level. RetainedUnfoldings protects retained native bodies.
--   * Preserve automatic unboxing of small strict fields, including imported
--     constructor layouts, without enabling the optimizer. Explicit
--     Opt_UnboxStrictFields remains source-selected, as with the former policy.
--   * FullLaziness and CPR stay disabled; PreparedStg owns CorePrep, STG CSE
--     and linting separately from this frontend optimization policy.
--   * Opt_ShowErrorContext / maxRelevantBinds (this change): every repl/eval
--     turn typechecks the user's expression inside harness scaffolding
--     (@__user@, @__b@, @it@, @toWire@ wrapper bindings). An ambiguity in
--     user code cascades into fallout against that wrapper, and GHC's
--     default renderer appends "In the expression: toWire it / In a stmt
--     of a 'do' block: …" context trails and "Relevant bindings include
--     __b :: f0 (Text, Int) …" lists that name scaffold identifiers the
--     caller never wrote and never asked about — pure noise, unlike
--     hole-fits below (which the caller DID ask about, via a literal `_`).
--     Opt_ShowErrorContext off drops the context trail entirely;
--     maxRelevantBinds = Just 0 drops (or minimizes, GHC may print a
--     "(Some bindings suppressed …)" stub) the relevant-bindings list.
canonicalizeDFlags :: DynFlags -> DynFlags
canonicalizeDFlags dflags =
  -- Keep common partial-pattern and missing-field diagnostics visible without
  -- rejecting otherwise valid Haskell. This pipeline also serves the stateful
  -- workbench, where warnings must not discard an input unit or prevent its
  -- bindings from becoming available to later turns.
  enableDiagnosticWarning Opt_WarnMissingFields $
  enableDiagnosticWarning Opt_WarnIncompletePatterns $
  enableDiagnosticWarning Opt_WarnIncompleteUniPatterns $
  -- Trim machine-channel noise: typed-hole "Valid hole fits include …" lists
  -- are enormous (dozens of candidates) and useless to an LLM caller; the
  -- "Perhaps you meant …" similar-name hints are a separate mechanism and stay.
  -- -fprefer-byte-code (session-wide): when enableCodeGenForTH must provision
  -- a splice's home-module dependencies, provision them as BYTECODE, not native
  -- object code. Set at session init (NOT just per-summary) so GHC's downsweep
  -- — which re-derives each module's backend inside 'load' and ignores a
  -- backend field we patch onto a summary afterwards — chooses the interpreter.
  -- Object-code provisioning emits a .s and shells to the assembler; under the
  -- genericPlatform spoof that .s is x86_64/ELF and the macOS Mach-O assembler
  -- rejects it (`.type …, @object`; x86 mnemonics on aarch64). Bytecode is
  -- architecture-neutral, so the spoof stays confined to compiler lowering while
  -- splices run host-agnostically.
  (`gopt_set` Opt_UseBytecodeRatherThanObjects) $
  -- Valid-hole-fits stay ON: with ~200 stdlib/verb names in scope, "fits"
  -- on a typed hole is the interface's vocabulary-discovery engine (an LLM
  -- writes `_` to ask "what goes here"). The search only runs on hole
  -- errors, never on clean compiles.
  (`gopt_unset` Opt_ShowErrorContext) $
  (`gopt_set` Opt_UnboxSmallStrictFields) $
  (`gopt_unset` Opt_IgnoreInterfacePragmas) $
  (`gopt_unset` Opt_OmitInterfacePragmas) $
  gopt_set (gopt_set (gopt_unset (gopt_unset (updOptLevel 0 $ dflags
        { backend = noBackend
        , ghcLink = NoLink
        , maxRelevantBinds = Just 0
        }) Opt_FullLaziness) Opt_CprAnal)
        Opt_ExposeAllUnfoldings) Opt_ExposeOverloadedUnfoldings

-- | The TH/QQ downsweep must retain its interpreter backend so 'load'' can
-- execute splices. It must nevertheless expose the same physical constructor
-- choices as the later extraction front half. GHC's bang-option construction
-- uses 'Opt_OmitInterfacePragmas' as its automatic-unboxing switch, so copy it
-- with the two unboxing flags from the canonical policy. The remaining TH
-- execution settings stay on the summary produced by downsweep.
canonicalizeRepresentationFlags :: DynFlags -> DynFlags
canonicalizeRepresentationFlags dflags =
  copy Opt_OmitInterfacePragmas . copy Opt_UnboxStrictFields
    . copy Opt_UnboxSmallStrictFields
    . (`gopt_unset` Opt_IgnoreInterfacePragmas) $ dflags
  where
    canonical = canonicalizeDFlags dflags
    copy flag current
      | gopt flag canonical = gopt_set current flag
      | otherwise = gopt_unset current flag

enableDiagnosticWarning :: WarningFlag -> DynFlags -> DynFlags
enableDiagnosticWarning warning = (`wopt_set` warning)

-- | Give internal top-level simplifier floats stable module-qualified names.
--
-- Top-level binders with INTERNAL names (floats like @k_X1@, @$wk_snOX@) keep
-- per-module uniques. Prepared recovery concatenates several modules' bindings,
-- so (occName, unique-key) pairs can collide across modules.
-- Give every internal top-level binder an EXTERNAL name qualified by its
-- defining module, with a STABLE disambiguator baked into the OccName
-- (@k@ → @Probe.k_t3@, where @3@ is @k@'s ordinal position among this
-- module's own top-level binders), yielding globally unique, deterministic
-- identities. The ordinal — not the binder's raw
-- GHC 'Unique' — is what makes this deterministic ACROSS separate compiles
-- of the same source: 'mg_binds'\'s order is a pure function of this
-- module's own source and simplifier passes, never of how many Uniques the
-- surrounding GHC session happened to consume before reaching this module
-- (which a warm build-products-dir compile perturbs). Internal names cannot be referenced
-- from other modules' ModGuts, so substituting binder + occurrences within
-- the module is complete. Nested binders are untouched: their uniques cannot
-- collide with top-level uniques of the same module, and cross-module nested
-- references are lexically impossible.
externalizeInternalTops :: ModGuts -> ModGuts
externalizeInternalTops guts = guts { mg_binds = map goTop (mg_binds guts) }
  where
    m = mg_module guts
    topBinders = concatMap binders (mg_binds guts)
      where binders (NonRec b _) = [b]
            binders (Rec ps)     = map fst ps
    fixes = mkVarEnv [ (v, externalize ordinal v)
                     | (ordinal, v) <- zip [0 :: Int ..] topBinders
                     , not (isExternalName (idName v)) ]
    externalize ordinal v =
      let n    = idName v
          u    = nameUnique n
          occ  = nameOccName n
          occ' = mkOccName (occNameSpace occ)
                           (occNameString occ ++ "_t" ++ show ordinal)
      in setVarName v (mkExternalName u m occ' (nameSrcSpan n))
    sub v = fromMaybe v (lookupVarEnv fixes v)
    goTop (NonRec b rhs) = NonRec (sub b) (goExpr rhs)
    goTop (Rec ps)       = Rec [ (sub b, goExpr rhs) | (b, rhs) <- ps ]
    -- Substitute occurrences only; nested binders keep their names.
    goBind (NonRec b rhs) = NonRec b (goExpr rhs)
    goBind (Rec ps)       = Rec [ (b, goExpr rhs) | (b, rhs) <- ps ]
    goExpr e = case e of
      Var v            -> Var (sub v)
      Lit _            -> e
      App f a          -> App (goExpr f) (goExpr a)
      Lam b body       -> Lam b (goExpr body)
      Let b body       -> Let (goBind b) (goExpr body)
      Case s b t alts  -> Case (goExpr s) b t
                            [ Alt c bs (goExpr rhs) | Alt c bs rhs <- alts ]
      Cast e' co       -> Cast (goExpr e') co
      Tick t e'        -> Tick t (goExpr e')
      Type _           -> e
      Coercion _       -> e
