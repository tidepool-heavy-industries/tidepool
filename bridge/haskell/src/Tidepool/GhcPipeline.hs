{-# LANGUAGE GADTs #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}

module Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedModuleCompletion, PreparedModuleCompletionInputs(..), PreparedModuleObserver(..)
  , PreparedPipelineResult, pprPipelineResult, pprModules, pprProductInterfaces, pprFinalizedModules, pprCanonicalOriginalInputs, pprAcquiredCanonicalOriginals
  , CheckedPublicModuleResult, checkedPublicModuleIdentity, checkedPublicModuleEvidence
  , WorldModulePolicyFailure(..), WorldModulePolicy(..)
  , pprPackageImports, pprCandidateAdmissions, pprOriginalBindings, pprOriginalReferences, pprReuseContext
  , AdmittedSourceCandidate, pprAcceptedCandidates, preparedCandidateOriginal, preparedCandidateProof, revalidatePreparedCandidateInputs, PreparedSegmentProductsResult(..), TypedSegmentPreparation, CheckedEnvironmentResult(..)
  , PreparedDependencies, PreparedSourceRoute(..), preparedSourceRoute
  , preparedFreshDependencies, preparedExactCompilation, preparedHomeRequirements
  , ProgramSourceImports, retainProgramSourceImports, withProgramSourceImports
  , runPipelineSelected, runPipelineSessionSelected
  , runPipelineSelectedRetaining
  , CompilerProducerIdentity, captureCompilerProducerIdentity
  , NativeProductProvenance, nativeProductProducer, nativeProductProfile
  , NativeProductConfigurationUnavailable(..), scopedNativeProductProvenance
  , captureSupportedPackageProfile
  , runPipelineSessionSelectedWithProducer
  , CompilePurpose(..), CompilePurposeConflict(..), normalizeCompilePurpose, withSourceImportIntents
  , PipelineResult, prBinds, prHscEnv, prInjectedSessionInterfaces, prProducedSessionInterfaces
  , prCanonicalInterfaceAdmissions, prCapturedType, prCheckedBinderPins, prResultType, prWarnings
  , prTargetRdrEnv, prTargetTcGblEnv
  , generatedScaffoldRecipe
  , FinalizedModule, finalizedHomeModInfo, finalizedTidyGuts
  , WorldSourceOriginal, acquireWorldSourceOriginal, worldSourceOriginalFinalization, worldSourceOriginalPackages, captureWorldSourceOriginals
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
  , CompilerScope, scopedCompile, scopedCompileCompleted, scopedReadExactScope, scopedRunGhc, scopedParserFlags
  , scopedEnsureWorld, scopedReadHeldWorld, scopedQueryWorld, scopedPlanWorldCell, scopedValidateWorldCell, scopedCompileWorldCell, scopedCommitWorldDependencies, HeldWorldQueryBinding
  , HeldWorldCellAdmission, heldWorldCellQuestion, heldWorldCellPlan, heldWorldCellSlots
  , HeldWorldCellStage, scopedPrepareWorldCellStage, heldWorldCellStagePlan, heldWorldCellStageItems, heldWorldCellStageOffset, heldWorldCellStagePath, heldWorldCellStageTypedSource, heldWorldCellStageOriginal
  , HeldWorldCellItem, heldWorldCellItemOrdinal, heldWorldCellItemSlot, heldWorldCellItemSourceDigest
  , WorldCellRefusal(..)
  , materializeWorldSources, worldMaterializationCharge, retainWorldMaterialization, releaseWorldMaterialization
  , worldMaterializationOwners, emitWorldAcquisitionRejection
  , CompletedCompilerResult, completedCompilerResult, completedCompilerRecovery, completedCompilerGuard
  , completedCompilerWorldCompletion, completedCompilerWorldStage, CompiledWorldStage, withCompiledWorldStage, withCompiledWorldStageRecovery, withCompiledWorldInputs, withCompiledWorldNativeCoordinates, inheritCompiledWorldCanonicalRecovery, requireSameCompiledWorldStage, finishCompiledWorldStage, compiledWorldStageCompletion, WorldCompilerCompletion, worldCompilerCompletionInputs, worldCompilerCompletionDependencies, worldCompilerCompletionPlannedDeclaration, requireWorldProductPredecessor, requireWorldProductInputs
  , WorldCellInputs, worldCellRetainedGenerations, worldCellCompilerInputReferences, worldCellSourceSnapshot, worldCellExactImports, worldCellSourceSelection, worldCellSourceRootAliases, worldCellTypeInterfaces
  , WorldCellInputReference, encodeWorldCellInputReference, worldCellInputReferenceBinding, selectWorldCellExecutionInputReferences
  , PendingWorldDependencies
  , CompilerScopeRunner, withResidentCompilerScopes
  , CompilerRecoveryCaches(..)
  , withScopedExactInterfaceTransaction
  , withExactInterfaceTransaction
  ) where

import Tidepool.RequestCompletion
  ( CleanupStage(CompilerScopeRestore), captureCompletion, withCompletionRelease
  , attemptReleases, appendCleanupFailures, finishCompletion, completionResult, BodyOutcome(..) )

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
import GHC.Unit.Home.ModInfo (HomeModInfo(..), HomePackageTable, HomeModLinkable(..), emptyHomeModInfoLinkable, emptyHomePackageTable, justBytecode, addToHpt, lookupHpt, eltsHpt, filterHpt)
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
import GHC.Unit.Finder (initFinderCache, addHomeModuleToFinder)
import GHC.Unit.Module.ModIface (set_mi_extra_decls, mi_iface_hash)
import GHC.Unit.Module.Deps (imp_mods)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import Codec.CBOR.Encoding qualified as WorldEncoding
import Codec.CBOR.Write (toStrictByteString)
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
import Tidepool.TypedSegment.Source
  ( rewriteParsedSegmentRoot, moduleInputOccurrences, moduleImportRequests, prepareSegmentDemand, prepareModuleDemand
  , ParsedModuleDemand, segmentModuleDemand, moduleDemandMatchesSource, annotateModuleDemand, ObservationOperand(..), validateObservationContribution
  , SegmentDemandScope, newSegmentDemandScope, updateSegmentDemandScope, associateSegmentDemandName )
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), SourceRejection(..), InputRejection(..), DependencyLoadFailure(..), dependencyDiagnostic, renderDiag, spanOf)
import GHC.Data.FastString (unpackFS, mkFastString)
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Unit.Module.Graph (mgModSummaries', ModuleGraphNode(..), NodeKey(..), ModNodeKeyWithUid(..), mkNodeKey, nodeDependencies)
import GHC.Unit.Home (homeUnitAsUnit, homeUnitId, isHomeUnit)
import GHC.Unit.Env (UnitEnv(..))
import GHC.Unit.Types (unitString, unitIdString, stringToUnit, toUnitId, GenWithIsBoot(..), IsBootInterface(..))
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Driver.Session
  ( updOptLevel, gopt_set, gopt_unset, xopt
  , WarningFlag
      ( Opt_WarnMissingFields
      , Opt_WarnIncompletePatterns
      , Opt_WarnIncompleteUniPatterns
      )
  , wopt_set
  , packageFlagsChanged
  , PackageFlag(..), PackageArg(..), ModRenaming(..)
  , PackageDBFlag(..), PkgDbRef(..), ParMakeCount(..) )
import GHC.Utils.TmpFs (TempDir(..))
import GHC.Unit.Module.ModGuts (ModGuts(..), CgGuts(..))
import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), Alt(..))
import GHC.Core.FVs (exprSomeFreeVars)
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BS8
import qualified Crypto.Hash.SHA256 as SHA256
import Data.Char (digitToInt, ord)
import qualified Data.Graph as Graph
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable
  ( renderWithContext, defaultSDocContext, ppr, SDocContext(..), Outputable, showSDocUnsafe
  , mkUserStyle, NamePprCtx(..), QualifyName(..), Depth(..), PromotionTickContext(..) )
import GHC.Types.Id (idName, setIdExported)
import GHC.Types.Avail (availNames)
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
import GHC.Types.Unique.Set (UniqSet, nonDetEltsUniqSet, emptyUniqSet, addOneToUniqSet, elementOfUniqSet)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Tc.Types (FrontendResult(..), TcGblEnv, tcg_mod, tcg_th_coreplugins, tcg_import_decls, tcg_dependent_files, tcg_binds, tcg_rdr_env, tcg_type_env, tcg_imports, tcg_keep)
import GHC.Types.Name.Set (extendNameSetList)
import GHC.Types.Name.Reader (GlobalRdrEnv, rdrNameOcc)
import GHC.Types.Name.Ppr (mkNamePprCtx)
import GHC.Types.Name (nameOccName, nameUnique, mkExternalName, mkInternalName, nameModule_maybe)
import GHC.Types.Name.Occurrence (mkOccName, mkTyVarOcc, occNameSpace, occNameString, isTcOcc, isSymOcc, mkVarOcc)
import GHC.Types.Unique.Supply (UniqSupply, mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Types.Var (isId, mkTyVar, mkTyVarBinder, setVarName)
import GHC.Types.Var.Set (isEmptyVarSet)
import Language.Haskell.Syntax.Specificity (Specificity (SpecifiedSpec))
import GHC.Types.Var.Env (mkVarEnv, lookupVarEnv)
import Control.Applicative ((<|>))
import Control.DeepSeq (force)
import Control.Concurrent (myThreadId)
import Control.Exception
  ( Exception, SomeException, SomeAsyncException, fromException, finally, bracket, mask, try, catch, throw, throwIO, IOException, evaluate )
import Data.Maybe (fromMaybe, isJust, isNothing, catMaybes, mapMaybe)
import Data.List (find, isPrefixOf, nub, nubBy, sort, sortOn, intercalate, foldl')
import Data.Containers.ListUtils (nubOrd)
import Data.IORef (IORef, atomicModifyIORef', newIORef, modifyIORef', readIORef, writeIORef)
import Numeric (showHex)
import System.Environment (lookupEnv)
import System.FilePath (takeFileName, takeDirectory, takeExtension, dropExtension, isAbsolute, normalise, pathSeparator, (</>))
import System.Directory (canonicalizePath, createDirectory, createDirectoryIfMissing, doesDirectoryExist, makeAbsolute, doesFileExist, getCurrentDirectory, getModificationTime, getTemporaryDirectory, removeDirectoryRecursive)
import System.Posix.Temp (mkdtemp)
import System.IO (hPutStrLn, stderr, hClose)
import Tidepool.ArtifactBytes (ArtifactBytes, artifactBytes, artifactSha256, artifactLength, captureArtifactBytes)
import Tidepool.RequestInputs (capturedRequestInputToken)
import Tidepool.BoundedRead (readFileAtMost)
import Control.Monad.IO.Class (liftIO)
import Control.Monad (forM, forM_, when, unless, filterM, foldM, void, (>=>))
import Data.Data (Data, cast, gmapQ)
import Data.Foldable (toList)
import Data.Unique qualified as RequestUnique
import Data.Word (Word64)
import Tidepool.CborEncode (encodeCellOut)
import Tidepool.Binders (analyzeOrderedCellWithFlags, sourceModuleFlags, renderCellSplitError, CellAnalysisItem(..), CellBindingForm(..), TurnKind(..), StmtBinders(..), cellInferenceSegments, CellSourceSpan(..), PreparedTypedSegmentSource(..), prepareTypedSegmentSource, CheckedBinderPin(..), CellSourcePlan(..), SourcePrologue(..), LocatedPragma(..), LocatedImport(..), ImportIntent(..), CellGenericDeclaration(..), CellStructuralDisplayTarget(..), CellExpressionPlan(..), ExpressionLiftPlan(..), omitCellGenericDeclarations, omitCellStructuralDisplayDeclarations)
import Tidepool.CheckedCell (CheckedSignature, captureCheckedSignature, rewriteCheckedAnnotations, rewriteHostInputType, rewriteRequestTypes
  , NativeParsedModule, unannotatedModule, mapNativeModule, thenNativeModule, typecheckNativeModuleWithDiagnostics
  , resolveNativeAnnotations, annotateNativeModule)
import Tidepool.FinalizedModule (FinalizedModule, newFinalizedModule, finalizedHomeModInfo, finalizedTidyGuts, homeInterfaceUsageOwners)
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, LocalFinalizedAdmission, finalizedLocalAdmissions, localFinalizedInterface
  , localFinalizedSourceSha256, localFinalizedRequirements, localFinalizedHomeUnits, localFinalizedCore
  , matchesCapturedFinalization, localFinalizedPackageEvidence, finalizedInterfaceSeals )
import Tidepool.HomeProducts
  ( CandidateCoreFailure(..), CanonicalOriginalInputs, CanonicalOriginalInput, captureCanonicalOriginalInputs
  , lookupCanonicalOriginalInput, canonicalOriginalHome, canonicalOriginalProof
  , canonicalOriginalPackages, canonicalOriginalVersion, OriginalVersion, originalVersionOwner, AdmittedFinalizedOriginal
  , admittedOriginalModule, admittedOriginalProof
  , materializeCanonicalOriginalView )
import Tidepool.HomeProducts
  ( hydrateCandidateHomeProductsWithOriginalsUsing, validateCandidateCompilerViewProfile, materializeCandidateCompilerView, admittedCompilerInterface
  , validateCandidateInterfaceRequirements, validateCapturedAdmittedCore )
import Tidepool.CompileInputPolicy (pluginInputIssues)
import Tidepool.PlannedDeclaration
  ( PlannedDeclaration, plannedModule, plannedSource, plannedCheckPlan, certifyPlannedDeclaration, plannedOriginalOwner, preparePlannedDeclaration, originalDeclarationWrapper, replaceTemplateModuleHeader, PlannedDeclarationInventory, transformPlannedDeclarationImports, transformPlannedDeclarationImportsWithCompleted, transformProgramDeclarationImports, hydratePlannedDeclarationInventory )
import Tidepool.DeclarationJoin (DeclarationExport(..))
import Tidepool.CheckedPrefixImports
  ( CompletedValueImport(..), CompletedValueImports, hydrateCompletedValueImportsWithVerifiedDependencies
  , transformCompletedValueImports, refineParsedDeclarationImports, refineOriginalDeclarationImports, selectedImportNames
  , CompletedValueAliases, completedValueAliasIds, hydrateCompletedValueAliases, transformCompletedValueAliases )
import Tidepool.FamilyConsistency (validateCompilationFamilies, validateEnvironmentFamilies)
import Tidepool.TypePolicy (nominalHeadsOfType, stabilizeEffectRows)
import Tidepool.ExtractUtil (getLibdir, boundedUtf8, trySynchronous)
import Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences)
import Tidepool.Introspection (normalizeInspectionWildcards, withInspectionWork, runWorldInspectionGhc, inspectPublicModuleExports, encodeInspectionResults, InspectionResult(..), StructuredQueryError(..))
import Tidepool.ExtractRequest
  ( ExactInputsRequest(..), ExactInputsAcquisition(..), validExactInputsRequest, EnsureExactInputsOutcome(..), ExactInputsRejection(..)
  , WorldResidency(..), WorldNonRetentionReason(..), WorldAcquisitionMiss(..)
  , WorldQueryRequest(..), WorldQueryQuestion(..), WorldPublicQuestion(..), publicQuestionInspection, publicQuestionIsType, WorldObservationQuery(..), InspectionRequest(..), StructuredInspection(..), StructuredNameScope(..)
  , WorldCellPlanRequest(..), WorldCellPrepareRequest(..), WorldCellOperandRequest(..), WorldCellRecipe(..), PlannedCellSlot(..)
  , StructuredNameNamespace(..), InspectionProvenance(..) )
import Tidepool.Session
  ( SessionModule(..), SessionModuleKind(..), SessionScope(..)
  , Generation(..), sessionModuleString, capturedSessionInterfaceEvidence
  , isSessionScopeActive, injectSessionScopeWithCaptures, CapturedSessionInterface, capturedSessionInterface, registerSessionInterfaceLocation, renderSessionModule
  , scaffoldTargetName, scaffoldOutputBase, evalUserBinder, parseSessionModule, isReservedSessionModuleName
  , injectCapturedSessionIfaceWithBindings )
import Tidepool.Timing
  ( readTimingEnabled, timeSection, timePhase, emitPhase, emitCount
  , timeDetailPhase, timeTaskDetailPhase, TaskOwner(..), TaskComponent(..), ResourceTimingStart, beginResourceTiming, endResourceTiming
  , monotonicTime, elapsedMs
  , emitCompileSummary, emitModuleTiming, emitModuleInterfaceTiming
  , InterfaceStage(..), InterfaceReuse(..), measureModuleInterface
  , newTimingRequestIdentity
  , ReuseContext(..), ReuseModule(..), ReuseStage(..), ReuseDecision(..), ReuseReason(..)
  , ReuseVersionKind(..), emitReuse, emitReuseComplete, emitCheckOnlyReuseApplicability
  , WorldRetentionEvent(..), emitWorldRetention
  , readMemoTraceEnabled, MemoSelectionState(..), MemoSelectionTrace(..), MemoExecutableTrace(..)
  , emitMemoCycleGraph, emitMemoMissTrace )
import Tidepool.PreparedStg (PreparedModule, pmModule, preparedUsesSiteAuthority, resolvePreparedSiteEnvironment, preparedSiteDependenciesMatch, preparedSiteDependenciesEquivalent, acquirePreparedModuleWithSiteEnvironment, runPreparedModuleTask
  , newPreparedCanonicalModuleTaskPreparer, PreparedBodyReuse(..), PreparedBodyCache, newPreparedBodyCache, mergePreparedBodyCaches, selectPreparedBodyCaches, inheritPreparedBodyOwner, preparedBodyHasOriginalVersion, evictPreparedBodyMatching)
import Tidepool.FatIface
  ( FatIfaceCache, newFatIfaceCache, mergeFatIfaceCaches, selectFatIfaceCaches, evictFatIfaceMatching
  , OwnerInterfaceCache, newOwnerInterfaceCache, mergeOwnerInterfaceCaches, selectOwnerInterfaceCaches, evictOwnerInterfaceMatching)
import Tidepool.ExecutionProjection
  ( OriginalProjectionCache, newOriginalProjectionCache, mergeOriginalProjectionCaches, selectOriginalProjectionCaches, evictOriginalProjectionMatching
  , preparedRootIdentity )
import Tidepool.CompilerExecution
  ( CompilerExecutionGrant, serialCompilerExecutionGrant, compilerModuleJobs
  , CompilerExecutor, sameCompilerExecutor, requireCompilerExecutorOpen, withCompilerExecutor, runCompilerTasks, runCompilerTasksWithAttempt, dependencyClosedReuse )
import Tidepool.PreparedSites
  ( resolvePreparedSiblings, resolvePreparedInterfaceSiblings )
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.Identity (stableVarId, nameSymbolIdentity)
import Tidepool.TurnSource (generatedScaffoldModuleName)
import Tidepool.ExecutionEncode (encodeSymbol)
import Tidepool.TypedSegment.Types (TypedItemPlan(..), typedSegmentPlanItems)
import Tidepool.RetainedUnfoldings
  ( RetainedContext, retainedContext
  , installRetainedUnfoldingsPlugin, retainedDefinedBy
  , scopeRetainedModuleGraph, scopeRetainedSummaryHscEnv )
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyResolution(..)
  , DependencyModule(..), DependencyImport(..), DependencyQualifier(..), ProductAvailability(..)
  , sourceEvidenceWithFingerprint, selectedFreshHomeRequirements )
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, originalInterfaceSha256, ExactIfaceArtifact(..), depanalSourceModules, freshExactState, PackageFinderFacts, newPackageFinderFacts, forkExactContextWithPackageFacts, hydrateExactScope, hydrateExactScopeWithProducts, HydratedExactInterfaces, VerifiedExactInterface, verifiedExactInterface, exactInterfaceSummary, serializeOriginalInterface, exactHomeInstancesFor
  , ExactInterfaceOperations, exactInterfaceOperations
  , readVerifiedExactIfaceClosureWithCheckedValues, readVerifiedExactIfaceClosureWithCheckedValuesReader, selectVerifiedExactInterfaces, selectVerifiedValueInterfaces
  , noCheckedValueImports, checkedValueImportAuthorityFromVerified
  , GeneratedScaffoldRecipe, generatedScaffoldRecipe, captureGeneratedScaffoldTarget, captureGeneratedScaffoldSource
  , capturedGeneratedScaffoldBytes
  , CheckedTemplateImports(..), captureProtectedTemplateImportsAt, renderProtectedTemplateImports
  , generatedTypedSegmentRecipeWithProtectedImports, generatedCheckingTemplateRecipeWithProtectedImports, generatedCheckingTemplateRecipe
  , noGeneratedScaffoldImports, readGeneratedScaffoldImportAuthority, permitsGeneratedScaffoldImport, installExactLexicalGraphWithScaffold )
import Tidepool.ExactScope
  ( ExactScope , scopeManifestPath, scopeRequestSha256, scopeProducerSha256, scopeSemanticSha256, scopeLexical, scopeExecutionGraphs, scopeExecutionOwners, scopePurpose, scopeRequestTypes, scopeSourceSelectedOwners, scopeCompletedSourceOriginals, scopeInterfaces, scopeInterfaceEvidence, ExactScopePurpose(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..), SourceSelectedOriginals(..), revalidateSourceSelectedOriginals, extendSourceSelectedOriginals, CheckedCellAdmission(..), CheckedItemAdmission(..), readExactScope, validateExactScopeEnvironment, scopeInterfaceBytes, scopeOriginalBytes, readScopedInterfaceClosure, writeCheckedExactCompilation, scopeValueInterfaces
  , scopeAvailableOriginalProducts
  , ExactInputOwner, newExactInputOwner, readExactScopeWithOwner, validateExactScopeRequest
  , exactInputRetentionLimits, optionalInputByteLimit, originalImageCountLimit, commitWorldInputRetention
  , WorldInputFacts, WorldBinding, CapturedWorldSources, WorldSourceClosureEntry(..), readWorldInputsWithOwner, readWorldSuccessorWithOwner, WorldSuccessorInputs, worldSuccessorFacts, worldSuccessorChangedAliases, worldInputsIdentity, worldInputsIdentityBytes, worldInputsProducer, worldInputsProfile
  , worldInputsCompilerIdentity, worldInputsCompilerIdentityBytes, worldInputsSources, worldSourceRoots, worldSourceFiles, worldSourceDomainSha256, worldInputsSourceModuleHints, worldInputsSamePrimary, worldInputsRetainedCharge, worldInputsBindings, worldInputsBindingScope
  , worldInputsLexicalSurface, worldLexicalImportLines, worldLexicalDeclaration, worldDeclarationInterface
  , worldLexicalObservation, WorldObservationScaffold, worldObservationRequestTypes, worldObservationInputType
  , worldObservationFragment, worldObservationImports
  , worldBindingSymbol, worldBindingSessionVar, worldBindingScope, worldBindingGeneration, worldBindingRow, worldBindingRowFactCID
  , WorldOriginalReference, worldOriginalReferenceFor
  , worldBindingInterfaceArtifact, readWorldBindingInputs, worldHasSelectedSource, worldHasSelectedPrimary, readWorldSourceInputs
  , WorldCellOperandInputs, readWorldCellOperandWithOwner, worldCellOperandExposure, WorldCellOperandExposure(..)
  , worldCellOperandBinding, worldCellOperandInterface, worldCellOperandBody, worldCellOperandPackages, worldCellOperandTypes
  , WorldTypeClosureEntry, readWorldTypeInputs, worldTypeEntryArtifact, worldTypeEntryEvidence, worldTypeEntryVerifiedInterface, worldTypeEntryInterface, worldTypeEntryPackages, worldTypeEntryInterfaceBody, hydrateWorldDeclarationInputs, verifiedWorldDeclarationTypeInputs, verifiedWorldDeclarationInterface, verifiedWorldDeclarationExports
  , WorldInputsFailure(..), WorldInputsFailureCause(..)
  , ActivationPreviewAdmission(..), scopeActivationPreview
  , scopeCheckedCell, scopeCheckedItem, scopeIncludePaths
  , compilationOriginalSourceImports, scopeExecutionNativeOwners, ExactInterfaceEvidence(..), CanonicalOrigin(..), canonicalOrigin, normalizeInterfaceEvidence )
import Tidepool.ExactScope
  ( CanonicalInterfaceProof, CanonicalInterfaceAdmission(..), captureSourceOriginals, canonicalInterfaceInput, scopeCanonicalInterfaces
  , scopeSourceOriginalInterfaces, admittedInterfaceRequirements
  , validateCandidateCanonicalInterfaceProof, captureCanonicalProofs, CanonicalInterfaceUse(..), canonicalProofInterfaceBytes, revalidateCanonicalProofInputs
  , canonicalCertificateSha256, canonicalCoreArtifact
  , canonicalCorePath, canonicalCoreSha256, canonicalHomeUnits, canonicalRequirements
  , scopeModuleInterfaceProofs, canonicalSourceSha256, canonicalSourceImports, isSourceOriginal )
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), executionGraphBytes, executionGraphSha256, ExecutionSourceIdentity(..)
  , ExecutionSourceFailure(..), ExecutionSourceValidationStage(..), ExecutionSourceInterfaceReason(..)
  , ExecutionSourceRef(..), executionSourceClosure, executionIdentityKey
  , executionSourceGraphsFit
  , executionNodeIdentity, executionNodeModule, executionNodeSourceSha256, executionNodeRequirements )
import Tidepool.PackageWitness
  ( PackageImportEvidence(..), CompilerProvidedImport(..), emptyPackageImports, packageImportRoot, readPackageImports, decodeCapturedPackageImports
  , validatePackageImportRoot
  , withPackageRootObservations, packageImportRootWithin, validatePackageImportRootWithin )
import Tidepool.PackageWitness
  ( captureSupportedPackageProfileWith, requireObservationPackageEnvironment
  , ServicePackageAssociation, readCompilerPackageBoot, validateCompilerPackageBoot
  , compilerPackageBootLibdir
  , servicePackageWorkerInstance, servicePackageProfile
  , validateCompilerPackageBootConfiguration
  , emitCompilerPackageBootAcknowledgement )
import Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateImport(..), CandidateQualifier(..), CandidateGroup(..)
  , CapturedCandidateManifest, captureCandidateManifest
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
  CheckedPublicModule :: PipelineSelection CheckedPublicModuleResult
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
  { completionRecovery :: Maybe CompilerRecoveryCaches
  , completionSourceOwners :: Set.Set Module
  , completionSiblings :: Map.Map String Id
  , completionExternalOriginals :: Set.Set SymbolIdentity
  , completionExternalOriginalBindings :: Map.Map Name SymbolIdentity
  , completionReuseContext :: ReuseContext
  , completionCanonicalInterfaces :: Map.Map (String,String) CanonicalInterfaceAdmission
  , completionCanonicalOriginalInputs :: Maybe CanonicalOriginalInputs
  , completionAcquiredCanonicalOriginals :: Map.Map Module AdmittedFinalizedOriginal
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
  { ownedPprPipelineResult :: PipelineResult
  , ownedPprModules :: [PreparedModule]
  , ownedPprDependencies :: PreparedDependencies
  , ownedPprProductInterfaces :: Map.Map ModuleName ModIface
  , ownedPprFinalizedModules :: Map.Map ModuleName FinalizedModule
  , ownedPprPackageImports :: Map.Map ModuleName PackageImportEvidence
  , ownedPprCandidateAdmissions :: [AdmittedSourceCandidate]
  , ownedPprOriginalReferences :: Map.Map SymbolIdentity WorldOriginalReference
  , ownedPprOriginalBindings :: Map.Map Name SymbolIdentity
  , ownedPprReuseContext :: ReuseContext
  , ownedPprCanonicalOriginalInputs :: Maybe CanonicalOriginalInputs
  , ownedPprAcquiredCanonicalOriginals :: Map.Map Module AdmittedFinalizedOriginal
  }

-- Read accessors expose issued facts without record-update labels.
pprPipelineResult :: PreparedPipelineResult -> PipelineResult
pprPipelineResult = ownedPprPipelineResult

pprModules :: PreparedPipelineResult -> [PreparedModule]
pprModules = ownedPprModules

pprProductInterfaces :: PreparedPipelineResult -> Map.Map ModuleName ModIface
pprProductInterfaces = ownedPprProductInterfaces

pprFinalizedModules :: PreparedPipelineResult -> Map.Map ModuleName FinalizedModule
pprFinalizedModules = ownedPprFinalizedModules

pprCanonicalOriginalInputs :: PreparedPipelineResult -> Maybe CanonicalOriginalInputs
pprCanonicalOriginalInputs = ownedPprCanonicalOriginalInputs

pprAcquiredCanonicalOriginals :: PreparedPipelineResult -> Map.Map Module AdmittedFinalizedOriginal
pprAcquiredCanonicalOriginals = ownedPprAcquiredCanonicalOriginals

pprPackageImports :: PreparedPipelineResult -> Map.Map ModuleName PackageImportEvidence
pprPackageImports = ownedPprPackageImports

pprCandidateAdmissions :: PreparedPipelineResult -> [AdmittedSourceCandidate]
pprCandidateAdmissions = ownedPprCandidateAdmissions

pprOriginalBindings :: PreparedPipelineResult -> Map.Map Name SymbolIdentity
pprOriginalBindings = ownedPprOriginalBindings

pprOriginalReferences :: PreparedPipelineResult -> Map.Map SymbolIdentity WorldOriginalReference
pprOriginalReferences = ownedPprOriginalReferences

pprReuseContext :: PreparedPipelineResult -> ReuseContext
pprReuseContext = ownedPprReuseContext

-- The opaque admissions retain the issuer's selected descriptor, loading
-- demand and captured owner as one value through every prepared consumer.
pprAcceptedCandidates :: PreparedPipelineResult -> [ModuleCandidate]
pprAcceptedCandidates = map admittedCandidateOriginal . ownedPprCandidateAdmissions

preparedCandidateOriginal :: AdmittedSourceCandidate -> ModuleCandidate
preparedCandidateOriginal = admittedCandidateOriginal

preparedCandidateProof :: AdmittedSourceCandidate -> CanonicalInterfaceProof
preparedCandidateProof = admittedCandidateProof

revalidatePreparedCandidateInputs :: PreparedPipelineResult -> IO ()
revalidatePreparedCandidateInputs prepared =
  revalidateAdmittedCandidateInputs (ownedPrHscEnv (ownedPprPipelineResult prepared)) (ownedPprCandidateAdmissions prepared)

-- | One publication owns both source lookup and retained exact imports.
-- Constructors stay private so consumers cannot pair unrelated compilations.
data PreparedDependencies
  = SourceOnly DependencyEvidence
  | ExactScoped DependencyEvidence ExactCompilation CompletedSourceImports
  | WorldScoped DependencyEvidence BS.ByteString BS.ByteString (Maybe WorldPrivateCompletion)
      WorldSourceCaptureBasis

-- The admitted producer and source-import projection belong to this completed
-- World operation. Earlier operations retain their own captured certificates.
data WorldSourceCaptureBasis = WorldSourceCaptureBasis String
  [((String,String,Bool),[(DependencyQualifier,String,Bool,String)])]

-- A read-only projection of the actual compiler route. Absence of a finite
-- scope cannot classify a held World completion as ordinary source.
data PreparedSourceRoute
  = PreparedOrdinarySource
  | PreparedExactSource ExactCompilation
  | PreparedWorldSource BS.ByteString BS.ByteString

-- One earlier completed source paired with the capture that authenticated
-- its exact compiler object and package facts. It is compiler availability;
-- the consuming worklist still prepares and certifies any native groups.
data WorldSourceOriginal = WorldSourceOriginal !FinalizedModule !LocalFinalizedAdmission

worldSourceOriginalFinalization :: WorldSourceOriginal -> FinalizedModule
worldSourceOriginalFinalization (WorldSourceOriginal original _) = original

worldSourceOriginalPackages :: WorldSourceOriginal -> PackageImportEvidence
worldSourceOriginalPackages (WorldSourceOriginal _ proof) = localFinalizedPackageEvidence proof

acquireWorldSourceOriginal :: PreparedPipelineResult -> Module -> LocalFinalizedAdmission
  -> IO (Maybe WorldSourceOriginal)
acquireWorldSourceOriginal prepared owner proof = case ownedPprDependencies prepared of
  WorldScoped _ _ domain (Just (WorldPrivateCompletion _ (WorldCompletedDependencies policy versions _))) _ -> do
    let WorldDependencyPolicy _ _ _ selectedDomain = policy
        (artifact,_,_) = localFinalizedInterface proof
        environment = prHscEnv (pprPipelineResult prepared)
    unless (selectedDomain == domain && (exactUnit artifact,exactModule artifact) ==
        (unitString (moduleUnit owner),moduleNameString (moduleName owner))) (throwIO WorldCellCorrupt)
    case Map.lookup owner versions of
      Nothing -> throwIO WorldCellCorrupt
      Just byIngress -> do
        selected <- foldM (\known node -> do
          let payload = gmePayload (completedModuleEntry node)
              original = payloadFinalized payload
          matches <- matchesCapturedFinalization proof original
            (moduleFactPackageImports (payloadFacts payload))
          if not matches then pure known else case known of
            Nothing -> pure (Just original)
            -- Every match was checked against the same captured finalization
            -- identity and package witness by its owning admission above.
            Just _ -> pure known) Nothing (concatMap Map.elems (Map.elems byIngress))
        case selected of
          Just original -> do
            let home = finalizedHomeModInfo original
            current <- maybe (throwIO WorldCellCorrupt) pure (lookupHpt (hsc_HPT environment) (moduleName owner))
            unless (mi_module (hm_iface home) == owner && sameWorldCellHome current home
                && mi_hsc_src (hm_iface home) == HsSrcFile && mi_hsc_src (hm_iface current) == HsSrcFile)
              (throwIO WorldCellCorrupt)
            pure (if isJust (localFinalizedCore proof)
              then Just (WorldSourceOriginal original proof) else Nothing)
          _ -> throwIO WorldCellCorrupt
  _ -> pure Nothing

-- Source capture shares the exact issuer with checked compilation. Required
-- interface seals come from the actual original-interface owner, never names
-- reconstructed as source/native authority.
captureWorldSourceOriginals :: OriginalInterfaceArtifacts -> PreparedPipelineResult
  -> (String,String) -> FinalizedModuleArtifacts -> DependencyEvidence
  -> IO (Map.Map (String,String) CanonicalInterfaceProof)
captureWorldSourceOriginals originals prepared target finalized evidence = case ownedPprDependencies prepared of
  WorldScoped _ _ _ _ (WorldSourceCaptureBasis producer imports) -> do
    let inherited = map interfaceRow (Map.elems (prCanonicalInterfaceAdmissions (pprPipelineResult prepared)))
        requirements = Map.unions (map localFinalizedRequirements (Map.elems (finalizedLocalAdmissions finalized)))
    requiredSeals <- fmap Map.fromList $ forM (Map.toAscList requirements) $ \(key@(unit,name),expected) -> do
      actual <- originalInterfaceSha256 originals (mkModule (stringToUnit unit) (mkModuleName name))
        >>= maybe (throwIO WorldCellCorrupt) pure
      unless (actual == expected) (throwIO WorldCellCorrupt)
      pure (key,actual)
    let ownSeals = Map.fromList [((Text.unpack unit,Text.unpack name),Text.unpack sha)
          | ((unit,name),sha) <- finalizedInterfaceSeals finalized]
    captureSourceOriginals producer inherited [] (Map.union requiredSeals ownSeals)
      imports target finalized evidence
  _ -> pure Map.empty
  where
    interfaceRow (ModuleInterfaceAdmission proof) = canonicalInterfaceInput proof
    interfaceRow (LocalInterfaceAdmission proof) = localFinalizedInterface proof

preparedSourceRoute :: PreparedPipelineResult -> PreparedSourceRoute
preparedSourceRoute prepared = case ownedPprDependencies prepared of
  SourceOnly _ -> PreparedOrdinarySource
  ExactScoped _ exact _ -> PreparedExactSource exact
  WorldScoped _ base domain _ _ -> PreparedWorldSource base domain

-- Only the completed compiler pass captures which original authored requests
-- actually resolved to fresh home roots. Generated imports cannot issue one.
newtype CompletedSourceImports = CompletedSourceImports [(ImportIntent,(String,String))]

preparedDependencies
  :: HscEnv -> PipelineVariant -> DependencyEvidence -> Maybe ExactCompilation
  -> IO PreparedDependencies
preparedDependencies _ variant fresh exact
  | Just (WorldSourceInputBasis _ facts receipt _ _ _ _) <- pvWorldSources variant = case exact of
      Nothing -> do
        selected <- readIORef receipt
        imports <- case selected of
          Just (WorldSourceSelection _ imports _ _ _) -> pure imports
          Nothing -> throwIO WorldCellCorrupt
        let producer = hexBytes (worldInputsProducer facts)
        _ <- evaluate (length producer)
        pure (WorldScoped fresh (worldInputsIdentityBytes facts)
          (worldSourceDomainSha256 (worldInputsSources facts)) (pvWorldCompletedDependencies variant)
          (WorldSourceCaptureBasis producer imports))
      Just _ -> throwIO CheckedPurposeMismatch
preparedDependencies _ _ fresh Nothing = pure (SourceOnly fresh)
preparedDependencies env variant fresh (Just exact) = do
  roots <- fmap catMaybes $ forM (pvSourceImportIntents variant) $ \intent -> case intent of
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
preparedFreshDependencies prepared = case ownedPprDependencies prepared of
  SourceOnly fresh -> fresh
  ExactScoped fresh _ _ -> fresh
  WorldScoped fresh _ _ _ _ -> fresh

-- | Exact-scope evidence only; fresh source imports remain in their own view.
preparedExactCompilation :: PreparedPipelineResult -> Maybe ExactCompilation
preparedExactCompilation prepared = case preparedSourceRoute prepared of
  PreparedExactSource exact -> Just exact
  _ -> Nothing

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
  case ownedPprDependencies prepared of
    SourceOnly _ -> pure previous
    WorldScoped _ _ _ _ _ -> throwIO CheckedPurposeMismatch
    ExactScoped _ _ (CompletedSourceImports []) -> pure previous
    ExactScoped fresh compilation (CompletedSourceImports roots) -> do
      let admitted = compilationScope compilation
          env = ownedPrHscEnv (ownedPprPipelineResult prepared)
      case programImportIdentity admitted of
        Nothing -> pure previous
        Just identity -> do
          unless (programImportIdentity retained == Just identity)
            (fail "completed program imports leave their checked cell")
          either fail pure =<< validateExactScopeEnvironment env retained
          let freshNodes = Map.fromList [((dependencyModuleUnit node,dependencyModuleName node),node)
                | node <- dependencyModules fresh, not (dependencyModuleBoot node)]
              sourceMatches key originalRow@(ProgramImportOriginal row evidence lexical) =
                case (Map.lookup key freshNodes, evidence) of
                  (Nothing, _) -> pure (programImportOriginal admitted key == Just originalRow
                    && key `Set.notMember` scopeSourceSelectedOwners admitted)
                  (Just node, ModuleInterfaceEvidence canonical)
                    | Just original <- Map.lookup (mkModuleName (snd key)) (ownedPprFinalizedModules prepared)
                    , mi_module (hm_iface (finalizedHomeModInfo original)) ==
                        mkModule (stringToUnit (fst key)) (mkModuleName (snd key))
                    , Just packages <- Map.lookup (mkModuleName (snd key)) (ownedPprPackageImports prepared)
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
                          , dependencySourcePath source == dependencyModuleSource node] ->
                        matchesCapturedFinalization finalized original packages
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
-- Package imports confer no home requirement. The lexical row is one canonical
-- set, independent of its fresh/retained partition or authored import order.
preparedHomeRequirements :: PreparedPipelineResult -> String -> String -> Either String [(String, String)]
preparedHomeRequirements prepared unit owner = do
  fresh <- selectedFreshHomeRequirements (preparedFreshDependencies prepared) unit owner
  let exact = [(importedUnit,name)
        | compilation <- maybe [] pure (preparedExactCompilation prepared)
        , ((sourceUnit,sourceName,False), edges) <- compilationExactImports compilation
        , sourceUnit == unit, sourceName == owner
        , (_,name,_,importedUnit) <- edges]
  pure (Set.toAscList (Set.fromList (fresh ++ exact)))

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

-- Issued only by successful checking of the actual demanded module. No mutable
-- target typechecker environment or executable target survives this projection.
data CheckedPublicModuleResult = CheckedPublicModuleResult
  HomeModInfo HomePackageTable DependencyEvidence [Diag]

checkedPublicModuleIdentity :: CheckedPublicModuleResult -> Module
checkedPublicModuleIdentity (CheckedPublicModuleResult home _ _ _) = mi_module (hm_iface home)

checkedPublicModuleEvidence :: CheckedPublicModuleResult -> DependencyEvidence
checkedPublicModuleEvidence (CheckedPublicModuleResult _ _ evidence _) = evidence

data WorldModulePolicy = WorldLiterateSource | WorldPreprocessing
  | WorldUntrackedOptions | WorldCodeGeneration | WorldSideOutputs | WorldOutputPolicyChanged
  | WorldSearchInputsChanged | WorldUncapturedDependency | WorldPrivateModule
  deriving (Eq, Show)

data WorldModulePolicyFailure = WorldModulePolicyFailure FilePath WorldModulePolicy
  deriving (Show)

instance Exception WorldModulePolicyFailure

selectionKind :: PipelineSelection result -> PreparationKind
selectionKind PreparedStg = PrepareStg
selectionKind (PreparedProducts _) = PrepareStg
selectionKind (PreparedSegmentProducts _ _) = PrepareStg
selectionKind (WithTypedSegmentPreparation _ inner) = selectionKind inner
selectionKind CheckedEnvironment = CheckOnly
selectionKind CheckedPublicModule = CheckOnly
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

checksPublicModule :: PipelineSelection result -> Bool
checksPublicModule CheckedPublicModule = True
checksPublicModule (WithCompilerExecution _ _ inner) = checksPublicModule inner
checksPublicModule (WithPreparedModuleCompletion _ inner) = checksPublicModule inner
checksPublicModule (WithTypedSegmentPreparation _ inner) = checksPublicModule inner
checksPublicModule _ = False

data PipelineResult = PipelineResult
  { ownedPrBinds  :: [CoreBind]
  , ownedPrHscEnv :: HscEnv
  -- | Actual thin-interface bytes selected by session injection.
  -- These authorize type-dependency seals, not source or native products.
  , ownedPrInjectedSessionInterfaces :: [CapturedSessionInterface]
  -- | New typed capture outputs, kept distinct from injected inputs.
  , ownedPrProducedSessionInterfaces :: [CapturedSessionInterface]
  -- | Verified canonical owners selected by this exact compiler request.
  -- Retained home interfaces have no source location in the GHC finder.
  , ownedPrCanonicalInterfaceAdmissions :: Map.Map (String,String) CanonicalInterfaceAdmission
  -- | The GHC-inferred type of the target module's @__user@ binding (the eval's
  -- top-level expression), rendered to a string via 'ppr'. 'Nothing' when no
  -- @__user@ binding is present (e.g. fixture/Suite extraction). Captured at the
  -- typecheck stage because our CBOR serializer strips all type information.
  --
  -- This display string is not parser-faithful: 'ppr' can elide qualifiers or
  -- use Unicode. Cross-turn typechecking must use structured type data.
  , ownedPrCapturedType :: Maybe String
  -- | Post-zonk types of compiler-reserved local aliases emitted by the cell
  -- checker. These are structured separately from display-only inspection
  -- strings because the runtime replants them into staged statement compiles.
  , ownedPrCheckedBinderPins :: [CheckedBinderPin]
  -- | The GHC 'Type' of the target module's @result@ binding, captured for the
  -- value-binding mode. For @result = do { x <- action;
  -- pure x } :: Eff stack T@ this is the FULL @Eff stack T@; 'stripMonadHead'
  -- recovers the bound value type @T@. 'Nothing' when the module has no @result@
  -- binder (every non-bind extraction — reference turns, fixtures, one-shot
  -- evals — so the field is inert off the bind path).
  , ownedPrResultType :: Maybe Type
  -- | GHC diagnostic warnings (@-Wincomplete-patterns@, name shadowing, ...)
  -- emitted while compiling the TARGET module — dependency modules (the
  -- preamble, stdlib) are excluded, see 'diagnosticCollectorHook'. Rendered by
  -- GHC's own diagnostic pretty-printer, so a warning carries its
  -- @Expr.hs:<line>:<col>@ location exactly like a compile error does. Empty
  -- on a clean compile.
  , ownedPrWarnings :: [String]
  -- | GHC's resolved reader environment for the target module. Inspection
  -- uses this exact scope rather than reconstructing visibility from source.
  , ownedPrTargetRdrEnv :: GlobalRdrEnv
  , ownedPrTargetTcGblEnv :: TcGblEnv
  }

-- Read accessors expose issued facts without record-update labels.
prBinds :: PipelineResult -> [CoreBind]
prBinds = ownedPrBinds

prHscEnv :: PipelineResult -> HscEnv
prHscEnv = ownedPrHscEnv

prInjectedSessionInterfaces :: PipelineResult -> [CapturedSessionInterface]
prInjectedSessionInterfaces = ownedPrInjectedSessionInterfaces

prProducedSessionInterfaces :: PipelineResult -> [CapturedSessionInterface]
prProducedSessionInterfaces = ownedPrProducedSessionInterfaces

prCanonicalInterfaceAdmissions :: PipelineResult -> Map.Map (String,String) CanonicalInterfaceAdmission
prCanonicalInterfaceAdmissions = ownedPrCanonicalInterfaceAdmissions

prCapturedType :: PipelineResult -> Maybe String
prCapturedType = ownedPrCapturedType

prCheckedBinderPins :: PipelineResult -> [CheckedBinderPin]
prCheckedBinderPins = ownedPrCheckedBinderPins

prResultType :: PipelineResult -> Maybe Type
prResultType = ownedPrResultType

prWarnings :: PipelineResult -> [String]
prWarnings = ownedPrWarnings

prTargetRdrEnv :: PipelineResult -> GlobalRdrEnv
prTargetRdrEnv = ownedPrTargetRdrEnv

prTargetTcGblEnv :: PipelineResult -> TcGblEnv
prTargetTcGblEnv = ownedPrTargetTcGblEnv


checkCellInstances
  :: (CellSourcePlan -> IO result)
  -> CellSourcePlan
  -> IO (CellSourcePlan, result)
checkCellInstances compile plan = do
  attempted <- try (compile plan)
  case attempted of
    Right result -> pure (plan, result)
    Left (GeneratedInstanceRejection failure rejected) -> do
      let remaining = retryCellInstancePlan rejected plan
      if remaining == plan
        then throwIO failure
        else checkCellInstances compile remaining

retryCellInstancePlan :: [GeneratedInstanceFailure] -> CellSourcePlan -> CellSourcePlan
retryCellInstancePlan rejected plan = omitCellStructuralDisplayDeclarations structural
  (omitCellGenericDeclarations generic plan)
  where
    generic = [name | GenericDuplicate name <- rejected] ++ [name | UnsupportedGeneric name <- rejected]
    structural = [name | StructuralDisplayCompanionDuplicate name <- rejected] ++ [name | UnsupportedGeneric name <- rejected]

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

-- | Select the target identity issued by the source downsweep. Source paths
-- distinguish the requested module from its dependencies; header spelling,
-- CPP and the implicit Main owner remain GHC's decisions.
targetModuleFor :: ModuleGraph -> FilePath -> IO Module
targetModuleFor graph path = do
  requested <- canonicalizePath path
  matches <- filterM (\summary -> case ml_hs_file (ms_location summary) of
    Nothing -> pure False
    Just source -> (== requested) <$> canonicalizePath source)
    [summary | ModuleNode _ summary <- mgModSummaries' graph
      , ms_hsc_src summary == HsSrcFile]
  case matches of
    [summary] -> pure (ms_mod summary)
    _ -> fail "source downsweep target summary is missing or duplicated"

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
  variant <- normalVariant GeneralCompile
  runCompile selection retained variant {pvCompilerProducer = producer} path includes Nothing

-- The process owner supplies this configuration before any authored compiler
-- effect runs. A resident compiler retains it across requests and recovery;
-- candidates and mutable source code cannot replace their own authority.
newtype CompilerProducerIdentity = CompilerProducerIdentity BS.ByteString
  deriving (Eq, Show)

data NativeProductProvenance = NativeProductProvenance !BS.ByteString !BS.ByteString

nativeProductProducer :: NativeProductProvenance -> BS.ByteString
nativeProductProducer (NativeProductProvenance producer _) = producer

nativeProductProfile :: NativeProductProvenance -> BS.ByteString
nativeProductProfile (NativeProductProvenance _ profile) = profile

data NativeProductConfigurationUnavailable = NativeProductConfigurationUnavailable
  deriving Show
instance Exception NativeProductConfigurationUnavailable

captureCompilerProducerIdentity :: IO (Maybe CompilerProducerIdentity)
captureCompilerProducerIdentity = lookupEnv "TIDEPOOL_COMPILER_PRODUCER" >>= traverse decode
  where
    decode raw
      | length raw /= 64 || any (`notElem` ("0123456789abcdef" :: String)) raw
          || all (== '0') raw = fail "invalid compiler launch producer identity"
      | otherwise = pure (CompilerProducerIdentity (SHA256.hash (BS.pack (pairs raw))))
    pairs [] = []
    pairs (first:second:remaining) =
      fromIntegral (16 * digitToInt first + digitToInt second) : pairs remaining
    pairs _ = error "validated compiler producer has odd length"

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
  , pvWorldOnly :: Bool
  , pvWorldOriginalBindings :: Map.Map Name SymbolIdentity
  , pvWorldCompletedDependencies :: Maybe WorldPrivateCompletion
  , pvWorldSources :: Maybe WorldSourceInputBasis
    -- ^ Admitted immutable declaration owners, independent of live values.
  , pvCompilerProducer :: Maybe CompilerProducerIdentity
  , pvGeneratedScaffold :: Maybe GeneratedScaffoldRecipe
    -- ^ The protected target and its compiler-owned support import.
  , pvDownsweepExcludes :: [ModuleName]
    -- ^ Modules @depanal@ must NOT try to summarise (the session path's
    -- source-less @Val.G\<g\>@ ifaces). Empty on the normal path.
  , pvPlan :: FilePath -> Bool -> ModuleName -> ModuleGraph -> Maybe ExactScope -> Ghc CompilePlan
    -- ^ @pvPlan compilerViewDirectory timingEnabled targetName downsweepGraph selectedExactScope@.
  , pvGeneratedInstanceCheck :: Maybe GeneratedInstanceRecipe
  , pvSourceImportIntents :: [ImportIntent]
  , pvTransformParsed :: ModuleName -> HscEnv -> ModSummary -> ParsedModule -> IO NativeParsedModule
  }

-- Operation-local source admission reuses the actual held facts. Only its
-- explicit selected-source receipt leaves the compile; no callback owns a world.
data WorldSourceInputBasis = WorldSourceInputBasis ExactInputOwner WorldInputFacts
  (IORef (Maybe WorldSourceSelection))
  ParsedModuleDemand [(ExactIfaceArtifact,ModIface)] WorldImportBasis WorldPrefixLexicalOwners

-- Issued only from the actual completed declaration prefix. This projection
-- retains lexical ownership without its GHC homes, source paths or buffers.
newtype WorldPrefixLexicalOwners = WorldPrefixLexicalOwners (Map.Map ModuleName String)

mergeWorldLexicalOwners :: [Map.Map ModuleName String] -> IO (Map.Map ModuleName String)
mergeWorldLexicalOwners = foldM insert Map.empty . concatMap Map.toAscList
  where
    insert owners (name,unit) = do
      forM_ (Map.lookup name owners) $ \previous -> unless (previous == unit) $
        throwIO (WorldInputsFailure WorldInputsCorrupt "World lexical import has conflicting exact units")
      pure (Map.insert name unit owners)

-- TYPE custody and lexical adjacency are separate: requirement-only rows
-- hydrate GHC details but never become lexical imports or native grants.
-- This request-local value retains neither a compiler environment nor Core.
data WorldImportBasis = WorldImportBasis
  (Map.Map (String,String) WorldTypeClosureEntry)
  [(ExactIfaceArtifact, [(String,String)])]
  [(String,String)]

readWorldImportBasis :: ExactInputOwner -> HscEnv -> WorldInputFacts
  -> [ImportIntent] -> ParsedModuleDemand -> IO WorldImportBasis
readWorldImportBasis owner environment facts intents parsed = do
  roots <- fmap Set.fromList $ fmap catMaybes $ forM (moduleImportRequests parsed) $ \(name,raw,boot) -> do
    let qualifier = renameRawPkgQual (hsc_unit_env environment) name raw
        home = homeUnitId (hsc_home_unit environment)
        selected = case qualifier of
          NoPkgQual -> Just (unitString home,moduleNameString name)
          ThisPkg unit | unit == home -> Just (unitString unit,moduleNameString name)
          _ -> Nothing
        authored = AuthoredSourceImport name raw `elem` intents
    case selected of
      Just key | boot == NotBoot, not authored, worldHasSelectedPrimary facts key -> do
        found <- findImportedModule environment name qualifier
        -- A captured current source is qualified by the existing source
        -- validator, including its exact bytes and SOURCE/boot graph.
        pure $ case found of
          Found location actual
            | (unitString (moduleUnit actual),moduleNameString (moduleName actual)) == key
            , isJust (ml_hs_file location) -> Nothing
          _ -> Just key
      _ -> pure Nothing
  if Set.null roots then pure (WorldImportBasis Map.empty [] []) else do
    entries <- readWorldTypeInputs owner environment facts (Set.toAscList roots)
      >>= either (throwIO . WorldInputsFailure WorldInputsCorrupt) pure
    lexical <- lexicalClosure entries Set.empty (Set.toAscList roots)
    sources <- fmap catMaybes $ forM (Map.toAscList entries) $ \(key,entry) -> do
      found <- findImportedModule environment (mkModuleName (snd key)) (ThisPkg (homeUnitId (hsc_home_unit environment)))
      case found of
        Found location actual
          | (unitString (moduleUnit actual),moduleNameString (moduleName actual)) == key
          , isJust (ml_hs_file location) -> case worldTypeEntryEvidence entry of
              ModuleInterfaceEvidence proof | isSourceOriginal (canonicalOrigin proof) -> pure (Just key)
              _ -> unsupported
        _ -> do
          unless (not (any (\(name,raw,boot) ->
              let qualifier = renameRawPkgQual (hsc_unit_env environment) name raw
                  local = qualifier == NoPkgQual || qualifier == ThisPkg (homeUnitId (hsc_home_unit environment))
              in local && moduleNameString name == snd key
                && (boot == IsBoot || AuthoredSourceImport name raw `elem` intents)) (moduleImportRequests parsed))) $
            throwIO (WorldInputsFailure WorldInputsUnavailable "current source import lacks captured source")
          pure Nothing
    pure (WorldImportBasis entries lexical sources)
  where
    lexicalClosure _ _ [] = pure []
    lexicalClosure entries visited (key:rest)
      | key `Set.member` visited = lexicalClosure entries visited rest
      | otherwise = do
          entry <- maybe (throwIO (WorldInputsFailure WorldInputsCorrupt
            "carried import lost its selected TYPE closure owner")) pure (Map.lookup key entries)
          imports <- case worldTypeEntryEvidence entry of
            ModuleInterfaceEvidence proof -> maybe unsupported pure (canonicalSourceImports proof)
            _ -> unsupported
          let edges = Set.toAscList (Set.fromList [(unit,name) | (_,name,_,Just unit) <- imports])
          unless (all (`Map.member` entries) edges) $
            throwIO (WorldInputsFailure WorldInputsCorrupt "carried lexical import lacks its exact TYPE association")
          remaining <- lexicalClosure entries (Set.insert key visited) (edges ++ rest)
          pure ((worldTypeEntryArtifact entry,edges):remaining)
    unsupported = throwIO (WorldInputsFailure WorldInputsCorrupt
      "carried import lacks canonical source-import provenance")

worldImportEntries :: WorldImportBasis -> [WorldTypeClosureEntry]
worldImportEntries (WorldImportBasis entries _ _) = Map.elems entries

qualifyWorldImportSources :: WorldImportBasis -> Ghc ()
qualifyWorldImportSources (WorldImportBasis _ _ []) = pure ()
qualifyWorldImportSources (WorldImportBasis entries _ sources) = do
  environment <- getSession
  let selected = Map.restrictKeys entries (Set.fromList sources)
      proofs = Map.mapMaybe (\entry -> case worldTypeEntryEvidence entry of
        ModuleInterfaceEvidence proof -> Just proof
        _ -> Nothing) selected
      interfaces = [(worldTypeEntryArtifact entry,worldTypeEntryInterface entry) | entry <- Map.elems entries]
      readPackages key = maybe (throwIO (ExecutionSourcePackageChanged key))
        (pure . worldTypeEntryPackages) (Map.lookup key entries)
  _ <- withHeldWorldSourceRefusal $ validateCanonicalSourceInputs True
    (Just (map worldTypeEntryVerifiedInterface (Map.elems entries))) proofs interfaces
    (map fst interfaces) [] readPackages (hsc_mod_graph environment) sources
  pure ()

hydrateWorldImportBasis :: WorldImportBasis -> HscEnv -> IO HscEnv
hydrateWorldImportBasis basis environment = fst <$> hydrateExactScopeWithProducts environment
  (map worldTypeEntryVerifiedInterface (worldImportEntries basis)) []

-- Make rebuilds phase environments from its load graph. Reinstall the same
-- admitted interfaces there, while keeping lexical instance reachability on
-- the exact source-import graph rather than the broader TYPE requirement set.
restoreWorldImportBasis :: WorldImportBasis -> HscEnv -> IO HscEnv
restoreWorldImportBasis basis@(WorldImportBasis _ lexical _) environment
  | null lexical = pure environment
  | otherwise = do
      let owners = Set.fromList [(exactUnit artifact,exactModule artifact) | (artifact,_) <- lexical]
          key summary = (unitString (moduleUnit (ms_mod summary)),moduleNameString (ms_mod_name summary))
          nodes = mgModSummaries' (hsc_mod_graph environment)
      unless (null [() | ModuleNode _ summary <- nodes, key summary `Set.member` owners
          , isJust (ml_hs_file (ms_location summary))]) $
        throwIO (WorldInputsFailure WorldInputsCorrupt "carried interface collides with current source")
      ready <- hydrateWorldImportBasis basis environment
      let ordinary (ModuleNode _ summary) = key summary `Set.notMember` owners
          ordinary _ = True
      installExactLexicalGraphWithScaffold (mkModuleGraph (filter ordinary nodes)) lexical
        noCheckedValueImports noGeneratedScaffoldImports ready
        >>= either (throwIO . WorldInputsFailure WorldInputsCorrupt) pure

data WorldSourceSelection = WorldSourceSelection (Maybe SourceSelectedOriginals)
  [((String,String,Bool),[(DependencyQualifier,String,Bool,String)])] [WorldTypeClosureEntry]
  (Map.Map (String,String) CanonicalInterfaceAdmission) (Maybe CanonicalOriginalInputs)

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
  Just (CompilerProducerIdentity producer) -> Just (hexBytes producer)
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
  deriving (Eq, Show)

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
  , universeOriginalInputs :: ExactInputOwner
  , universePackageAssociation :: Maybe ServicePackageAssociation
  , universeWorlds :: CompilerWorldCache
  , universeWorldDirectory :: Maybe FilePath
  }

-- Immutable logical inputs and the NameCache-bound HPT have separate owners.
-- This realization is indexed by its universe and strongly borrowed by one
-- active compiler scope through confirmed END.
-- Only actual World acquisition issues this accelerator. Transport strings
-- are compared at the scope boundary; they do not construct held bindings.
newtype IssuedWorldRealization = IssuedWorldRealization String deriving (Eq)

worldRealizationText :: IssuedWorldRealization -> String
worldRealizationText (IssuedWorldRealization value) = value

newWorldRealization :: BS.ByteString -> IO IssuedWorldRealization
newWorldRealization instanceNonce = do
  unique <- RequestUnique.hashUnique <$> RequestUnique.newUnique
  pure (IssuedWorldRealization (hexBytes instanceNonce ++ "." ++ show unique))

data HeldCompilerWorld = HeldCompilerWorld WorldInputFacts IssuedWorldRealization WorldMaterialization (IORef WorldPreparedModules)

-- A retained closed request carries correlation, never a second payload hold.
-- The scope revalidates this binding and borrows its actual held payload only
-- after the fresh executor and phase checks have succeeded.
data HeldWorldQueryBinding = HeldWorldQueryBinding !String !IssuedWorldRealization

data WorldHomeSource = WorldHomeSource Int FilePath FilePath BS.ByteString
data WorldMaterialization = WorldMaterialization FilePath FilePath [FilePath]
  (Map.Map ModuleName WorldHomeSource) (Map.Map FilePath WorldHomeSource) (Set.Set String) !Integer !Int WorldBackingLease
data WorldPreparedModules = WorldPreparedModules HomePackageTable (Map.Map Module WorldHomeSource)
  (Maybe WorldCompletedDependencies) (Maybe (SegmentDemandScope WorldDemandBinding))

type WorldModuleVersions = Map.Map Module (Map.Map MemoSelectionKey (Map.Map MemoValidity CompletedModuleVersion))

-- This policy is issued only by the fixed world-cell route. The worker instance
-- owns the boot flags; captured source bytes own module-local OPTIONS pragmas.
-- Public-module metadata never issues this policy or completed dependency rows.
data WorldDependencyPolicy = WorldDependencyPolicy BS.ByteString BS.ByteString BS.ByteString BS.ByteString
  deriving (Eq)
newtype WorldCanonicalRecovery = WorldCanonicalRecovery (Map.Map OriginalVersion RecoveryContext)

emptyWorldCanonicalRecovery :: WorldCanonicalRecovery
emptyWorldCanonicalRecovery = WorldCanonicalRecovery Map.empty

mergeWorldCanonicalRecovery :: WorldCanonicalRecovery -> WorldCanonicalRecovery -> WorldCanonicalRecovery
mergeWorldCanonicalRecovery (WorldCanonicalRecovery additions) (WorldCanonicalRecovery previous) =
  WorldCanonicalRecovery (Map.union additions previous)

data WorldCompletedDependencies = WorldCompletedDependencies WorldDependencyPolicy WorldModuleVersions !WorldCanonicalRecovery

data WorldDependencyAdditions = WorldDependencyAdditions !WorldModuleVersions !WorldCanonicalRecovery

data WorldDependencyAttempt = WorldDependencyAttempt
  String IssuedWorldRealization WorldDependencyPolicy (Map.Map FilePath WorldHomeSource)
  WorldModuleVersions !WorldCanonicalRecovery ModIfaceCache (IORef GutsMemo) (IORef CompilerRecoveryCaches)
  (IORef CompilerInterpreterState)

-- No held world, mutable attempt, source tree or backing lease escapes in this
-- pending result. The scope resolves the actual held owner again at commit.
data PendingWorldDependencies = PendingWorldDependencies CompilerExecutor String IssuedWorldRealization WorldDependencyPolicy
  !WorldCanonicalRecovery !CompilerRecoveryCaches (IORef (Maybe WorldDependencyAdditions))

-- Immutable private versions remain paired with the actual pending completion
-- that issued them. Borrowing is not publication or a second attempt owner.
data WorldPrivateCompletion = WorldPrivateCompletion !PendingWorldDependencies !WorldCompletedDependencies

emptyWorldPreparedModules :: WorldPreparedModules
emptyWorldPreparedModules = WorldPreparedModules emptyHomePackageTable Map.empty Nothing Nothing

worldDependencyPolicy :: CompilerUniverse -> WorldInputFacts -> IO WorldDependencyPolicy
worldDependencyPolicy universe facts = case universePackageAssociation universe of
  Just association
    | servicePackageProfile association == worldInputsProfile facts -> pure
        (WorldDependencyPolicy (servicePackageWorkerInstance association)
          (worldInputsProducer facts) (worldInputsProfile facts)
          (worldSourceDomainSha256 (worldInputsSources facts)))
  _ -> throwIO WorldCellCorrupt

heldWorldCellFlags :: HscEnv -> FilePath -> [FilePath] -> DynFlags
heldWorldCellFlags baseEnv directory roots = foldl' gopt_unset ((hsc_dflags baseEnv)
  {backend=noBackend,ghcLink=NoLink,importPaths=roots,workingDirectory=Nothing
  ,hiDir=Just directory,objectDir=Just directory,stubDir=Just directory})
  [Opt_WriteInterface,Opt_WriteIfSimplifiedCore,Opt_ByteCode,Opt_ByteCodeAndObjectCode]

beginHeldWorldDependencyAttempt :: CompilerUniverse -> HeldCompilerWorld -> Maybe WorldPrivateCompletion -> IO WorldDependencyAttempt
beginHeldWorldDependencyAttempt universe
    (HeldCompilerWorld facts reference (WorldMaterialization _ _ _ _ paths _ _ _ _) preparedRef) private = do
  policy <- worldDependencyPolicy universe facts
  WorldPreparedModules _ _ completed _ <- readIORef preparedRef
  committed <- case completed of
    Nothing -> pure (Map.empty,emptyWorldCanonicalRecovery)
    Just (WorldCompletedDependencies issued selected recovery)
      | issued == policy -> pure (selected,recovery)
      | otherwise -> throwIO WorldCellCorrupt
  (versions,canonicalRecovery) <- case private of
    Nothing -> pure committed
    Just (WorldPrivateCompletion _ (WorldCompletedDependencies issued selected recovery))
      | issued == policy -> pure (mergeWorldModuleVersions selected (fst committed),
          mergeWorldCanonicalRecovery recovery (snd committed))
      | otherwise -> throwIO WorldCellCorrupt
  cache <- newIfaceCache
  memo <- newIORef Map.empty
  recovery <- selectCompilerRecoveryCaches (universeEnvironment universe) (universeRecovery universe) []
  recoveryRef <- newIORef recovery
  pure (WorldDependencyAttempt (worldInputsIdentity facts) reference policy paths versions canonicalRecovery cache memo recoveryRef (universeInterpreter universe))

worldDependencyCycle :: WorldDependencyAttempt -> CompilerUniverse -> IORef InterpreterAttemptState -> CycleState
worldDependencyCycle (WorldDependencyAttempt _ _ _ _ versions canonicalRecovery cache memo recoveryRef _) universe attempt =
  TransactionCycle cache memo Nothing versions canonicalRecovery activate
    (universeInterpreter universe) (universePackageFinder universe) attempt
  where
    activate selected = do
      recovery <- selectCompilerRecoveryCaches (universeEnvironment universe) (universeRecovery universe)
        [(owner,completedModuleRecovery node) | (owner,node) <- Map.toList selected]
      writeIORef recoveryRef recovery
      pure recovery

worldDependencyRecovery :: WorldDependencyAttempt -> IO CompilerRecoveryCaches
worldDependencyRecovery (WorldDependencyAttempt _ _ _ _ _ _ _ _ recoveryRef _) = readIORef recoveryRef

stageHeldWorldDependencies :: CompilerExecutor -> WorldDependencyAttempt -> PipelineVariant -> HscEnv -> FilePath -> IO PendingWorldDependencies
stageHeldWorldDependencies executor
    (WorldDependencyAttempt identity reference policy paths previous canonicalRecovery _ memo recoveryRef interpreter) variant final path = do
  target <- targetModuleFor (hsc_mod_graph final) path
  recovery <- readIORef recoveryRef
  recoveryIdentity <- RequestUnique.newUnique
  snapshot <- readIORef memo
  epoch <- compilerInterpreterEpoch <$> readIORef interpreter
  let sourceOwners = Map.fromList
        [(ms_mod summary,source)
        | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph final)
        , ms_hsc_src summary == HsSrcFile
        , ms_mod summary /= target
        , not (isReservedSessionModuleName (moduleNameString (ms_mod_name summary)))
        , Just physical <- [normalise <$> ml_hs_file (ms_location summary)]
        , Just source <- [Map.lookup physical paths]]
      eligible = Map.filter ((`Map.member` sourceOwners) . payloadOwner . gmePayload)
        (admittedSourceMemoSnapshot variant final snapshot)
      context = RecoveryContext recoveryIdentity recovery
      -- The normal cycle already validated each selected source version before
      -- entering the working memo. Only captured source owners can be added;
      -- private value aliases and earlier checking targets have no such row.
      additions = Map.fromListWith Map.union
        [(owner,Map.singleton (memoSelectionKey validity)
          (Map.singleton validity (CompletedModuleVersion entry home context epoch products)))
        | entry <- Map.elems eligible
        , let owner = payloadOwner (gmePayload entry)
        , let validity = gmeValidity entry
        , let home = retainCompletedModuleHome (lookupHpt (hsc_HPT final) (moduleName owner))
        , let products = maybe [] pure (payloadProduct (gmePayload entry))
        , let previousVersion = do
                bySelection <- Map.lookup owner previous
                byValidity <- Map.lookup (memoSelectionKey validity) bySelection
                Map.lookup validity byValidity
        , maybe True (\node -> gmeCycle (completedModuleEntry node) /= gmeCycle entry
            || completedModuleEpoch node /= epoch) previousVersion]
  evictCompilerRecovery recovery (== target)
  _ <- evaluate (Map.size additions)
  pending <- newIORef (Just (WorldDependencyAdditions additions emptyWorldCanonicalRecovery))
  pure (PendingWorldDependencies executor identity reference policy canonicalRecovery recovery pending)

-- Borrowing a successful private stage does not consume its eventual
-- publication. The admission owner already binds this continuation to one
-- executor and held World; publication remains independently one-shot.
borrowHeldWorldDependencies :: CompilerExecutor -> WorldInputFacts -> String
  -> Maybe WorldPrivateCompletion -> PendingWorldDependencies -> IO WorldPrivateCompletion
borrowHeldWorldDependencies executor facts reference previous
    completion@(PendingWorldDependencies issued identity realization policy _ _ pending) = do
  unless (sameCompilerExecutor executor issued && identity == worldInputsIdentity facts
      && worldRealizationText realization == reference) (throwIO WorldCellCorrupt)
  requireCompilerExecutorOpen executor
  WorldDependencyAdditions additions recovery <- readIORef pending >>= maybe (throwIO CheckedPurposeMismatch) pure
  (retained,oldRecovery) <- case previous of
    Nothing -> pure (Map.empty,emptyWorldCanonicalRecovery)
    Just (WorldPrivateCompletion _ (WorldCompletedDependencies old selected known))
      | old == policy -> pure (selected,known)
      | otherwise -> throwIO WorldCellCorrupt
  pure (WorldPrivateCompletion completion (WorldCompletedDependencies policy
    (mergeWorldModuleVersions additions retained) (mergeWorldCanonicalRecovery recovery oldRecovery)))

-- Only actual demanded canonical versions retain body acceleration. The
-- pending owner is still private; END commits these additions with source rows.
sealWorldCanonicalRecovery :: PendingWorldDependencies -> [OriginalVersion] -> IO ()
sealWorldCanonicalRecovery (PendingWorldDependencies _ _ _ _ _ recovery pending) versions = do
  selected <- filterM (preparedBodyHasOriginalVersion (compilerPreparedBodies recovery)) versions
  timing <- readTimingEnabled
  emitCount timing "world.cell.canonical_recovery_roots" (fromIntegral (length selected))
  identity <- RequestUnique.newUnique
  let additions = WorldCanonicalRecovery (Map.fromList [(version,RecoveryContext identity recovery) | version <- selected])
  actual <- readIORef pending >>= maybe (throwIO CheckedPurposeMismatch) pure
  let WorldDependencyAdditions sources known = actual
  writeIORef pending (Just (WorldDependencyAdditions sources (mergeWorldCanonicalRecovery additions known)))

inheritWorldCanonicalRecovery :: WorldCanonicalRecovery -> CompilerRecoveryCaches -> OriginalVersion -> IO ()
inheritWorldCanonicalRecovery (WorldCanonicalRecovery roots) current version =
  forM_ (Map.lookup version roots) $ \source -> inheritPreparedBodyOwner
    (compilerPreparedBodies current) (compilerPreparedBodies (recoveryContextCaches source)) (originalVersionOwner version)

cycleCanonicalRecovery :: CycleState -> WorldCanonicalRecovery
cycleCanonicalRecovery StandaloneCycle = emptyWorldCanonicalRecovery
cycleCanonicalRecovery (TransactionCycle _ _ _ _ roots _ _ _ _) = roots

mergeWorldModuleVersions :: WorldModuleVersions -> WorldModuleVersions -> WorldModuleVersions
mergeWorldModuleVersions = Map.unionWith (Map.unionWith (Map.unionWith preferModuleVersion))

commitHeldWorldDependencies :: PendingWorldDependencies -> HeldCompilerWorld -> IO ()
commitHeldWorldDependencies (PendingWorldDependencies _ identity reference policy _ _ pending)
    (HeldCompilerWorld facts issued _ preparedRef) = do
  unless (identity == worldInputsIdentity facts && reference == issued) (throwIO WorldCellCorrupt)
  WorldPreparedModules table receipts previous demandScope <- readIORef preparedRef
  (versions,oldRecovery) <- case previous of
    Nothing -> pure (Map.empty,emptyWorldCanonicalRecovery)
    Just (WorldCompletedDependencies selected retained recovery)
      | selected == policy -> pure (retained,recovery)
      | otherwise -> throwIO WorldCellCorrupt
  -- Preflight has verified the exact scope, allowance, held owner and policy.
  -- Consume before changing retained state; an uncertain mutation cannot replay.
  WorldDependencyAdditions additions recovery <- atomicModifyIORef' pending (\value -> (Nothing,value))
    >>= maybe (throwIO CheckedPurposeMismatch) pure
  let completed = WorldCompletedDependencies policy
        (mergeWorldModuleVersions additions versions) (mergeWorldCanonicalRecovery recovery oldRecovery)
  let WorldCompletedDependencies _ retained _ = completed
  _ <- evaluate (Map.size retained)
  writeIORef preparedRef (WorldPreparedModules table receipts (Just completed) demandScope)

-- The same world owner accounts optional residency separately from all live
-- physical backing, including active-only and quarantined extents.
data WorldBackingState
  = WorldBackingLive !Int | WorldBackingRemoving | WorldBackingDeleted
  | WorldBackingQuarantined
newtype WorldBackingLease = WorldBackingLease (IORef WorldBackingState)
data WorldBackingAccounting = WorldBackingAccounting !Int !Int !Integer !Int
-- live realized owners, optional indexed owners, physical bytes and nodes

-- Each logical World reserves its encoded input basis conservatively. Equal
-- payloads are not proof of physical sharing; only the existing backing lease
-- account below de-duplicates storage across authenticated successors.
data CachedCompilerWorld = CachedCompilerWorld !Integer !Integer !Int HeldCompilerWorld
data CompilerWorldCache = CompilerWorldCache
  { cachedCompilerWorlds :: Map.Map String CachedCompilerWorld
  , cachedCompilerWorldOrder :: Map.Map Integer String
  , nextCompilerWorldTicket :: !Integer
  , compilerWorldRetainedBytes :: !Integer
  , compilerWorldRetainedNodes :: !Int
  , compilerWorldBackingAccounts :: Map.Map FilePath WorldBackingAccounting
  , compilerWorldLiveBackingBytes :: !Integer
  , compilerWorldLiveBackingNodes :: !Int
  }

emptyCompilerWorldCache :: CompilerWorldCache
emptyCompilerWorldCache = CompilerWorldCache Map.empty Map.empty 0 0 0 Map.empty 0 0

lookupCompilerWorld :: String -> CompilerWorldCache -> Maybe HeldCompilerWorld
lookupCompilerWorld root cache =
  (\(CachedCompilerWorld _ _ _ world) -> world) <$> Map.lookup root (cachedCompilerWorlds cache)

compilerWorldCacheCharge :: CompilerWorldCache -> (Integer,Int)
compilerWorldCacheCharge cache = (compilerWorldRetainedBytes cache,compilerWorldRetainedNodes cache)

compilerWorldLiveCharge :: CompilerWorldCache -> (Integer,Int)
compilerWorldLiveCharge cache = (compilerWorldLiveBackingBytes cache,compilerWorldLiveBackingNodes cache)

compilerWorldCharge :: HeldCompilerWorld -> (Integer,Int)
compilerWorldCharge (HeldCompilerWorld facts _ materialized _) =
  let (copies,nodes) = worldMaterializationCharge materialized
      (raw,entries) = worldInputsRetainedCharge facts
  in (raw + copies,entries + nodes + 1)

-- This is an optional input/backing storage policy, not a GHC heap bound or a
-- lease. A retained realization can subsequently be evicted or lose its worker.
worldAdmissionResidency :: Integer -> HeldCompilerWorld -> WorldResidency
worldAdmissionResidency limit world
  | required > limit = WorldActiveOnly (WorldStorageAllowanceExceeded required limit)
  | otherwise = WorldRetainedAtAdmission
  where required = fst (compilerWorldCharge world)

worldMaterializationCharge :: WorldMaterialization -> (Integer,Int)
worldMaterializationCharge (WorldMaterialization _ _ _ _ _ _ copies nodes _) = (copies,nodes)

worldMaterializationKey :: WorldMaterialization -> FilePath
worldMaterializationKey (WorldMaterialization directory _ _ _ _ _ _ _ _) = directory

registerWorldBacking :: WorldMaterialization -> CompilerWorldCache -> CompilerWorldCache
registerWorldBacking materialized cache = case Map.lookup key (compilerWorldBackingAccounts cache) of
  Nothing -> cache
    {compilerWorldBackingAccounts=Map.insert key (WorldBackingAccounting 1 0 bytes nodes) (compilerWorldBackingAccounts cache)
    ,compilerWorldLiveBackingBytes=compilerWorldLiveBackingBytes cache+bytes
    ,compilerWorldLiveBackingNodes=compilerWorldLiveBackingNodes cache+nodes}
  Just (WorldBackingAccounting live resident charge entries)
    | (charge,entries) == (bytes,nodes) -> cache
        {compilerWorldBackingAccounts=Map.insert key (WorldBackingAccounting (live+1) resident bytes nodes)
          (compilerWorldBackingAccounts cache)}
    | otherwise -> throw WorldQueryCorrupt
  where
    key = worldMaterializationKey materialized
    (bytes,nodes) = worldMaterializationCharge materialized

-- Called only after one actual lease hold has been released successfully. An
-- index removal never calls this and cannot report physical backing released.
releaseWorldBackingAccounting :: WorldMaterialization -> Bool -> CompilerWorldCache -> CompilerWorldCache
releaseWorldBackingAccounting materialized removed cache =
  case Map.lookup key (compilerWorldBackingAccounts cache) of
    Just (WorldBackingAccounting live resident bytes nodes)
      | live > 1 && not removed -> cache
          {compilerWorldBackingAccounts=Map.insert key (WorldBackingAccounting (live-1) resident bytes nodes)
            (compilerWorldBackingAccounts cache)}
      | live == 1 && resident == 0 && removed -> cache
          {compilerWorldBackingAccounts=Map.delete key (compilerWorldBackingAccounts cache)
          ,compilerWorldLiveBackingBytes=compilerWorldLiveBackingBytes cache-bytes
          ,compilerWorldLiveBackingNodes=compilerWorldLiveBackingNodes cache-nodes}
    _ -> throw WorldQueryCorrupt
  where key = worldMaterializationKey materialized

removeCompilerWorld :: String -> CompilerWorldCache -> CompilerWorldCache
removeCompilerWorld root cache = case Map.lookup root (cachedCompilerWorlds cache) of
  Nothing -> cache
  Just (CachedCompilerWorld ticket charge entries (HeldCompilerWorld _ _ materialized _)) ->
    case Map.lookup key (compilerWorldBackingAccounts cache) of
      Just (WorldBackingAccounting live resident bytes nodes) | resident > 0 -> cache
        {cachedCompilerWorlds=Map.delete root (cachedCompilerWorlds cache)
        ,cachedCompilerWorldOrder=Map.delete ticket (cachedCompilerWorldOrder cache)
        ,compilerWorldRetainedBytes=compilerWorldRetainedBytes cache-charge-(if resident == 1 then bytes else 0)
        ,compilerWorldRetainedNodes=compilerWorldRetainedNodes cache-entries-(if resident == 1 then nodes else 0)
        ,compilerWorldBackingAccounts=Map.insert key (WorldBackingAccounting live (resident-1) bytes nodes)
          (compilerWorldBackingAccounts cache)}
      _ -> throw WorldQueryCorrupt
    where key = worldMaterializationKey materialized

admitCompilerWorld :: Integer -> String -> HeldCompilerWorld -> CompilerWorldCache
  -> (CompilerWorldCache,[HeldCompilerWorld])
admitCompilerWorld byteLimit root world@(HeldCompilerWorld facts _ materialized _) cache
  | WorldActiveOnly _ <- worldAdmissionResidency byteLimit world = (cache,[])
  | Map.member root (cachedCompilerWorlds cache) = throw WorldQueryCorrupt
  | otherwise = case Map.lookup key (compilerWorldBackingAccounts cache) of
      Just (WorldBackingAccounting live resident bytes nodes) -> trim [] cache
        {cachedCompilerWorlds=Map.insert root (CachedCompilerWorld ticket charge entries world) (cachedCompilerWorlds cache)
        ,cachedCompilerWorldOrder=Map.insert ticket root (cachedCompilerWorldOrder cache)
        ,nextCompilerWorldTicket=ticket+1
        ,compilerWorldRetainedBytes=compilerWorldRetainedBytes cache+charge+backingBytes
        ,compilerWorldRetainedNodes=compilerWorldRetainedNodes cache+entries+backingNodes
        ,compilerWorldBackingAccounts=Map.insert key (WorldBackingAccounting live (resident+1) bytes nodes)
          (compilerWorldBackingAccounts cache)}
      Nothing -> throw WorldQueryCorrupt
  where
    ticket = nextCompilerWorldTicket cache
    key = worldMaterializationKey materialized
    (raw,rawEntries) = worldInputsRetainedCharge facts
    (charge,entries) = (raw,rawEntries+1)
    (backingBytes,backingNodes) = case Map.lookup key (compilerWorldBackingAccounts cache) of
      Just (WorldBackingAccounting _ resident bytes nodes) | resident == 0 -> (bytes,nodes)
      Just _ -> (0,0)
      Nothing -> throw WorldQueryCorrupt
    trim evicted selected
      | compilerWorldRetainedBytes selected <= byteLimit = (selected,reverse evicted)
      | otherwise = case Map.lookupMin (cachedCompilerWorldOrder selected) of
          Just (_,oldest) -> case Map.lookup oldest (cachedCompilerWorlds selected) of
            Just (CachedCompilerWorld _ _ _ retired) -> trim (retired:evicted) (removeCompilerWorld oldest selected)
            Nothing -> throw WorldQueryCorrupt
          Nothing -> throw WorldQueryCorrupt

retainWorldMaterialization :: WorldMaterialization -> IO ()
retainWorldMaterialization (WorldMaterialization _ _ _ _ _ _ _ _ (WorldBackingLease lease)) = mask $ \_ -> do
  state <- readIORef lease
  case state of
    WorldBackingLive count | count > 0 -> writeIORef lease (WorldBackingLive (count+1))
    _ -> throwIO WorldCellCorrupt

worldMaterializationOwners :: WorldMaterialization -> IO (Maybe Int)
worldMaterializationOwners (WorldMaterialization _ _ _ _ _ _ _ _ (WorldBackingLease lease)) = do
  state <- readIORef lease
  pure $ case state of
    WorldBackingLive count -> Just count
    WorldBackingDeleted -> Just 0
    _ -> Nothing

releaseWorldMaterialization :: WorldMaterialization -> IO Bool
releaseWorldMaterialization (WorldMaterialization directory _ _ _ _ _ _ _ (WorldBackingLease lease)) = mask $ \restore -> do
  state <- readIORef lease
  case state of
    WorldBackingLive count | count > 1 -> writeIORef lease (WorldBackingLive (count-1)) >> pure False
    WorldBackingLive 1 -> do
      writeIORef lease WorldBackingRemoving
      completed <- captureCompletion (restore (removeDirectoryRecursive directory))
      case completionResult completed of
        BodySucceeded () -> writeIORef lease WorldBackingDeleted >> pure True
        _ -> do
          writeIORef lease WorldBackingQuarantined
          finishCompletion completed
          throwIO WorldCellCorrupt
    _ -> throwIO WorldCellCorrupt

-- The caller is the service's authenticated successor issuer. Equal source
-- seals can validate that exact base's inherited backing, never select a base.
retainHeldWorldSuccessor :: IORef CompilerUniverse -> HeldCompilerWorld -> WorldSuccessorInputs -> IssuedWorldRealization
  -> (HeldCompilerWorld -> IO result) -> IO result
retainHeldWorldSuccessor universeRef (HeldCompilerWorld base _ materialized preparedRef) successorInputs reference publish = mask $ \_ -> do
  let facts = worldSuccessorFacts successorInputs
  unless (worldInputsProducer base == worldInputsProducer facts
      && worldInputsProfile base == worldInputsProfile facts
      && worldSourceDomainSha256 (worldInputsSources base) == worldSourceDomainSha256 (worldInputsSources facts))
    (throwIO WorldCellCorrupt)
  WorldPreparedModules table receipts dependencies demandScope <- readIORef preparedRef
  universe <- readIORef universeRef
  updatedScope <- traverse (updateSegmentDemandScope (universeEnvironment universe)
    [(alias,WorldBaseBinding <$> Map.lookup alias (worldInputsBindings facts)) | alias <- Set.toAscList (worldSuccessorChangedAliases successorInputs)]) demandScope
  prepared <- newIORef (WorldPreparedModules table receipts dependencies updatedScope)
  unless (Map.member (worldMaterializationKey materialized) (compilerWorldBackingAccounts (universeWorlds universe)))
    (throwIO WorldCellCorrupt)
  let WorldMaterialization directory products roots names paths baseHints bytes nodes lease = materialized
      hints = if worldInputsSamePrimary base facts then baseHints else worldInputsSourceModuleHints facts
      successorMaterialization = WorldMaterialization directory products roots names paths hints bytes nodes lease
      successor = HeldCompilerWorld facts reference successorMaterialization prepared
  charged <- evaluate (registerWorldBacking materialized (universeWorlds universe))
  next <- evaluate (universe {universeWorlds=charged})
  _ <- evaluate successor
  retainWorldMaterialization materialized
  writeIORef universeRef next
  completed <- captureCompletion (publish successor)
  case completionResult completed of
    BodySucceeded _ -> finishCompletion completed
    _ -> do
      failures <- attemptReleases [(CompilerScopeRestore,
        retireCompilerWorldBacking False universeRef successor)]
      finishCompletion (appendCleanupFailures completed failures)

-- Scope/session restoration precedes this release. A successful index
-- retirement is distinct from releasing the last physical backing owner.
releaseRegisteredWorldMaterialization :: IORef CompilerUniverse -> WorldMaterialization -> IO Bool
releaseRegisteredWorldMaterialization universeRef materialized = mask $ \_ -> do
  removed <- releaseWorldMaterialization materialized
  universe <- readIORef universeRef
  let cache = universeWorlds universe
      next = if Map.member (worldMaterializationKey materialized) (compilerWorldBackingAccounts cache)
        then releaseWorldBackingAccounting materialized removed cache else cache
  _ <- evaluate next
  writeIORef universeRef universe {universeWorlds=next}
  pure removed

retireCompilerWorldBacking :: Bool -> IORef CompilerUniverse -> HeldCompilerWorld -> IO ()
retireCompilerWorldBacking timing universeRef world@(HeldCompilerWorld facts reference materialized prepared) = do
  completed <- captureCompletion $ do
    writeIORef prepared emptyWorldPreparedModules
    releaseRegisteredWorldMaterialization universeRef materialized
  universe <- readIORef universeRef
  observed <- attemptReleases $ case completionResult completed of
    BodySucceeded False -> [(CompilerScopeRestore,do
      emitCount timing "worlds.backing_owner_releases" 1
      owners <- worldMaterializationOwners materialized
      forM_ owners (emitCount timing "worlds.backing_realization_owners" . fromIntegral))]
    result -> [(CompilerScopeRestore,
      observeCompilerWorld timing universe
        (case result of BodySucceeded True -> WorldBackingRemoved; _ -> WorldBackingRemoveFailed)
        (worldInputsIdentity facts) (worldRealizationText reference) (Just world))]
  void (finishCompletion (appendCleanupFailures completed observed))

observeCompilerWorld :: Bool -> CompilerUniverse -> WorldRetentionEvent -> String -> String
  -> Maybe HeldCompilerWorld -> IO ()
observeCompilerWorld timing universe event root reference world =
  forM_ (universePackageAssociation universe) $ \association -> do
    let limits = exactInputRetentionLimits (universeOriginalInputs universe)
        backing = (\(HeldCompilerWorld _ _ materialized _) -> worldMaterializationCharge materialized) <$> world
        directory = (\(HeldCompilerWorld _ _ (WorldMaterialization path _ _ _ _ _ _ _ _) _) -> path) <$> world
    emitWorldRetention timing event root (hexBytes (servicePackageWorkerInstance association)) reference
      (compilerWorldCharge <$> world) backing (compilerWorldCacheCharge (universeWorlds universe))
      (optionalInputByteLimit limits,originalImageCountLimit limits) directory
    let (liveBytes,liveNodes) = compilerWorldLiveCharge (universeWorlds universe)
    emitCount timing "worlds.live_backing_bytes" liveBytes
    emitCount timing "worlds.live_backing_nodes" (fromIntegral liveNodes)
    emitCount timing "worlds.retained_worlds" (fromIntegral (Map.size (cachedCompilerWorlds (universeWorlds universe))))
    forM_ world $ \(HeldCompilerWorld _ _ materialized _) -> do
      owners <- worldMaterializationOwners materialized
      forM_ owners (emitCount timing "worlds.backing_realization_owners" . fromIntegral)
    forM_ world $ \(HeldCompilerWorld _ _ _ preparedRef) -> do
      WorldPreparedModules _ receipts completed _ <- readIORef preparedRef
      emitCount timing "worlds.prepared_home_receipts" (fromIntegral (Map.size receipts))
      emitCount timing "worlds.completed_source_modules" $ case completed of
        Nothing -> 0
        Just (WorldCompletedDependencies _ versions _) -> fromIntegral (Map.size versions)

data WorldQueryFailure = WorldQueryUnsupported | WorldQueryUnavailable | WorldQueryCorrupt deriving Show
instance Exception WorldQueryFailure

-- The final callback is masked: its sole production consumer prepares all
-- publication metadata, then transfers this directory to the world/hold owner
-- with nonblocking writes. A failed unissued extent preserves both its primary
-- exception (including cancellation) and any cleanup failure. Failed removal
-- remains beneath the existing worker namespace for confirmed-reap cleanup.
materializeWorldSources :: FilePath -> CapturedWorldSources -> Set.Set String
  -> (WorldMaterialization -> IO result) -> IO result
materializeWorldSources = materializeWorldSourcesWithRelease (void . releaseWorldMaterialization)

materializeWorldSourcesWithRelease :: (WorldMaterialization -> IO ()) -> FilePath -> CapturedWorldSources -> Set.Set String
  -> (WorldMaterialization -> IO result) -> IO result
materializeWorldSourcesWithRelease release parent sources hints publish = mask $ \restore -> do
  directory <- mkdtemp (parent </> "world.XXXXXX")
  built <- newIORef Nothing
  completed <- captureCompletion $ do
    materialized <- restore (build directory)
    writeIORef built (Just materialized)
    publish materialized
  case completionResult completed of
    BodySucceeded _ -> finishCompletion completed
    _ -> do
      materialized <- readIORef built
      failures <- attemptReleases [(CompilerScopeRestore,
        maybe (removeDirectoryRecursive directory) release materialized)]
      finishCompletion (appendCleanupFailures completed failures)
  where
    build directory = do
      let products = directory </> "products"
      createDirectory products
      roots <- forM (zip [0..] (worldSourceRoots sources)) $ \(ordinal,root) -> do
        let physicalRoot = directory </> ("root-" ++ show (ordinal :: Int))
        createDirectory physicalRoot
        entries <- forM (Map.toAscList (worldSourceFiles root)) $ \(relative,bytes) -> do
          let physical = physicalRoot </> relative
          createDirectoryIfMissing True (takeDirectory physical)
          BS.writeFile physical bytes
          sha <- evaluate (SHA256.hash bytes)
          pure (relative,WorldHomeSource ordinal relative physical sha)
        pure (physicalRoot,entries)
      let sourceNames entries = Map.fromList
            [(mkModuleName (map (\c -> if c == pathSeparator then '.' else c) (dropExtension relative)),source)
            | (relative,source) <- reverse entries, takeExtension relative `elem` [".hs",".lhs"]]
          names = foldr (Map.union . sourceNames . snd) Map.empty roots
          paths = Map.fromList [(physical,source) | (_,entries) <- roots
            , (_,source@(WorldHomeSource _ _ physical _)) <- entries]
      -- Complete cold indexes before publication, never enumerate passive
      -- custody on a resident query or select an interface from these hints.
      let copiedBytes = sum [toInteger (BS.length body)
            | root <- worldSourceRoots sources, body <- Map.elems (worldSourceFiles root)]
          sourceDirectories relative = case takeDirectory relative of
            "." -> []
            parent -> parent : sourceDirectories parent
          sourceNodes entries = length entries + 1 + Set.size (Set.fromList
            [parent | (relative,_) <- entries, parent <- sourceDirectories relative])
          nodes = 2 + sum [sourceNodes entries | (_,entries) <- roots]
      _ <- evaluate (Set.size hints + Map.size names + Map.size paths + length roots + nodes)
      _ <- evaluate copiedBytes
      lease <- WorldBackingLease <$> newIORef (WorldBackingLive 1)
      evaluate (WorldMaterialization directory products (map fst roots) names paths hints copiedBytes nodes lease)

-- This path starts from captured source selection, never the universe's most
-- recent HPT or a raw-custody module inventory. The universe supplies only its
-- fixed package/NameCache/interpreter owners to the private operation.
runWorldPublicModule :: Maybe CompilerProducerIdentity -> CompilerUniverse -> HscEnv -> Bool
  -> HeldCompilerWorld -> CompilerExecutionGrant -> CompilerExecutor -> WorldPublicQuestion -> Ghc BS.ByteString
runWorldPublicModule producer universe baseEnv timing
    (HeldCompilerWorld facts _ (WorldMaterialization _ products roots sources paths sourceHints _ _ _) preparedRef) grant executor question =
  handleWorldQueryFailure facts question $ do
    requested <- case structuredScope (publicQuestionInspection question) of
      StructuredPublicModule name | not (isReservedSessionModuleName name) -> pure name
      _ -> liftIO (throwIO WorldQueryUnsupported)
    prepared@(WorldPreparedModules table receipts dependencies demandScope) <- liftIO (readIORef preparedRef)
    borrowed <- liftIO (forkExactContextWithPackageFacts (universePackageFinder universe) (universeEnvironment universe))
    let flags = (hsc_dflags baseEnv) {importPaths=[],workingDirectory=Nothing}
        fresh = (hscUpdateHPT (const table) (hscUpdateFlags (const flags) borrowed))
          {hsc_hooks=hsc_hooks baseEnv,hsc_logger=hsc_logger baseEnv,hsc_targets=[]}
    setSession fresh
    resolved <- case Map.lookup (mkModuleName requested) sources of
      Just source@(WorldHomeSource _ _ physical _) ->
        case lookupHpt table (mkModuleName requested) of
          Just home -> do
            let owner = mi_module (hm_iface home)
            case Map.lookup owner receipts of
              Just selected | sameWorldSource source selected -> pure (Just (owner,prepared))
              _ -> liftIO (throwIO WorldQueryCorrupt)
          Nothing -> do
            -- A cold demand checks the real module and its transitive source
            -- closure. Historical memo/candidate/home alternatives are absent.
            compileEnv <- liftIO (forkExactContextWithPackageFacts (universePackageFinder universe) (universeEnvironment universe))
            -- This metadata route retains the actual in-memory HMI. No lazy
            -- realization may depend on a generated home interface file.
            let compileFlags = foldl' gopt_unset ((hsc_dflags baseEnv)
                  {importPaths=roots,workingDirectory=Nothing,hiDir=Just products,objectDir=Just products})
                  [Opt_WriteInterface,Opt_WriteIfSimplifiedCore]
            setSession ((hscUpdateHPT (const emptyHomePackageTable)
              (hscUpdateFlags (const compileFlags) compileEnv))
                {hsc_hooks=hsc_hooks baseEnv,hsc_logger=hsc_logger baseEnv,hsc_targets=[]})
            variant <- liftIO (normalVariant GeneralCompile)
            cache <- liftIO newIfaceCache
            memo <- liftIO (newIORef Map.empty)
            interpreterAttempt <- liftIO (newIORef InterpreterConfirmed)
            identity <- liftIO newTimingRequestIdentity
            start <- monotonicTime
            resources <- beginResourceTiming timing
            let cycle = TransactionCycle cache memo Nothing Map.empty emptyWorldCanonicalRecovery
                  (const (selectCompilerRecoveryCaches baseEnv (universeRecovery universe) []))
                  (universeInterpreter universe) (universePackageFinder universe) interpreterAttempt
            CheckedPublicModuleResult target compiled evidence _ <- runCompileCycle
              (WithCompilerExecution grant executor CheckedPublicModule) cycle (retainedContext Set.empty)
              Nothing timing identity start resources variant {pvCompilerProducer=producer} physical
            selected <- liftIO (validateWorldHomeEvidence paths compiled evidence)
            let owner = mi_module (hm_iface target)
            unless (moduleNameString (moduleName owner) == requested
                && maybe False (sameWorldSource source) (Map.lookup owner selected))
              (liftIO (throwIO WorldQueryCorrupt))
            let combined = foldl' (\homes home -> addToHpt homes (moduleName (mi_module (hm_iface home))) home)
                  table (eltsHpt compiled)
                completed = WorldPreparedModules combined (Map.union selected receipts) dependencies demandScope
            pure (Just (owner,completed))
      Nothing -> do
        -- A known source name without a captured finder-representable path
        -- cannot become a package answer or an asserted closed-domain miss.
        when (Set.member requested sourceHints) (liftIO (throwIO WorldQueryUnavailable))
        env <- getSession
        found <- liftIO (findImportedModule env (mkModuleName requested) NoPkgQual)
        case found of
          Found _ owner | not (isHomeUnit (hsc_home_unit env) (moduleUnit owner)) -> pure (Just (owner,prepared))
          NotFound {fr_pkg=Nothing,fr_mods_hidden=[],fr_pkgs_hidden=[],fr_unusables=[]} -> pure Nothing
          _ -> liftIO (throwIO WorldQueryUnavailable)
    case resolved of
      Nothing -> pure (encodeWorldQueryResult facts question WorldAbsent Nothing
        (encodeInspectionResults [InspectionStructuredError (StructuredUnknownModule requested)]))
      Just (owner,completed@(WorldPreparedModules selected selectedReceipts _ _)) -> do
        -- Restore a fresh mutable context around exactly this realization's
        -- selected immutable HPT. No expression/checking target is fabricated.
        current <- getSession
        setSession ((hscUpdateHPT (const selected) (hscUpdateFlags (const flags) current))
          {hsc_targets=[],hsc_mod_graph=hsc_mod_graph baseEnv})
        information <- getModuleInfo owner >>= maybe (liftIO (throwIO WorldQueryUnavailable)) pure
        (answer,definition) <- inspectPublicModuleExports (publicQuestionIsType question)
          (publicQuestionInspection question) (modInfoExports information)
        env <- getSession
        queried <- liftIO (worldModuleReceipt env selectedReceipts owner)
        defining <- liftIO (traverse (worldModuleReceipt env selectedReceipts) definition)
        let bytes = encodeWorldQueryResult facts question (WorldResolved queried) defining (encodeInspectionResults [answer])
        -- Publication follows all source/provenance checks and actual rendering.
        -- A failed attempt leaves the prior same-world realization intact.
        _ <- liftIO (evaluate (BS.length bytes))
        liftIO (writeIORef preparedRef completed)
        pure bytes

sameWorldSource :: WorldHomeSource -> WorldHomeSource -> Bool
sameWorldSource (WorldHomeSource root path _ sha) (WorldHomeSource root' path' _ sha') =
  root == root' && path == path' && sha == sha'

validateWorldHomeEvidence :: Map.Map FilePath WorldHomeSource -> HomePackageTable -> DependencyEvidence
  -> IO (Map.Map Module WorldHomeSource)
validateWorldHomeEvidence paths table evidence = do
  unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence)
    (throwIO WorldQueryUnsupported)
  forM_ (dependencySources evidence) $ \source -> case Map.lookup (normalise (dependencySourcePath source)) paths of
    Just (WorldHomeSource _ _ _ sha) | hexBytes sha == dependencySourceSha256 source -> pure ()
    _ -> throwIO WorldQueryCorrupt
  pairs <- forM (eltsHpt table) $ \home -> do
    let owner = mi_module (hm_iface home)
    case [source | node <- dependencyModules evidence, not (dependencyModuleBoot node)
      , dependencyModuleUnit node == unitString (moduleUnit owner)
      , dependencyModuleName node == moduleNameString (moduleName owner)
      , Just source <- [Map.lookup (normalise (dependencyModuleSource node)) paths]] of
      [source] -> pure (owner,source)
      _ -> throwIO WorldQueryCorrupt
  pure (Map.fromList pairs)

data WorldModuleReceipt = WorldModuleReceipt Module (Maybe WorldHomeSource)
data WorldResolution = WorldResolved WorldModuleReceipt | WorldAbsent | WorldRejected WorldQueryFailure

worldModuleReceipt :: HscEnv -> Map.Map Module WorldHomeSource -> Module -> IO WorldModuleReceipt
worldModuleReceipt env selected owner
  | isReservedSessionModuleName (moduleNameString (moduleName owner)) = throwIO WorldQueryUnsupported
  | Just source <- Map.lookup owner selected = pure (WorldModuleReceipt owner (Just source))
  | isHomeUnit (hsc_home_unit env) (moduleUnit owner) = throwIO WorldQueryCorrupt
  | otherwise = pure (WorldModuleReceipt owner Nothing)

handleWorldQueryFailure :: WorldInputFacts -> WorldPublicQuestion -> Ghc BS.ByteString -> Ghc BS.ByteString
handleWorldQueryFailure facts question action = reifyGhc $ \session ->
  let reject cause = pure (encodeWorldQueryResult facts question (WorldRejected cause) Nothing BS.empty)
  in reflectGhc action session
    `catch` (\(failure :: WorldQueryFailure) -> reject failure)
    `catch` (\(_failure :: WorldModulePolicyFailure) -> reject WorldQueryUnsupported)
    `catch` (\(_failure :: IOException) -> reject WorldQueryUnavailable)

encodeWorldQueryResult :: WorldInputFacts -> WorldPublicQuestion -> WorldResolution
  -> Maybe WorldModuleReceipt -> BS.ByteString -> BS.ByteString
encodeWorldQueryResult facts question resolution defining payload =
  let body = toStrictByteString (WorldEncoding.encodeListLen 8 <> WorldEncoding.encodeWord 1
        <> WorldEncoding.encodeBytes (worldInputsIdentityBytes facts)
        <> WorldEncoding.encodeWord (if not (publicQuestionIsType question) then 0 else 1)
        <> word requested <> encodeWorldQuestion (publicQuestionInspection question) <> encodeResolution resolution
        <> maybe WorldEncoding.encodeNull encodeReceipt defining <> WorldEncoding.encodeBytes payload)
  in frameWorldQuery body
  where
    requested = case structuredScope (publicQuestionInspection question) of StructuredPublicModule name -> name; _ -> ""
    word = WorldEncoding.encodeString . Text.pack
    encodeResolution (WorldResolved receipt) = encodeReceiptWith "resolved" receipt
    encodeResolution WorldAbsent = WorldEncoding.encodeListLen 4 <> word "absent" <> word requested
      <> WorldEncoding.encodeBytes (worldSourceDomainSha256 (worldInputsSources facts))
      <> WorldEncoding.encodeBytes (worldInputsProfile facts)
    encodeResolution (WorldRejected cause) = WorldEncoding.encodeListLen 2 <> word "rejected"
      <> WorldEncoding.encodeWord (case cause of WorldQueryUnsupported -> 0; WorldQueryUnavailable -> 1; WorldQueryCorrupt -> 2)
    encodeReceipt = encodeReceiptWith "resolved"
    encodeReceiptWith tag (WorldModuleReceipt owner source) = WorldEncoding.encodeListLen 4 <> word tag
      <> word (unitString (moduleUnit owner)) <> word (moduleNameString (moduleName owner)) <> origin source
    origin Nothing = WorldEncoding.encodeListLen 2 <> word "package" <> WorldEncoding.encodeBytes (worldInputsProfile facts)
    origin (Just (WorldHomeSource ordinal relative _ sha)) = WorldEncoding.encodeListLen 4 <> word "home"
      <> WorldEncoding.encodeInt ordinal <> word relative <> WorldEncoding.encodeBytes sha

-- A missing scope hold is a typed refusal; it does not fabricate world facts
-- or turn failure to acquire a world into a closed module absence.
encodeUnavailableWorldQuery :: WorldQueryRequest -> BS.ByteString
encodeUnavailableWorldQuery request = case worldQueryQuestion request of
  WorldPublicModuleQuery question -> frameWorldQuery $ toStrictByteString $
    WorldEncoding.encodeListLen 8 <> WorldEncoding.encodeWord 1
      <> WorldEncoding.encodeBytes root
      <> WorldEncoding.encodeWord (if publicQuestionIsType question then 1 else 0)
      <> WorldEncoding.encodeString (Text.pack requested)
      <> encodeWorldQuestion (publicQuestionInspection question)
      <> WorldEncoding.encodeListLen 2 <> WorldEncoding.encodeString "rejected" <> WorldEncoding.encodeWord 1
      <> WorldEncoding.encodeNull <> WorldEncoding.encodeBytes BS.empty
    where requested = case structuredScope (publicQuestionInspection question) of
            StructuredPublicModule name -> name
            StructuredCurrentScope -> ""
  WorldScopedQuery query -> encodeWorldObservationResult root query (Left WorldQueryUnavailable)
  where root = worldIdentityBytes (worldQueryIdentity request)

worldIdentityBytes :: String -> BS.ByteString
worldIdentityBytes = BS.pack . pairs
  where
    pairs [] = []
    pairs (a:b:rest) = fromIntegral (16 * digitToInt a + digitToInt b) : pairs rest
    pairs _ = []

observationProvenance :: WorldObservationQuery -> InspectionProvenance
observationProvenance query = case query of
  WorldObservationInfo question -> structuredProvenance question
  WorldObservationType question -> structuredProvenance question
  WorldObservationRowFit question _ -> structuredProvenance question
  WorldObservationExpression provenance _ -> provenance
  WorldObservationTypeSearch provenance _ _ -> provenance

encodeWorldObservationResult :: BS.ByteString -> WorldObservationQuery -> Either WorldQueryFailure BS.ByteString -> BS.ByteString
encodeWorldObservationResult root query outcome = frameWorldQuery $ toStrictByteString $
  WorldEncoding.encodeListLen 5 <> WorldEncoding.encodeWord 2 <> WorldEncoding.encodeBytes root
    <> WorldEncoding.encodeListLen 2 <> WorldEncoding.encodeWord64 (inspectionGeneration provenance)
    <> word (inspectionFingerprint provenance) <> echo query <> result outcome
  where
    provenance = observationProvenance query
    word = WorldEncoding.encodeString . Text.pack
    name question = WorldEncoding.encodeWord (case structuredNamespace question of
      StructuredAnyName -> 0; StructuredValueName -> 1; StructuredTypeName -> 2
      StructuredConstructorName -> 3; StructuredFieldName -> 4) <> word (structuredName question)
    echo question = case question of
      WorldObservationInfo operand -> WorldEncoding.encodeListLen 3 <> WorldEncoding.encodeWord 2 <> name operand
      WorldObservationType operand -> WorldEncoding.encodeListLen 3 <> WorldEncoding.encodeWord 3 <> name operand
      WorldObservationRowFit operand owner -> WorldEncoding.encodeListLen 4 <> WorldEncoding.encodeWord 4 <> name operand <> word owner
      WorldObservationExpression _ expression -> WorldEncoding.encodeListLen 2 <> WorldEncoding.encodeWord 5 <> word expression
      WorldObservationTypeSearch _ expression limit -> WorldEncoding.encodeListLen 3 <> WorldEncoding.encodeWord 6 <> word expression <> WorldEncoding.encodeInt limit
    result (Right payload) = WorldEncoding.encodeListLen 2 <> word "answer" <> WorldEncoding.encodeBytes payload
    result (Left failure) = WorldEncoding.encodeListLen 2 <> word "rejected" <> WorldEncoding.encodeWord
      (case failure of WorldQueryUnsupported -> 0; WorldQueryUnavailable -> 1; WorldQueryCorrupt -> 2)

handleWorldObservationFailure :: WorldObservationQuery -> BS.ByteString -> Ghc BS.ByteString -> Ghc BS.ByteString
handleWorldObservationFailure query root action = reifyGhc $ \session ->
  let reject cause = pure (encodeWorldObservationResult root query (Left cause))
  in (encodeWorldObservationResult root query . Right <$> reflectGhc action session)
    `catch` (\(failure :: WorldQueryFailure) -> reject failure)
    `catch` (\(failure :: WorldCellRefusal) -> reject (case failure of
      WorldCellUnavailable -> WorldQueryUnavailable
      WorldCellCorrupt -> WorldQueryCorrupt
      _ -> WorldQueryUnsupported))
    `catch` (\(_failure :: WorldModulePolicyFailure) -> reject WorldQueryUnsupported)
    `catch` (\(_failure :: IOException) -> reject WorldQueryUnavailable)

-- Correlation belongs to the exact request, including refused or absent
-- queries whose inspection payload cannot supply a successful name receipt.
encodeWorldQuestion :: StructuredInspection -> WorldEncoding.Encoding
encodeWorldQuestion question = WorldEncoding.encodeListLen 4
  <> WorldEncoding.encodeWord (case structuredNamespace question of
    StructuredAnyName -> 0
    StructuredValueName -> 1
    StructuredTypeName -> 2
    StructuredConstructorName -> 3
    StructuredFieldName -> 4)
  <> WorldEncoding.encodeString (Text.pack (structuredName question))
  <> WorldEncoding.encodeWord64 (inspectionGeneration (structuredProvenance question))
  <> WorldEncoding.encodeString (Text.pack (inspectionFingerprint (structuredProvenance question)))

frameWorldQuery :: BS.ByteString -> BS.ByteString
frameWorldQuery body
  | toInteger (BS.length body) > 4294967295 = throw WorldQueryCorrupt
  | otherwise = "TPWQ0002"
      <> BS.pack [fromIntegral (BS.length body `div` (256 ^ power)) | power <- [0..3 :: Int]] <> body


-- Parsing never exports the held payload, compiler environment or a target.
-- The receipt correlates only immutable recipe/parser observations and the
-- actual canonical producer of the world borrowed by this operation.
planHeldWorldCell :: DynFlags -> WorldInputFacts -> WorldCellRecipe -> Maybe WorldCellOperandRequest -> IO BS.ByteString
planHeldWorldCell flags facts recipe operand = do
  plan <- parseWorldCellPlan flags recipe
  let bytes = encodeHeldWorldCellPlan facts recipe operand plan
  _ <- evaluate (BS.length bytes)
  pure bytes

parseWorldCellPlan :: DynFlags -> WorldCellRecipe -> IO CellSourcePlan
parseWorldCellPlan flags recipe = analyzeOrderedCellWithFlags flags (worldCellTemplate recipe) (worldCellSource recipe)
  >>= either (throwIO . SourceRejection . renderCellSplitError) pure

encodeHeldWorldCellPlan :: WorldInputFacts -> WorldCellRecipe -> Maybe WorldCellOperandRequest -> CellSourcePlan -> BS.ByteString
encodeHeldWorldCellPlan facts recipe operand plan =
  let text = WorldEncoding.encodeString . Text.pack
      digest = SHA256.hash . TextEncoding.encodeUtf8 . Text.pack
      templates = worldCellTurnTemplates recipe
      bindingForm item = case cellAnalysisBindingForm item of
        Just ActionBinding -> "action"
        Just LetBinding -> "let"
        Just RecursiveBinding -> "recursive"
        Nothing -> "none"
  in toStrictByteString (WorldEncoding.encodeListLen 9 <> text "TPCELLPLAN5"
    <> WorldEncoding.encodeBytes (worldInputsIdentityBytes facts)
    <> WorldEncoding.encodeBytes (worldInputsProducer facts)
    <> WorldEncoding.encodeBytes (digest (worldCellSource recipe))
    <> WorldEncoding.encodeBytes (digest (worldCellTemplate recipe))
    <> WorldEncoding.encodeListLen (fromIntegral (length templates))
    <> foldMap (\(kind,source) -> WorldEncoding.encodeListLen 2 <> text kind <> WorldEncoding.encodeBytes (digest source)) templates
    <> WorldEncoding.encodeBytes (encodeCellOut plan [] [] "")
    <> WorldEncoding.encodeListLen (fromIntegral (length (cellPlanItems plan)))
    <> foldMap (text . bindingForm) (cellPlanItems plan)
    <> maybe WorldEncoding.encodeNull (\(WorldCellOperandRequest _ sha) -> WorldEncoding.encodeBytes sha) operand)

data WorldCellRefusal = WorldCellUnsupported | WorldCellUnavailable | WorldCellCorrupt
  deriving (Eq, Show)
instance Exception WorldCellRefusal

validateHeldWorldCell :: DynFlags -> WorldInputFacts -> WorldCellPrepareRequest -> IO (Either WorldCellRefusal CellSourcePlan)
validateHeldWorldCell flags facts question = do
  timing <- readTimingEnabled
  emitCount timing "world.cell.admission_parses" 1
  plan <- parseWorldCellPlan flags (worldCellPrepareRecipe question)
  let slots = worldCellPrepareSlots question
      matches (ordinal,slot) (index,item) = ordinal == index && case (sbKind (cellAnalysisVerdict item),slot) of
        (KDecl,PlannedPrologue _) -> index == 0 && cellAnalysisPrologueOnly item
        (KDecl,PlannedDeclaration _) -> not (cellAnalysisPrologueOnly item)
        (KBind,PlannedBind _) -> True
        (KExpr,PlannedExpression _ _) -> True
        _ -> False
      actual = zip [0..] (cellPlanItems plan)
  evaluate $ if encodeHeldWorldCellPlan facts (worldCellPrepareRecipe question) (worldCellPrepareOperand question) plan /= worldCellPreparePlan question
      || length slots /= length actual || not (and (zipWith matches slots actual))
    then Left WorldCellCorrupt
    else Right plan

-- The local attempt borrows only fixed package state and captured source roots.
-- Its memo, home recovery and completed target remain with this completion;
-- nothing is published into the universe's source/original/recovery indexes.
data WorldSourceOperation result where
  WorldCellSource :: WorldCellPrepareRequest -> IORef (Maybe WorldCellOperandInputs)
    -> WorldCellPrefix -> HeldWorldCellStage -> PipelineSelection checked -> Set.Set SymbolIdentity
    -> WorldSourceOperation (CompletedCompilerResult checked)
  WorldObservationSource :: IO () -> WorldObservationStage -> WorldSourceOperation BS.ByteString

data WorldObservationStage = WorldObservationStage WorldObservationScaffold WorldObservationQuery
  CompilePurpose FilePath FilePath Int ObservationOperand [String] (SegmentDemandScope WorldDemandBinding)

-- The query's module identity depends on its actual lexical World, not a
-- random target path or one operand. Helpers therefore retain stable Names.
prepareWorldObservationStage :: CompilerUniverse -> HscEnv -> Bool -> HeldCompilerWorld
  -> WorldObservationQuery -> IO WorldObservationStage
prepareWorldObservationStage universe baseEnv timing
    world@(HeldCompilerWorld facts _ (WorldMaterialization realization _ roots _ _ _ _ _ _) _) query = do
  scaffold <- maybe (throwIO WorldQueryUnsupported) pure
    (worldLexicalObservation (worldInputsLexicalSurface facts))
  let owner = generatedScaffoldModuleName ["held-world-observation-v1",worldInputsCompilerIdentity facts]
      directory = realization </> "observation"
      path = directory </> owner ++ ".hs"
      flags = heldWorldCellFlags baseEnv directory roots
      lexical = worldInputsLexicalSurface facts
      declarationImports = case worldLexicalDeclaration lexical of
        Nothing -> []
        Just declaration -> ["import " ++ exactModule (worldDeclarationInterface declaration)]
      imports = worldObservationImports scaffold ++ worldLexicalImportLines lexical ++ declarationImports
  createDirectoryIfMissing True directory
  protected <- either fail pure (replaceTemplateModuleHeader
    ("module " ++ owner ++ " where\n" ++ unlines imports) (worldObservationFragment scaffold))
  let inert = "(let observation = observation in observation)"
      indent = unlines . map ("  " ++) . lines
      typeProbeHeader = "\n__tidepool_type_search_query :: " :: String
      probes = case query of
        WorldObservationExpression _ expression ->
          "\n__tidepool_inspect_0 = (\n" ++ indent expression ++ "  )\n"
        WorldObservationTypeSearch _ expression _ ->
          typeProbeHeader ++ expression
            ++ "\n__tidepool_type_search_query = " ++ inert ++ "\n"
        _ -> ""
      candidates = case query of
        WorldObservationInfo question -> keyed question
        WorldObservationType question -> keyed question
        WorldObservationRowFit question definingModule ->
          [alias | (alias,binding) <- Map.toList (worldInputsBindings facts)
            , let symbol = worldBindingSymbol binding
            , Text.unpack (symbolModule symbol) == definingModule
            , Text.unpack (symbolOccurrence symbol) == structuredName question]
        WorldObservationExpression _ _ -> []
        WorldObservationTypeSearch _ _ _ -> Map.keys (worldInputsBindings facts)
      keyed question = case structuredNamespace question of
        StructuredAnyName -> known (structuredName question)
        StructuredValueName -> known (structuredName question)
        _ -> []
      known name = [name | Map.member name (worldInputsBindings facts)]
      expression name = if isSymOcc (mkVarOcc name) then "(" ++ name ++ ")" else name
      probeNames = take (length candidates)
        [name | ordinal <- [0 :: Int ..], let name = "__tidepool_query_name_" ++ show ordinal
          , Map.notMember name (worldInputsBindings facts)]
      demandProbes = unlines [name ++ " () = " ++ expression alias
        | (name,alias) <- zip probeNames candidates]
      source = protected ++ probes ++ "\n" ++ demandProbes
      operand = case query of
        WorldObservationExpression {} -> ObservationExpressionOperand
        WorldObservationTypeSearch _ expression _ -> ObservationTypeOperand
          (length protected + length typeProbeHeader + length expression + 1)
        _ -> ObservationNoOperand
  writeFile path source
  recipe <- generatedCheckingTemplateRecipe flags (CheckedTemplateImports [] [])
    protected source path owner >>= either fail pure
  scope <- heldWorldDemandScope universe timing world
  pure (WorldObservationStage scaffold query (GeneratedScaffoldCompile recipe WorldObservationCompile)
    path directory (length protected) operand probeNames scope)

heldWorldDemandScope :: CompilerUniverse -> Bool -> HeldCompilerWorld -> IO (SegmentDemandScope WorldDemandBinding)
heldWorldDemandScope universe timing (HeldCompilerWorld facts _ _ preparedRef) = do
  WorldPreparedModules table receipts dependencies cached <- readIORef preparedRef
  case cached of
    Just scope -> pure scope
    Nothing -> do
      emitCount timing "world.cell.demand_scope_builds" 1
      emitCount timing "world.cell.demand_scope_aliases" (toInteger (Map.size (worldInputsBindings facts)))
      scope <- newSegmentDemandScope (universeEnvironment universe) (Map.map WorldBaseBinding (worldInputsBindings facts))
      writeIORef preparedRef (WorldPreparedModules table receipts dependencies (Just scope))
      pure scope

runWorldObservation :: IO () -> Maybe CompilerProducerIdentity -> CompilerUniverse -> HscEnv -> Bool
  -> IORef InterpreterAttemptState -> HeldCompilerWorld -> CompilerExecutionGrant -> CompilerExecutor
  -> WorldObservationQuery -> Ghc BS.ByteString
runWorldObservation guard producer universe baseEnv timing attempt world grant executor query = do
  stage <- liftIO (prepareWorldObservationStage universe baseEnv timing world query)
  runHeldWorldSource (pure ()) producer universe baseEnv timing attempt world grant executor (WorldObservationSource guard stage)

compileHeldWorldCell :: IO () -> Maybe CompilerProducerIdentity -> CompilerUniverse -> HscEnv -> Bool -> IORef InterpreterAttemptState
  -> HeldCompilerWorld -> CompilerExecutionGrant -> CompilerExecutor -> WorldCellPrepareRequest
  -> IORef (Maybe WorldCellOperandInputs) -> WorldCellPrefix -> HeldWorldCellStage -> PipelineSelection result -> Set.Set SymbolIdentity
  -> Ghc (CompletedCompilerResult result)
compileHeldWorldCell guard producer universe baseEnv timing attempt world grant executor question operandRef prefix stage selection retained =
  runHeldWorldSource guard producer universe baseEnv timing attempt world grant executor
    (WorldCellSource question operandRef prefix stage selection retained)

-- Both closed operations share source policy, dependency preparation, demand
-- resolution and the target HPT hook. Their completion owners remain distinct.
runHeldWorldSource :: IO () -> Maybe CompilerProducerIdentity -> CompilerUniverse -> HscEnv -> Bool -> IORef InterpreterAttemptState
  -> HeldCompilerWorld -> CompilerExecutionGrant -> CompilerExecutor -> WorldSourceOperation result -> Ghc result
runHeldWorldSource guard producer universe baseEnv timing attempt
    world@(HeldCompilerWorld facts _ (WorldMaterialization _ _ roots _ _ _ _ _ _) _) grant executor operation = do
  let (purpose,path,operationDirectory,offset,retained) = case operation of
        WorldCellSource question _ _ stage _ selected ->
          (worldCellStagePurpose stage,heldWorldCellStagePath stage,worldCellPrepareDirectory question,
            heldWorldCellStageOffset stage,selected)
        WorldObservationSource _ (WorldObservationStage _ _ selected target directory _ _ _ _) ->
          (selected,target,directory,0,Set.empty)
      inventories = case operation of
        WorldCellSource _ _ (WorldCellPrefix _ _ _ _ values _ _ _) _ _ _ -> values
        WorldObservationSource {} -> []
  -- Dependency completions use one closed compiler route. Caller-composed
  -- purpose wrappers cannot silently widen the retained dependency policy.
  unless (case operation of
    WorldCellSource {} -> closedWorldCellPurpose purpose
    WorldObservationSource {} -> originalPurpose purpose == WorldObservationCompile) $ liftIO $ do
      emitWorldAcquisitionRejection "world stage purpose" path "operation purpose is outside the admitted closed source route"
      throwIO WorldCellUnsupported
  directory <- liftIO (canonicalizePath operationDirectory)
  target <- liftIO (canonicalizePath path)
  unless ((directory ++ [pathSeparator]) `isPrefixOf` target) (liftIO (throwIO WorldCellCorrupt))
  borrowed <- liftIO (forkExactContextWithPackageFacts (universePackageFinder universe) (universeEnvironment universe))
  let flags = heldWorldCellFlags baseEnv directory roots
      policy = retainedContext retained
  -- Source compatibility runs before the target hook refines value demand.
  -- It must see the same module-scoped plugin as every source issuer.
  setSession (installRetainedUnfoldingsPlugin policy
    ((hscUpdateHPT (const emptyHomePackageTable) (hscUpdateFlags (const flags) borrowed))
      {hsc_hooks=hsc_hooks baseEnv,hsc_logger=hsc_logger baseEnv,hsc_targets=[]}))
  variant <- liftIO (normalVariant purpose)
  recipe <- maybe (liftIO (throwIO WorldCellCorrupt)) pure (pvGeneratedScaffold variant)
  capturedSource <- liftIO (captureGeneratedScaffoldSource recipe path >>= either (\reason -> do
    emitWorldAcquisitionRejection "world stage source recipe" path reason
    throwIO WorldCellCorrupt) pure)
  let source = capturedGeneratedScaffoldBytes capturedSource
      sourceText = Text.unpack (TextEncoding.decodeUtf8 source)
  effective <- liftIO (sourceModuleFlags flags sourceText >>= either (throwIO . SourceRejection . renderCellSplitError) pure)
  liftIO (requireWorldHeaderPolicy WorldAuthoredHeader path flags effective)
  parsedDemand <- either (\reason -> liftIO $ case operation of
    WorldCellSource {} -> do
      emitWorldAcquisitionRejection "world cell parsed demand" path (moduleDemandFailureDetail reason)
      throwIO WorldCellUnsupported
    WorldObservationSource {} -> do
      emitWorldObservationSourceRejection effective path source reason
      throwIO (SourceRejection (boundedDiagnosticText 4096 (moduleDemandFailureDetail reason)))) pure $
    case typedPurposePlan purpose of
      Just plan -> segmentModuleDemand <$> prepareSegmentDemand plan effective capturedSource
      Nothing | isJust (worldPlannedDeclaration purpose) || originalPurpose purpose == WorldObservationCompile -> prepareModuleDemand effective capturedSource
      _ -> Left UnprovedRootAbstraction
  case operation of
    WorldCellSource {} -> pure ()
    WorldObservationSource _ (WorldObservationStage _ _ _ _ _ prefix operand names _) -> do
      either (\detail -> liftIO (throwIO (SourceRejection detail))) pure
        (validateObservationContribution prefix operand names parsedDemand)
  -- Captured only after this issued stage has passed source policy and the
  -- parsed no-execution proof. The admission retains the same sealed operand
  -- across stages and retries; it never becomes a public World binding.
  operandEnvironment <- getSession
  operand <- liftIO $ case operation of
    WorldObservationSource {} -> pure Nothing
    WorldCellSource question operandRef _ _ _ _ -> case worldCellPrepareOperand question of
      Nothing -> pure Nothing
      Just (WorldCellOperandRequest descriptor sha) -> do
        cached <- readIORef operandRef
        case cached of
          Just value -> pure (Just value)
          Nothing -> do
            value <- readWorldCellOperandWithOwner (universeOriginalInputs universe) operandEnvironment facts descriptor sha
              `catch` \(WorldInputsFailure cause detail) -> do
                emitWorldAcquisitionRejection "world cell operand" descriptor detail
                throwIO (case cause of
                  WorldInputsUnavailable -> WorldCellUnavailable
                  WorldInputsCorrupt -> WorldCellCorrupt)
            writeIORef operandRef (Just value)
            pure (Just value)
  initialBase <- getSession
  initial <- liftIO $ case operation of
    WorldCellSource _ _ prefix _ _ _ -> restoreWorldCellPrefixHomes initialBase prefix
    WorldObservationSource {} -> pure initialBase
  (withOperand,operandProjection) <- liftIO (hydrateWorldCellOperand initial operand)
  let operandInterfaces = maybe [] (\(WorldCellOperandProjection value iface _ _) ->
        [(worldTypeEntryArtifact entry,worldTypeEntryInterface entry) | entry <- Map.elems (worldCellOperandTypes value)]
        ++ [(worldCellOperandInterface value,iface)]) operandProjection
  setSession withOperand
  declarationInputs <- liftIO (hydrateWorldDeclarationInputs (universeOriginalInputs universe) withOperand facts)
  (current,verifiedDeclaration) <- either (\reason -> liftIO $ do
    emitWorldAcquisitionRejection "world declaration type input" path reason
    throwIO WorldCellCorrupt) pure declarationInputs
  let importFailure :: WorldInputsFailure -> IO value
      importFailure (WorldInputsFailure cause detail) = do
        emitWorldAcquisitionRejection "world carried import" path detail
        throwIO (case cause of
          WorldInputsUnavailable -> WorldCellUnavailable
          WorldInputsCorrupt -> WorldCellCorrupt)
  importBasis <- liftIO (readWorldImportBasis (universeOriginalInputs universe) current facts
    (sourceImportIntents purpose) parsedDemand `catch` importFailure)
  prefixImports <- liftIO $ case operation of
    WorldCellSource _ _ prefix _ _ _ -> worldCellPrefixLexicalOwners current prefix
    WorldObservationSource {} -> pure (WorldPrefixLexicalOwners Map.empty)
  setSession current
  qualifyWorldImportSources importBasis
  qualified <- getSession
  imported <- liftIO (hydrateWorldImportBasis importBasis qualified `catch` importFailure)
  setSession imported
  resolvedRef <- liftIO (newIORef Nothing)
  nativeRef <- liftIO (newIORef Nothing)
  let requireNative = readIORef nativeRef >>= maybe (throwIO WorldCellCorrupt) pure
      requireResolved = readIORef resolvedRef >>= maybe (throwIO WorldCellCorrupt) pure
      resolveInputs environment sourceOwners = do
        existing <- readIORef resolvedRef
        case existing of
          Just resolved -> do
            emitCount timing "world.cell.demand_rehydrations" 1
            ready <- hydrateWorldCellResolvedInputs environment resolved
            pure (ready,resolved)
          Nothing -> do
            let baseScope = case operation of
                  WorldCellSource _ _ (WorldCellPrefix _ _ _ _ _ scope _ _) _ _ _ -> scope
                  WorldObservationSource _ (WorldObservationStage _ _ _ _ _ _ _ _ scope) -> scope
            demandScope <- case operandProjection of
              Just projection@(WorldCellOperandProjection value _ _ name) -> either (const (throwIO WorldCellCorrupt)) pure $
                associateSegmentDemandName name (operandAlias value) (WorldOperandBinding projection) baseScope
              Nothing -> pure baseScope
            emitCount timing "world.cell.demand_resolutions" 1
            available <- hydrateExactScope environment operandInterfaces
            let inputAnnotation = case operation of
                  WorldCellSource {} -> Nothing
                  WorldObservationSource _ (WorldObservationStage scaffold _ _ _ _ _ _ _ _) ->
                    (\signature -> (1,signature)) <$> worldObservationInputType scaffold
            annotations <- resolveNativeAnnotations available
              (worldLexicalObservation (worldInputsLexicalSurface facts) >>= worldObservationRequestTypes) inputAnnotation
            annotatedDemand <- annotateModuleDemand annotations parsedDemand
            writeIORef nativeRef (Just annotations)
            (ready,resolved) <- resolveWorldCellInputs (universeOriginalInputs universe) facts path
              offset sourceOwners available demandScope annotatedDemand (case operation of
                WorldCellSource {} -> WorldCellInputDemand
                WorldObservationSource {} -> WorldObservationInputDemand)
            writeIORef resolvedRef (Just resolved)
            pure (ready,resolved)
      policyFor resolved = retainedContext
        (Set.union retained (Map.keysSet (worldResolvedGenerations resolved)))
  importsRef <- liftIO (newIORef Nothing)
  -- Demanded values have source-less owners. Dependencies retain their own
  -- module-local policy; the target hook installs the resolved value policy.
  let privateDependencies = case operation of
        WorldCellSource _ _ (WorldCellPrefix _ _ _ previous _ _ _ _) _ _ _ -> previous
        WorldObservationSource {} -> Nothing
  dependencies <- liftIO (beginHeldWorldDependencyAttempt universe world privateDependencies)
  identity <- liftIO newTimingRequestIdentity
  start <- monotonicTime
  resources <- beginResourceTiming timing
  let cycle = worldDependencyCycle dependencies universe attempt
      withAliases = variant
        { pvCompilerProducer=producer,pvWorldOnly=True
        , pvDownsweepExcludes=maybe [] (pure . mkModuleName . exactModule . worldCellOperandInterface) operand
            ++ [mkModuleName (exactModule (worldTypeEntryArtifact entry)) | entry <- worldImportEntries importBasis]
            ++ case operation of
              WorldCellSource _ _ prefix _ _ _ -> map ms_mod_name (worldCellPrefixOriginalSummaries prefix)
              WorldObservationSource {} -> []
        , pvWorldOriginalBindings=Map.empty
        , pvWorldCompletedDependencies=case (privateDependencies,dependencies) of
            (Just (WorldPrivateCompletion previous _),WorldDependencyAttempt _ _ policy _ versions canonicalRecovery _ _ _ _) ->
              Just (WorldPrivateCompletion previous (WorldCompletedDependencies policy versions canonicalRecovery))
            _ -> Nothing
        , pvWorldSources=Just (WorldSourceInputBasis (universeOriginalInputs universe) facts importsRef parsedDemand operandInterfaces importBasis prefixImports)
        , pvTransformParsed = \target environment summary parsed -> do
            annotated <- if ms_mod_name summary /= target
              then pvTransformParsed variant target environment summary parsed
              else do
                transformed <- pvTransformParsed variant target environment summary parsed
                annotations <- requireNative
                thenNativeModule transformed (annotateNativeModule annotations)
            if ms_mod_name summary /= target then pure annotated else do
              resolved <- requireResolved
              baseDeclared <- mapNativeModule (\parsed -> case verifiedDeclaration of
                Nothing -> pure parsed
                Just declaration -> do
                  let artifact = verifiedWorldDeclarationInterface declaration
                      entries = [entry | entry <- verifiedWorldDeclarationTypeInputs declaration
                        , worldTypeEntryArtifact entry == artifact]
                      exports = concatMap (\value -> exportHead value : exportChildren value)
                        (verifiedWorldDeclarationExports declaration)
                  iface <- case entries of
                    [entry] -> pure (worldTypeEntryInterface entry)
                    _ -> throwIO WorldCellCorrupt
                  refineOriginalDeclarationImports (mi_module iface) (mi_iface_hash (mi_final_exts iface)) exports environment parsed) annotated
              declared <- mapNativeModule (transformProgramDeclarationImports inventories Nothing environment) baseDeclared
              thenNativeModule declared (transformCompletedValueAliases (worldResolvedAliases resolved) environment)
        , pvPlan = \directory enabled target graph exact -> do
            environment <- getSession
            prefixEnvironment <- case operation of
              WorldCellSource _ _ prefix _ _ _ -> do
                summary <- case [summary | ModuleNode _ summary <- mgModSummaries' graph
                    , ms_mod_name summary == target, ms_hsc_src summary == HsSrcFile] of
                  [summary] -> pure summary
                  _ -> liftIO (throwIO WorldCellCorrupt)
                liftIO (restoreWorldCellPrefixFor summary environment {hsc_mod_graph=graph} prefix)
              WorldObservationSource {} -> pure environment {hsc_mod_graph=graph}
            ready <- liftIO (restoreWorldImportBasis importBasis prefixEnvironment `catch` importFailure)
            setSession ready
            planned <- pvPlan variant directory enabled target graph exact
            let sourceOwners = Set.fromList
                  [(unitString (moduleUnit (ms_mod summary)),moduleNameString (ms_mod_name summary))
                  | ModuleNode _ summary <- mgModSummaries' graph]
            pure planned
              { cpAfterLoad = \ingress -> do
                  cpAfterLoad planned ingress
                  case operation of
                    WorldCellSource _ _ prefix _ _ _ -> do
                      summary <- case [current | ModuleNode _ current <- mgModSummaries' (cpLoadGraph planned)
                          , ms_mod_name current == target, ms_hsc_src current == HsSrcFile] of
                        [current] -> pure current
                        _ -> liftIO (throwIO WorldCellCorrupt)
                      environment <- getSession
                      ready <- liftIO (restoreWorldCellPrefixFor summary environment prefix)
                      setSession ready
                    WorldObservationSource {} -> pure ()
                  environment <- getSession
                  ready <- liftIO (restoreWorldImportBasis importBasis environment `catch` importFailure)
                  setSession ready
              , cpBeforeModule = \summary -> do
                  cpBeforeModule planned summary
                  case operation of
                    WorldCellSource _ _ prefix _ _ _ -> do
                      environment <- getSession
                      ready <- liftIO (restoreWorldCellPrefixFor summary environment prefix)
                      setSession ready
                    WorldObservationSource {} -> pure ()
                  environment <- getSession
                  ready <- liftIO (restoreWorldImportBasis importBasis environment `catch` importFailure)
                  setSession ready
                  when (ms_mod_name summary == target) $ do
                    environment <- getSession
                    (ready,resolved) <- liftIO (resolveInputs environment sourceOwners)
                    setSession (installRetainedUnfoldingsPlugin (policyFor resolved) ready)
              , cpInjectedSessionInterfaces = do
                  resolved <- requireResolved
                  (worldResolvedCaptures resolved ++) <$> cpInjectedSessionInterfaces planned
              }
        }
  let runStage :: PipelineSelection checked -> Ghc checked
      runStage selection = runCompileCycle (WithCompilerExecution grant executor selection) cycle policy
        Nothing timing identity start resources withAliases path
      completeDependencies = do
        final <- getSession
        pending <- liftIO (stageHeldWorldDependencies executor dependencies withAliases final path)
        recovery <- liftIO (worldDependencyRecovery dependencies)
        selected <- liftIO (readIORef importsRef >>= maybe (throwIO WorldCellCorrupt) pure)
        let WorldSourceSelection sourceSelection _ _ _ _ = selected
        forM_ sourceSelection $ \value -> do
          unchanged <- liftIO (revalidateSourceSelectedOriginals value)
          unless unchanged (liftIO (throwIO WorldCellUnavailable))
        pure (pending,recovery,selected)
  case operation of
    WorldCellSource _ _ _ _ selection _ -> do
      result <- runStage selection
      final <- getSession
      resolved <- liftIO requireResolved
      (pending,recovery,WorldSourceSelection sourceSelection imports sourceTypes _ _) <- completeDependencies
      selectedTypeBodies <- liftIO $ do
        let entries = worldResolvedTypes resolved ++ sourceTypes ++ worldImportEntries importBasis ++ maybe [] verifiedWorldDeclarationTypeInputs verifiedDeclaration
              ++ maybe [] (Map.elems . worldCellOperandTypes) operand
            selected = Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,worldTypeEntryInterfaceBody entry))
              | entry <- entries, let artifact = worldTypeEntryArtifact entry]
        unless (all (\entry -> let artifact = worldTypeEntryArtifact entry in
          maybe False (\(actual,body) -> actual == artifact && artifactSha256 body == artifactSha256 (worldTypeEntryInterfaceBody entry))
            (Map.lookup (exactUnit artifact,exactModule artifact) selected)) entries) (throwIO WorldCellCorrupt)
        pure (Map.elems selected)
      original <- liftIO $ traverse (\planned -> do
        inventory <- certifyPlannedDeclaration planned final >>= either fail pure
        pure (planned,inventory)) (worldPlannedDeclaration purpose)
      pure (CompletedWorldCompilerResult result recovery guard
        (WorldCompilerCompletion
          (WorldCellInputs (worldResolvedGenerations resolved) (worldResolvedReferences resolved)
            (target,SHA256.hash source) imports sourceSelection roots
            selectedTypeBodies)
          pending original) Nothing)
    WorldObservationSource requireActive (WorldObservationStage _ query _ _ _ _ _ _ _) -> do
      checked <- runStage CheckedEnvironment
      unless (Map.member "__tidepool_inspection_row" (crInspectionProbes checked))
        (liftIO (throwIO WorldQueryCorrupt))
      resolved <- liftIO requireResolved
      answers <- reifyGhc $ \session -> withInspectionWork $ \work ->
        reflectGhc (runWorldInspectionGhc work (crHscEnv checked) (crTargetTcGblEnv checked)
          (crTargetRdrEnv checked) (crInspectionProbes checked)
          (completedValueAliasIds (worldResolvedAliases resolved)) [observationInspection query]) session
      let encoded = encodeInspectionResults answers
      _ <- liftIO (evaluate (BS.length encoded))
      (pending,_,_) <- completeDependencies
      liftIO $ do
        requireActive
        requireCompilerExecutorOpen executor
        commitHeldWorldDependencies pending world
      pure encoded

observationInspection :: WorldObservationQuery -> InspectionRequest
observationInspection query = case query of
  WorldObservationInfo question -> InspectStructuredInfoOf question
  WorldObservationType question -> InspectStructuredTypeOf question
  WorldObservationRowFit question owner -> InspectKeyedAvailability question owner
  WorldObservationExpression _ expression -> InspectTypeOf expression
  WorldObservationTypeSearch _ expression limit -> InspectTypeSearchBounded expression limit

-- The admission owner is the sole rendered-stage producer. Descriptive
-- getters cannot replace the source, plan or purpose consumed at compilation.
renderHeldWorldCellStage :: DynFlags -> WorldCellPrepareRequest -> CellSourcePlan
  -> [HeldWorldCellItem] -> Int -> IO HeldWorldCellStage
renderHeldWorldCellStage flags question plan items offset = do
  let recipe = worldCellPrepareRecipe question
      directory = worldCellPrepareDirectory question </> "stage-" ++ show offset
      reservation = hexBytes (worldCellPrepareAdmission question)
      slots :: [(Word64,PlannedCellSlot)]
      slots = [(fromIntegral (heldWorldCellItemOrdinal item),heldWorldCellItemSlot item) | item <- items]
  createDirectoryIfMissing True directory
  rendered <- case (cellPlanItems plan,slots) of
    ([item],[(_,slot)]) | sbKind (cellAnalysisVerdict item) == KDecl -> do
      generation <- case slot of
        PlannedPrologue value -> pure value
        PlannedDeclaration value -> pure value
        _ -> throwIO WorldCellCorrupt
      wrapper <- either fail pure (originalDeclarationWrapper (worldCellTemplate recipe))
      original <- either (fail . show) pure (preparePlannedDeclaration
        (sessionModuleString (SessionModule LibMod (Generation generation))) wrapper plan)
      let path = directory </> plannedModule original ++ ".hs"
      writeFile path (plannedSource original)
      imports <- either fail pure (captureProtectedTemplateImportsAt "{{CELL_IMPORTS}}" flags wrapper)
      let pragmaLines = sum [length (lines (locatedPragmaSource pragma)) | pragma <- prologuePragmas (cellPlanPrologue plan)]
      mapped <- either fail pure (renderProtectedTemplateImports pragmaLines imports)
      scaffold <- generatedCheckingTemplateRecipeWithProtectedImports flags (CheckedTemplateImports [] [])
        mapped wrapper (plannedSource original) path (plannedModule original) >>= either fail pure
      let purpose = withSourceImportIntents (cellPlanPrologue plan)
            (GeneratedScaffoldCompile scaffold (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan)
              (WorldPlannedDeclarationCompile original)))
      pure (WorldRenderedOriginal original purpose path)
    _ -> do
      checking <- either fail pure (replaceTemplateModuleHeader "module CellCheck where" (worldCellTemplate recipe))
      let valueSlots = [(fromIntegral ordinal,generation,observation) | (ordinal,slot) <- slots
            , (generation,observation) <- case slot of
                PlannedBind value -> [(value,Nothing)]
                PlannedExpression value name -> [(value,Just name)]
                _ -> []]
      unless (length valueSlots == length slots) (throwIO WorldCellCorrupt)
      initial <- either fail pure (prepareTypedSegmentSource checking plan reservation valueSlots)
      let owner = generatedScaffoldModuleName ["held-world-cell-v1",worldCellPrepareIdentity question
            ,hexBytes (SHA256.hash (worldCellPreparePlan question)),reservation,preparedTypedSegmentSource initial]
      named <- either fail pure (replaceTemplateModuleHeader ("module " ++ owner ++ " where") checking)
      source <- either fail pure (prepareTypedSegmentSource named plan reservation valueSlots)
      unless (preparedTypedSegmentPlan source == preparedTypedSegmentPlan initial) (throwIO WorldCellCorrupt)
      let protected = preparedTypedSegmentTemplate source
          path = directory </> owner ++ ".hs"
      imports <- either fail pure (captureProtectedTemplateImportsAt "{{CELL_IMPORTS}}" flags protected)
      mapped <- either fail pure (renderProtectedTemplateImports (preparedTypedSegmentLineOffset source) imports)
      writeFile path (preparedTypedSegmentSource source)
      scaffold <- generatedTypedSegmentRecipeWithProtectedImports (preparedTypedSegmentOperations source) flags
        (CheckedTemplateImports [] []) mapped protected (preparedTypedSegmentSource source) path owner >>= either fail pure
      let purpose = withSourceImportIntents (cellPlanPrologue plan)
            (GeneratedScaffoldCompile scaffold (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan)
              (TypedSegmentCompile (preparedTypedSegmentPlan source) (preparedTypedSegmentOperations source)
                (CheckedItemCompile [] Nothing []))))
      pure (WorldRenderedTyped source purpose path)
  pure (HeldWorldCellStage plan items offset rendered)

worldPlannedDeclaration :: CompilePurpose -> Maybe PlannedDeclaration
worldPlannedDeclaration (WorldPlannedDeclarationCompile planned) = Just planned
worldPlannedDeclaration (ParsedImportSelection _ inner) = worldPlannedDeclaration inner
worldPlannedDeclaration (GeneratedScaffoldCompile _ inner) = worldPlannedDeclaration inner
worldPlannedDeclaration (GeneratedInstanceCheck _ inner) = worldPlannedDeclaration inner
worldPlannedDeclaration _ = Nothing

worldPurposeMatchesStage :: WorldCellPrepareRequest -> Int -> CellSourcePlan -> CompilePurpose -> Bool
worldPurposeMatchesStage question offset plan purpose = case (typedPurposePlan purpose,worldPlannedDeclaration purpose) of
  (Just typed,Nothing) ->
    map (fromIntegral . plannedItemOrdinal) (typedSegmentPlanItems typed) == map fst slots
      && map plannedItemGeneration (typedSegmentPlanItems typed) == map (slotGeneration . snd) slots
      && all ((/= KDecl) . sbKind . cellAnalysisVerdict) (cellPlanItems plan)
  (Nothing,Just original) -> case (slots,cellPlanItems plan) of
    ([(_,slot)],[item]) -> sbKind (cellAnalysisVerdict item) == KDecl
      && plannedModule original == sessionModuleString (SessionModule LibMod (Generation (slotGeneration slot)))
      && map cellAnalysisSourceItems (cellPlanItems (plannedCheckPlan original)) == map cellAnalysisSourceItems (cellPlanItems plan)
    _ -> False
  _ -> False
  where
    slots = take (length (cellPlanItems plan)) (drop offset (worldCellPrepareSlots question))
    slotGeneration (PlannedPrologue value) = value
    slotGeneration (PlannedDeclaration value) = value
    slotGeneration (PlannedBind value) = value
    slotGeneration (PlannedExpression value _) = value

worldCellPrefixOriginalSummaries :: WorldCellPrefix -> [ModSummary]
worldCellPrefixOriginalSummaries (WorldCellPrefix _ _ graph _ _ _ _ _) =
  [summary | ModuleNode _ summary <- mgModSummaries' graph]

worldCellPrefixLexicalOwners :: HscEnv -> WorldCellPrefix -> IO WorldPrefixLexicalOwners
worldCellPrefixLexicalOwners environment prefix@(WorldCellPrefix _ homes _ _ _ _ _ _) = do
  owners <- forM (worldCellPrefixOriginalSummaries prefix) $ \summary -> do
    let owner = ms_mod summary
    home <- maybe (throwIO WorldCellCorrupt) pure (lookupHpt homes (moduleName owner))
    unless (mi_module (hm_iface home) == owner) (throwIO WorldCellCorrupt)
    pure $ if moduleUnit owner == homeUnitAsUnit (hsc_home_unit environment)
      && ms_hsc_src summary == HsSrcFile
      then Map.singleton (moduleName owner) (unitString (moduleUnit owner))
      else Map.empty
  WorldPrefixLexicalOwners <$> mergeWorldLexicalOwners owners

-- Keep the actual successful compiler homes. Re-typechecking their interfaces
-- would construct another details knot and lose the completed stage's Names.
-- Value homes remain with the demand scope until an importer selects them.
restoreWorldCellPrefixHomes :: HscEnv -> WorldCellPrefix -> IO HscEnv
restoreWorldCellPrefixHomes environment (WorldCellPrefix _ homes graph _ _ _ _ _) =
  foldM restore environment [summary | ModuleNode _ summary <- mgModSummaries' graph]
  where
    restore current summary = do
      let owner = ms_mod summary
      home <- maybe (throwIO WorldCellCorrupt) pure (lookupHpt homes (moduleName owner))
      unless (mi_module (hm_iface home) == owner) (throwIO WorldCellCorrupt)
      selected <- case lookupHpt (hsc_HPT current) (moduleName owner) of
        Nothing -> pure home
        Just existing -> do
          unless (sameWorldCellHome existing home) (throwIO WorldCellCorrupt)
          -- A reserved Original cannot be redefined within this request.
          -- Make may have reconstructed equal interface bytes with another
          -- lazy details knot; preserve the actual finalized owner instead.
          pure $ case parseSessionModule (moduleNameString (moduleName owner)) of
            Just selected | smKind selected == LibMod -> home
            _ -> existing
      _ <- addHomeModuleToFinder (hsc_FC current) (hsc_home_unit current)
        (GWIB (moduleName owner) (if ms_hsc_src summary == HsBootFile then IsBoot else NotBoot)) (ms_location summary)
      pure (hscUpdateHPT (\table -> addToHpt table (moduleName owner) selected) current)

sameWorldCellHome :: HomeModInfo -> HomeModInfo -> Bool
sameWorldCellHome left right = mi_module (hm_iface left) == mi_module (hm_iface right)
  && mi_iface_hash (mi_final_exts (hm_iface left)) == mi_iface_hash (mi_final_exts (hm_iface right))

-- Make supplies a phase-specific dependency environment. Restore the justified
-- lexical graph there, before rnImports and instance/family lookup, while
-- retaining every fresh phase node and its current compiler state.
restoreWorldCellPrefixFor :: ModSummary -> HscEnv -> WorldCellPrefix -> IO HscEnv
restoreWorldCellPrefixFor summary environment prefix@(WorldCellPrefix _ _ graph _ _ _ _ _)
  | Set.null (sourceImportOwners summary `Set.intersection` owners) = pure environment
  | otherwise = do
      ready <- restoreWorldCellPrefixHomes environment prefix
      let retained = mgModSummaries' graph
          present = Set.fromList (map mkNodeKey (mgModSummaries' (hsc_mod_graph ready)))
          retainedKeys = Set.fromList (map mkNodeKey retained)
          imports = Map.fromList
            [(mkNodeKey node,Set.fromList (sourceImportNodeKeys current))
              | node@(ModuleNode _ current) <- mgModSummaries' (hsc_mod_graph ready)]
          addEdges node@(ModuleNode edges current) =
            let imported = Set.toList (Map.findWithDefault Set.empty (mkNodeKey node) imports
                  `Set.intersection` retainedKeys)
                inherited = [dependency | retainedNode@(ModuleNode dependencies _) <- retained
                  , mkNodeKey retainedNode == mkNodeKey node, dependency <- dependencies]
            in ModuleNode (Set.toList (Set.fromList (edges ++ imported ++ inherited))) current
          addEdges node = node
          current = map addEdges (mgModSummaries' (hsc_mod_graph ready))
          missing = [node | node <- retained, mkNodeKey node `Set.notMember` present]
      pure ready {hsc_mod_graph=mkModuleGraph (current ++ missing)}
  where owners = Set.fromList [ms_mod retained | ModuleNode _ retained <- mgModSummaries' graph]

-- Retain the successful graph projection, including SOURCE/boot identity.
-- Virtual locations keep these completed nodes out of Make without retaining
-- paths or buffers owned by an earlier stage's disposable source directory.
retainWorldCellOriginal :: PreparedPipelineResult -> HomePackageTable -> ModuleGraph -> PlannedDeclaration
  -> IO (HomePackageTable,ModuleGraph)
retainWorldCellOriginal prepared homes graph planned = do
  -- The finalization owner retains hscTidy's actual details. A post-Make HPT
  -- lookup may instead return an interface-reloaded lazy knot without the
  -- captured value environment; equal interface bytes cannot replace it.
  original <- maybe (throwIO WorldCellCorrupt) (pure . finalizedHomeModInfo)
    (Map.lookup (mkModuleName (plannedModule planned)) (pprFinalizedModules prepared))
  unless (mi_module (hm_iface original) == mkModule
      (homeUnitAsUnit (hsc_home_unit environment)) (mkModuleName (plannedModule planned)))
    (throwIO WorldCellCorrupt)
  root <- case [node | node@(ModuleNode _ summary) <- completed
      , ms_mod summary == mi_module (hm_iface original), ms_hsc_src summary == HsSrcFile] of
    [node] -> pure (mkNodeKey node)
    _ -> throwIO WorldCellCorrupt
  let selected = visit Set.empty [root]
      added = [virtualize selected node | node <- completed
        , mkNodeKey node `Set.member` selected, mkNodeKey node `Set.notMember` known]
      modules = Set.fromList [ms_mod summary | ModuleNode _ summary <- added]
  retained <- foldM retainHome homes (Set.toList modules)
  pure (retained,mkModuleGraph (mgModSummaries' graph ++ added))
  where
    environment = prHscEnv (pprPipelineResult prepared)
    completed = mgModSummaries' (hsc_mod_graph environment)
    prior = mgModSummaries' graph
    nodes = Map.fromList [(mkNodeKey node,node) | node <- completed ++ prior]
    known = Set.fromList (map mkNodeKey prior)
    dependencies = Map.fromListWith Set.union
      [(mkNodeKey node,Set.fromList (nodeDependencies False node ++ sourceImportNodeKeys summary))
        | node@(ModuleNode _ summary) <- completed ++ prior]
    lexical (ModuleNode _ summary) = case
        parseSessionModule (moduleNameString (ms_mod_name summary)) of
      Just selected -> smKind selected /= ValMod
      Nothing -> True
    lexical _ = True
    visit seen [] = seen
    visit seen (key:rest)
      | key `Set.member` seen = visit seen rest
      | key `Set.member` known = visit (Set.insert key seen) rest
      | otherwise = case Map.lookup key nodes of
          Just node | lexical node -> visit (Set.insert key seen)
            (Set.toList (Map.findWithDefault Set.empty key dependencies) ++ rest)
          _ -> visit seen rest
    retainHome table owner = do
      home <- case Map.lookup (moduleName owner) (pprFinalizedModules prepared) of
        Just finalized -> pure (finalizedHomeModInfo finalized)
        Nothing -> maybe (throwIO WorldCellCorrupt) pure
          (lookupHpt homes (moduleName owner) <|> lookupHpt (hsc_HPT environment) (moduleName owner))
      unless (mi_module (hm_iface home) == owner) (throwIO WorldCellCorrupt)
      forM_ (lookupHpt table (moduleName owner)) $ \previous ->
        unless (sameWorldCellHome previous home) (throwIO WorldCellCorrupt)
      pure (addToHpt table (moduleName owner) home)
    virtualize selected (ModuleNode originalEdges summary) =
      let owner = ms_mod summary
          artifact = ExactIfaceArtifact (unitString (moduleUnit owner))
            (moduleNameString (moduleName owner))
            ("<private-world-prefix>/" ++ moduleNameString (moduleName owner) ++ ".hi") "" []
          location = ms_location (exactInterfaceSummary environment artifact)
          imported = Set.fromList (sourceImportNodeKeys summary) `Set.intersection` selected
          edges = Set.toList (Set.fromList originalEdges `Set.union` imported)
      in ModuleNode edges summary
        {ms_location=location, ms_parsed_mod=Nothing, ms_hspp_file=ml_hi_file location, ms_hspp_buf=Nothing}
    virtualize _ node = node

advanceWorldCellPrefix :: CompilerExecutor -> WorldInputFacts -> WorldCellPrepareRequest -> Int -> CellSourcePlan
  -> PreparedPipelineResult -> WorldCompilerCompletion -> WorldCellPrefix -> IO WorldCellPrefix
advanceWorldCellPrefix executor facts question offset current result completion
    (WorldCellPrefix cursor homes graph dependencies inventories scope inherited _) = do
  let environment = prHscEnv (pprPipelineResult result)
      captures = prProducedSessionInterfaces (pprPipelineResult result)
      slotByGeneration = Map.fromList
        [(generation,(ordinal,slot)) | (ordinal,slot) <- worldCellPrepareSlots question
          , let generation = case slot of
                  PlannedPrologue value -> value
                  PlannedDeclaration value -> value
                  PlannedBind value -> value
                  PlannedExpression value _ -> value]
  additions <- fmap concat $ forM captures $ \capture -> do
    let (owner,bytes) = capturedSessionInterface capture
    selected <- evaluate (lookupHpt (hsc_HPT environment) (moduleName owner)) >>= maybe (throwIO WorldCellCorrupt) evaluate
    unless (mi_module (hm_iface selected) == owner) (throwIO WorldCellCorrupt)
    SessionModule ValMod (Generation generation) <- maybe (throwIO WorldCellCorrupt) pure
      (parseSessionModule (moduleNameString (moduleName owner)))
    (ordinal,_) <- maybe (throwIO WorldCellCorrupt) pure (Map.lookup generation slotByGeneration)
    unless (fromIntegral ordinal >= offset && fromIntegral ordinal < offset + length (cellPlanItems current)) (throwIO WorldCellCorrupt)
    let requirements = maybe [] (\(_,_,required) -> required) (capturedSessionInterfaceEvidence capture)
        artifact = ExactIfaceArtifact (unitString (moduleUnit owner)) (moduleNameString (moduleName owner))
          (worldCellPrepareDirectory question </> "item-" ++ show ordinal </> "value.hi")
          (hexBytes (SHA256.hash bytes)) requirements
    forM (Set.toList (Set.fromList (concatMap availNames (mi_exports (hm_iface selected))))) $ \name -> do
      symbol <- maybe (throwIO WorldCellCorrupt) pure (nameSymbolIdentity "value" name)
      let alias = occNameString (nameOccName name)
      pure (alias,WorldStagedValue ordinal symbol (stableVarId name) (worldCellPrepareDefiningScope question) generation
        artifact (hm_iface selected) capture selected)
  let homesWithValues = foldl' (\table (_,WorldStagedValue _ _ _ _ _ _ _ _ home) -> addToHpt table (moduleName (mi_module (hm_iface home))) home) homes additions
  (nextHomes,nextGraph,nextInventories) <- case worldCompilerCompletionPlannedDeclaration completion of
    Nothing -> pure (homesWithValues,graph,inventories)
    Just (planned,inventory) -> do
      (retained,extended) <- retainWorldCellOriginal result homesWithValues graph planned
      pure (retained,extended,inventories ++ [inventory])
  nextScope <- updateSegmentDemandScope environment [(alias,Just (WorldPriorBinding value)) | (alias,value) <- additions] scope
  nextDependencies <- borrowHeldWorldDependencies executor facts (worldCellPrepareRealization question)
    dependencies (worldCompilerCompletionDependencies completion)
  pure (WorldCellPrefix (cursor+1) nextHomes nextGraph (Just nextDependencies) nextInventories nextScope inherited WorldStagePending)

worldPreparedResult :: PipelineSelection result -> result -> Maybe PreparedPipelineResult
worldPreparedResult PreparedStg result = Just result
worldPreparedResult (PreparedProducts _) result = Just result
worldPreparedResult (PreparedSegmentProducts _ _) result = Just (preparedSegmentProducts result)
worldPreparedResult (WithTypedSegmentPreparation _ inner) result = worldPreparedResult inner result
worldPreparedResult (WithCompilerExecution _ _ inner) result = worldPreparedResult inner result
worldPreparedResult (WithPreparedModuleCompletion _ inner) result = worldPreparedResult inner result
worldPreparedResult _ _ = Nothing

-- The private constructor keeps the origin and its seal correlated. A World
-- row CID and a captured prior-stage interface hash never share an untagged slot.
data WorldCellInputOrigin = WorldBaseInput | WorldPriorInput Word64 Word64 | WorldOperandInput
  deriving (Eq,Ord)
data WorldCellInputReference = WorldCellInputReference WorldCellInputOrigin String SymbolIdentity Word64 Word64 Word64 BS.ByteString
  deriving (Eq,Ord)

worldCellInputReferenceBinding :: WorldCellInputReference -> (SymbolIdentity,Word64)
worldCellInputReferenceBinding (WorldCellInputReference _ _ symbol _ _ generation _) = (symbol,generation)

-- Native root selection can remove compiler availability, never create a
-- reference or choose between incompatible owners of the same compiler Name.
-- Distinct consumer stages may use the same genuine earlier capture.
selectWorldCellExecutionInputReferences :: Set.Set (SymbolIdentity,Word64) -> [WorldCellInputReference]
  -> Either WorldCellRefusal [WorldCellInputReference]
selectWorldCellExecutionInputReferences needed inputs = do
  let selected = filter ((`Set.member` needed) . worldCellInputReferenceBinding) inputs
      groups = Map.fromListWith (++) [(worldCellInputReferenceBinding reference,[reference]) | reference <- selected]
      fact (WorldCellInputReference origin _ _ variable scope generation seal) =
        (case origin of WorldPriorInput producer _ -> WorldPriorInput producer 0; other -> other
        ,variable,scope,generation,seal)
  forM_ (Map.elems groups) $ \group -> case group of
    [] -> Left WorldCellCorrupt
    first:rest -> unless (all ((== fact first) . fact) rest) (Left WorldCellCorrupt)
  pure selected

encodeWorldCellInputReference :: WorldCellInputReference -> WorldEncoding.Encoding
encodeWorldCellInputReference (WorldCellInputReference origin occurrence symbol variable scope generation seal) =
  WorldEncoding.encodeListLen 7 <> (case origin of
    WorldBaseInput -> WorldEncoding.encodeListLen 1 <> WorldEncoding.encodeWord 0
    WorldOperandInput -> WorldEncoding.encodeListLen 1 <> WorldEncoding.encodeWord 2
    WorldPriorInput producer consumer -> WorldEncoding.encodeListLen 3 <> WorldEncoding.encodeWord 1
      <> WorldEncoding.encodeWord64 producer <> WorldEncoding.encodeWord64 consumer)
  <> WorldEncoding.encodeString (Text.pack occurrence) <> encodeSymbol symbol
  <> WorldEncoding.encodeWord64 variable <> WorldEncoding.encodeWord64 scope <> WorldEncoding.encodeWord64 generation
  <> WorldEncoding.encodeBytes seal

data WorldStagedValue = WorldStagedValue Word64 SymbolIdentity Word64 Word64 Word64
  ExactIfaceArtifact ModIface CapturedSessionInterface HomeModInfo
data WorldDemandBinding = WorldBaseBinding WorldBinding | WorldPriorBinding WorldStagedValue
  | WorldOperandBinding WorldCellOperandProjection

-- Issued together from one installed interface; the Name and capture cannot
-- be supplied independently of the sealed operation input.
data WorldCellOperandProjection = WorldCellOperandProjection WorldCellOperandInputs ModIface CapturedSessionInterface Name

operandAlias :: WorldCellOperandInputs -> String
operandAlias operand = case worldCellOperandExposure operand of
  WorldInputOperand -> "input"
  WorldSessionInputOperand -> "sessionInput"

worldNeedsAlias :: WorldDemandBinding -> Bool
worldNeedsAlias (WorldOperandBinding _) = False
worldNeedsAlias _ = True

-- Source-less type availability is required even when the helper is unused.
-- The exact exported Name is only a reverse demand association; no GRE or
-- executable input obligation is issued by installing this interface.
hydrateWorldCellOperand :: HscEnv -> Maybe WorldCellOperandInputs
  -> IO (HscEnv,Maybe WorldCellOperandProjection)
hydrateWorldCellOperand environment Nothing = pure (environment,Nothing)
hydrateWorldCellOperand environment (Just operand) = do
  let artifact = worldCellOperandInterface operand
  selected <- maybe (throwIO WorldCellCorrupt) pure (parseSessionModule (exactModule artifact))
  typed <- hydrateExactScope environment
    [(worldTypeEntryArtifact entry,worldTypeEntryInterface entry) | entry <- Map.elems (worldCellOperandTypes operand)]
  (ready,_,capture) <- injectCapturedSessionIfaceWithBindings selected (worldCellOperandBody operand)
    (worldCellOperandPackages operand) (exactRequirements artifact) typed
  home <- maybe (throwIO WorldCellCorrupt) pure (lookupHpt (hsc_HPT ready) (mkModuleName (exactModule artifact)))
  unless (mi_module (hm_iface home) == mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact)))
    (throwIO WorldCellCorrupt)
  name <- case [name | exported <- mi_exports (hm_iface home),name <- availNames exported
      , nameSymbolIdentity "value" name == Just (worldBindingSymbol (worldCellOperandBinding operand))] of
    [actual] -> pure actual
    _ -> throwIO WorldCellCorrupt
  registerSessionInterfaceLocation (exactPath artifact) selected ready
  pure (ready,Just (WorldCellOperandProjection operand (hm_iface home) capture name))

worldDemandSymbol :: WorldDemandBinding -> SymbolIdentity
worldDemandSymbol (WorldBaseBinding binding) = worldBindingSymbol binding
worldDemandSymbol (WorldOperandBinding (WorldCellOperandProjection operand _ _ _)) = worldBindingSymbol (worldCellOperandBinding operand)
worldDemandSymbol (WorldPriorBinding (WorldStagedValue _ symbol _ _ _ _ _ _ _)) = symbol
worldDemandGeneration :: WorldDemandBinding -> Word64
worldDemandGeneration (WorldBaseBinding binding) = worldBindingGeneration binding
worldDemandGeneration (WorldOperandBinding (WorldCellOperandProjection operand _ _ _)) = worldBindingGeneration (worldCellOperandBinding operand)
worldDemandGeneration (WorldPriorBinding (WorldStagedValue _ _ _ _ generation _ _ _ _)) = generation
worldDemandArtifact :: WorldDemandBinding -> ExactIfaceArtifact
worldDemandArtifact (WorldBaseBinding binding) = worldBindingInterfaceArtifact binding
worldDemandArtifact (WorldOperandBinding (WorldCellOperandProjection operand _ _ _)) = worldCellOperandInterface operand
worldDemandArtifact (WorldPriorBinding (WorldStagedValue _ _ _ _ _ artifact _ _ _)) = artifact

-- A request-local result of actual demand under the target's dependency HPT.
-- It owns only selected immutable input products, never a saved HscEnv. Every
-- target visit installs the same products and the same resolved alias Names.
data WorldCellResolvedInputs = WorldCellResolvedInputs
  [(String,WorldDemandBinding)] Int [WorldTypeClosureEntry] [(ExactIfaceArtifact,ModIface)]
  [CapturedSessionInterface] CompletedValueAliases

worldResolvedTypes :: WorldCellResolvedInputs -> [WorldTypeClosureEntry]
worldResolvedTypes (WorldCellResolvedInputs _ _ types _ _ _) = types

worldResolvedAliases :: WorldCellResolvedInputs -> CompletedValueAliases
worldResolvedAliases (WorldCellResolvedInputs _ _ _ _ _ aliases) = aliases

worldResolvedCaptures :: WorldCellResolvedInputs -> [CapturedSessionInterface]
worldResolvedCaptures (WorldCellResolvedInputs _ _ _ _ captures _) = captures

worldResolvedGenerations :: WorldCellResolvedInputs -> Map.Map SymbolIdentity Word64
worldResolvedGenerations (WorldCellResolvedInputs inputs _ _ _ _ _) = Map.fromList
  [(worldDemandSymbol binding,worldDemandGeneration binding) | (_,binding) <- inputs]

worldResolvedReferences :: WorldCellResolvedInputs -> [WorldCellInputReference]
worldResolvedReferences (WorldCellResolvedInputs inputs consumer _ _ _ _) = map reference inputs
  where
    reference (name,WorldBaseBinding binding) = WorldCellInputReference WorldBaseInput name
      (worldBindingSymbol binding) (worldBindingSessionVar binding) (worldBindingScope binding)
      (worldBindingGeneration binding) (worldBindingRowFactCID (worldBindingRow binding))
    reference (name,WorldOperandBinding (WorldCellOperandProjection operand _ _ _)) = let binding = worldCellOperandBinding operand in
      WorldCellInputReference WorldOperandInput name
        (worldBindingSymbol binding) (worldBindingSessionVar binding) (worldBindingScope binding)
        (worldBindingGeneration binding) (worldBindingRowFactCID (worldBindingRow binding))
    reference (name,WorldPriorBinding (WorldStagedValue producer symbol variable scope generation _ _ capture _)) =
      WorldCellInputReference (WorldPriorInput producer (fromIntegral consumer)) name symbol variable scope generation
        (SHA256.hash (snd (capturedSessionInterface capture)))

hydrateWorldCellResolvedInputs :: HscEnv -> WorldCellResolvedInputs -> IO HscEnv
hydrateWorldCellResolvedInputs environment (WorldCellResolvedInputs inputs _ types interfaces _ _) = do
  ready <- hydrateExactScope environment
    ([(worldTypeEntryArtifact entry,worldTypeEntryInterface entry) | entry <- types] ++ interfaces)
  let requested = [(name,worldDemandSymbol binding,worldDemandArtifact binding) | (name,binding) <- inputs, worldNeedsAlias binding]
  fst <$> (hydrateCompletedValueAliases requested interfaces ready >>= either reject pure)
  where reject reason = emitWorldAcquisitionRejection "world value rehydration" "<target input projection>" reason
          >> throwIO WorldCellCorrupt

data WorldSourceDemand = WorldCellInputDemand | WorldObservationInputDemand

resolveWorldCellInputs :: ExactInputOwner -> WorldInputFacts -> FilePath -> Int -> Set.Set (String,String)
  -> HscEnv -> SegmentDemandScope WorldDemandBinding -> ParsedModuleDemand -> WorldSourceDemand
  -> IO (HscEnv,WorldCellResolvedInputs)
resolveWorldCellInputs owner facts path consumer sourceOwners environment scope parsedDemand mode = do
  demand <- moduleInputOccurrences environment scope parsedDemand
  demanded <- either (\failure -> do
    emitWorldAcquisitionRejection "world cell demand" path (case failure of
      InputDemandRenameRejected diagnostic -> diagnostic
      _ -> show failure)
    case mode of
      WorldCellInputDemand -> throwIO WorldCellUnsupported
      WorldObservationInputDemand -> throwIO (SourceRejection (case failure of
        InputDemandRenameRejected diagnostic -> diagnostic
        _ -> show failure))) pure demand
  let reject :: String -> IO value
      reject reason = emitWorldAcquisitionRejection "world value import" path reason >> throwIO WorldCellCorrupt
  readback <- readWorldBindingInputs owner environment facts [name | (name,WorldBaseBinding _) <- demanded]
  (_,bodies,typeInputs) <- either reject pure readback
  let inputs = demanded
      prior = [value | (_,WorldPriorBinding value) <- inputs]
      operands = [value | (_,WorldOperandBinding value) <- inputs]
      allTypes = Map.unions (typeInputs:[worldCellOperandTypes value | WorldCellOperandProjection value _ _ _ <- operands])
  unless (all (\(WorldStagedValue producer _ _ _ _ _ _ _ _) -> producer < fromIntegral consumer) prior)
    (reject "prior value is not before the consuming stage")
  -- Value owners must never be source summaries: adding their retained policy
  -- at the target cannot change a dependency's earlier memo fingerprint.
  unless (all (\(artifact,_,_) -> (exactUnit artifact,exactModule artifact) `Set.notMember` sourceOwners) bodies
      && all (\(_,binding) -> let symbol = worldDemandSymbol binding
        in (Text.unpack (symbolUnit symbol),Text.unpack (symbolModule symbol)) `Set.notMember` sourceOwners) inputs)
    (reject "selected value owner is also a source module")
  withTypes <- hydrateExactScope environment
    [(worldTypeEntryArtifact entry,worldTypeEntryInterface entry) | entry <- Map.elems allTypes]
  (injected,reversedCaptures,reversedInterfaces) <- foldM (\(current,captures,interfaces) (artifact,body,packages) -> do
    selected <- maybe (throwIO WorldCellCorrupt) pure (parseSessionModule (exactModule artifact))
    (ready,_,captured) <- injectCapturedSessionIfaceWithBindings selected body packages (exactRequirements artifact) current
    home <- maybe (throwIO WorldCellCorrupt) pure (lookupHpt (hsc_HPT ready) (mkModuleName (exactModule artifact)))
    unless (mi_module (hm_iface home) == mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact)))
      (throwIO WorldCellCorrupt)
    pure (ready,captured:captures,(artifact,hm_iface home):interfaces)) (withTypes,[],[]) bodies
  let priorInterfaces = Map.elems (Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,iface))
        | WorldStagedValue _ _ _ _ _ artifact iface _ _ <- prior])
      priorCaptures = Map.elems (Map.fromList [(fst (capturedSessionInterface capture),capture)
        | WorldStagedValue _ _ _ _ _ _ _ capture _ <- prior])
      operandInterfaces = [(worldCellOperandInterface value,iface)
        | WorldCellOperandProjection value iface _ _ <- take 1 operands]
      interfaces = reverse reversedInterfaces ++ priorInterfaces ++ operandInterfaces
      requested = [(name,worldDemandSymbol binding,worldDemandArtifact binding) | (name,binding) <- inputs, worldNeedsAlias binding]
  (hydrated,aliases) <- hydrateCompletedValueAliases requested interfaces injected >>= either reject pure
  let operandCaptures = [capture | WorldCellOperandProjection _ _ capture _ <- take 1 operands]
  let resolved = WorldCellResolvedInputs inputs consumer (Map.elems allTypes) interfaces
        (reverse reversedCaptures ++ priorCaptures ++ operandCaptures) aliases
      generations = worldResolvedGenerations resolved
  unless (all (\(_,binding) -> Map.lookup (worldDemandSymbol binding) generations == Just (worldDemandGeneration binding)) inputs)
    (throwIO WorldCellCorrupt)
  pure (hydrated,resolved)

closedWorldCellPurpose :: CompilePurpose -> Bool
closedWorldCellPurpose (ParsedImportSelection _ (GeneratedScaffoldCompile _
    (GeneratedInstanceCheck _ (TypedSegmentCompile _ _ (CheckedItemCompile [] Nothing []))))) = True
closedWorldCellPurpose (ParsedImportSelection _ (GeneratedScaffoldCompile _
    (GeneratedInstanceCheck _ (WorldPlannedDeclarationCompile _)))) = True
closedWorldCellPurpose _ = False

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
  , completedModuleHome :: !(Maybe HomeModInfo)
  , completedModuleRecovery :: RecoveryContext
  , completedModuleEpoch :: Word64
  , completedModuleProducts :: [ModuleProduct]
  }

-- Retain the selected home, rather than a lookup closure over the full HscEnv.
-- The strict field forces the Maybe; its selected payload must also be in WHNF.
retainCompletedModuleHome :: Maybe HomeModInfo -> Maybe HomeModInfo
retainCompletedModuleHome Nothing = Nothing
retainCompletedModuleHome (Just home) = home `seq` Just home

data CompletedOriginalVersion = CompletedOriginalVersion
  { completedOriginalHome :: HomeModInfo
  , completedOriginalRecovery :: RecoveryContext
  , completedOriginalEpoch :: Word64
  }

-- Only the active attempt carries targets and a mutable memo. It is released
-- after the request; immutable dependency nodes survive independently.
newtype ActiveCompilerAttempt = ActiveCompilerAttempt
  { activeEnvironment :: HscEnv
  }

data CycleState
  = StandaloneCycle
  | TransactionCycle ModIfaceCache (IORef GutsMemo) (Maybe (Either String CapturedCandidateManifest))
      (Map.Map Module (Map.Map MemoSelectionKey (Map.Map MemoValidity CompletedModuleVersion)))
      WorldCanonicalRecovery
      (Map.Map Module CompletedModuleVersion -> IO CompilerRecoveryCaches)
      (IORef CompilerInterpreterState) PackageFinderFacts (IORef InterpreterAttemptState)

data CompilePurpose = GeneralCompile | InspectionTypeCompile | WorldObservationCompile | CertifyHomeProductsCompile | OriginalDeclarationCompile
  | WorldPlannedDeclarationCompile PlannedDeclaration
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

data CompilePurposeConflict
  = DuplicateCompilePurposeSetting String
  | MultipleCompilePurposeLeaves
  | MissingCompilePurposeLeaf
  deriving (Eq, Show)

instance Exception CompilePurposeConflict

-- Admission normalizes the requested compile settings once. Equivalent wrapper
-- orders no longer make one nesting order a hidden contract; genuinely
-- repeated settings refuse instead of silently shadowing each other.
data CompilePurposeSettings = CompilePurposeSettings
  { settingsExactScopes :: [ExactScope]
  , settingsProgramImports :: Maybe ProgramSourceImports
  , settingsImportIntents :: Maybe [ImportIntent]
  , settingsScaffold :: Maybe GeneratedScaffoldRecipe
  , settingsInstance :: Maybe GeneratedInstanceRecipe
  , settingsSegment :: Maybe (TypedSegmentPlan, GeneratedSegmentOperations)
  , settingsLeaf :: Maybe CompilePurpose
  }

emptyCompilePurposeSettings :: CompilePurposeSettings
emptyCompilePurposeSettings = CompilePurposeSettings [] Nothing Nothing Nothing Nothing Nothing Nothing

normalizeCompilePurpose :: CompilePurpose -> Either CompilePurposeConflict CompilePurpose
normalizeCompilePurpose purpose = rebuild =<< collect purpose emptyCompilePurposeSettings
  where
    collect (ExactScopeCompile inner scope) settings = collect inner settings
      { settingsExactScopes = settingsExactScopes settings ++ [scope] }
    collect (CompletedProgramImports imports inner) settings
      | isJust (settingsProgramImports settings) =
          Left (DuplicateCompilePurposeSetting "CompletedProgramImports")
      | otherwise = collect inner settings { settingsProgramImports = Just imports }
    collect (ParsedImportSelection intents inner) settings
      | isJust (settingsImportIntents settings) =
          Left (DuplicateCompilePurposeSetting "ParsedImportSelection")
      | otherwise = collect inner settings { settingsImportIntents = Just intents }
    collect (GeneratedScaffoldCompile recipe inner) settings
      | isJust (settingsScaffold settings) =
          Left (DuplicateCompilePurposeSetting "GeneratedScaffoldCompile")
      | otherwise = collect inner settings { settingsScaffold = Just recipe }
    collect (GeneratedInstanceCheck recipe inner) settings
      | isJust (settingsInstance settings) =
          Left (DuplicateCompilePurposeSetting "GeneratedInstanceCheck")
      | otherwise = collect inner settings { settingsInstance = Just recipe }
    collect (TypedSegmentCompile plan operations inner) settings
      | isJust (settingsSegment settings) =
          Left (DuplicateCompilePurposeSetting "TypedSegmentCompile")
      | otherwise = collect inner settings { settingsSegment = Just (plan,operations) }
    collect leaf settings = case settingsLeaf settings of
      Nothing -> Right settings { settingsLeaf = Just leaf }
      Just _ -> Left MultipleCompilePurposeLeaves
    rebuild settings = do
      leaf <- maybe (Left MissingCompilePurposeLeaf) Right (settingsLeaf settings)
      let segment = case settingsSegment settings of
            Nothing -> leaf
            Just (plan,operations) -> TypedSegmentCompile plan operations leaf
          instanceCheck = maybe segment (\recipe -> GeneratedInstanceCheck recipe segment)
            (settingsInstance settings)
          scaffold = maybe instanceCheck (\recipe -> GeneratedScaffoldCompile recipe instanceCheck)
            (settingsScaffold settings)
          intents = maybe scaffold (\selected -> ParsedImportSelection selected scaffold)
            (settingsImportIntents settings)
          imports = maybe intents (\selected -> CompletedProgramImports selected intents)
            (settingsProgramImports settings)
      pure (foldr (\scope inner -> ExactScopeCompile inner scope) imports
        (settingsExactScopes settings))

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
originalPurpose (WorldPlannedDeclarationCompile _) = OriginalDeclarationCompile
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
transformFor (WorldPlannedDeclarationCompile _) target env summary = transformFor OriginalDeclarationCompile target env summary
transformFor OriginalDeclarationCompile target env summary
  | ms_mod_name summary == target = fmap unannotatedModule . refineParsedDeclarationImports env
  | otherwise = pure . unannotatedModule
transformFor CertifyHomeProductsCompile _ _ _ = pure . unannotatedModule
transformFor WorldObservationCompile target env summary = transformFor InspectionTypeCompile target env summary
transformFor InspectionTypeCompile target _ summary
  | ms_mod_name summary == target = pure . unannotatedModule . normalizeInspectionWildcards
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
  , cpAfterLoad :: (ModSummary -> Ghc (Either HomeModInfo ModSummary)) -> Ghc ()
    -- ^ Runs immediately after @load'@ and its @ghc_load@ phase emit, before
    -- summaries are taken, after the shared load barrier. The session path
    -- restores its module graph here; dependency-directed Val injection occurs at
    -- 'cpBeforeModule'.
  , cpSummaries :: Ghc [ModSummary]
    -- ^ The modules to compile, in compile ORDER, BEFORE the hs-boot filter
    -- (which is the skeleton's, at one site).
  , cpKeepPrivateResult :: Bool
  , cpResultBinders :: [String]
    -- ^ OccNames to try, in order, for 'ownedPrResultType' — the @result@ vs
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
    -- ^ Applied to the post-loop session before it becomes 'ownedPrHscEnv'.
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
  , loadedAcquisition :: AcquiredOriginalProduct
  }

loadedFinalized :: LoadedModule -> FinalizedModule
loadedFinalized = acquiredOriginalFinalization . loadedAcquisition

freshLoadedModule :: ModSummary -> ModuleFacts -> ModuleOutput -> FinalizedModule -> LoadedModule
freshLoadedModule summary facts output original = LoadedModule summary facts output
  (FreshCompletion (FreshSourceAdmission (ms_mod summary) (ms_hsc_src summary)
    (ms_hs_hash summary) (ml_hs_file (ms_location summary))) original)

data CanonicalFrontendFailure
  = CustomLoadPhaseHook
  | CustomLoadFrontendHook
  | UnsupportedLoadBackend
  | CompilerProducerUnavailable
  | CompilerProducerScopeMismatch
  | LoadedFinalizationOwnerMismatch
  | MissingLoadedFrontend
  | MissingLoadedFinalization
  | DuplicateLoadedFinalization Module
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
    DuplicateLoadedFinalization owner -> renderGhc "DuplicateLoadedFinalization" owner
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
withExactInterfaceTransaction :: [FilePath] -> (ExactInterfaceOperations -> IO a) -> IO a
withExactInterfaceTransaction includes use =
  withResidentCompilerScopes includes $ \runScope ->
    runScope (pure ()) $ \compiler ->
      withScopedExactInterfaceTransaction compiler includes use

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
    Just scope | isSessionScopeActive scope -> sessionVariant purpose scope
    _ -> normalVariant purpose
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

-- Load deferral follows GHC's resolved graph. Excluded source-less home
-- imports have no downsweep edge, so retain their qualifier and boot kind.
-- A source/boot pair belongs to one SOURCE component and is deferred together.
sourceLoadDependencyGraph :: ModuleGraph -> Map.Map NodeKey (Set.Set NodeKey)
sourceLoadDependencyGraph graph = Map.fromListWith Set.union
  ([(mkNodeKey node, Set.fromList (nodeDependencies False node)) | node <- nodes]
    ++ [(mkNodeKey node, Set.fromList (excludedImports summary))
      | node@(ModuleNode _ summary) <- nodes]
    ++ [(key, Set.singleton sibling) | node@(ModuleNode _ summary) <- nodes
      , let key = mkNodeKey node
            sibling = mkNodeKey (ModuleNode [] summary
              {ms_hsc_src=if ms_hsc_src summary == HsBootFile then HsSrcFile else HsBootFile})
      , sibling `Set.member` keys])
  where
    nodes = mgModSummaries' graph
    keys = Set.fromList (map mkNodeKey nodes)
    excludedImports = sourceImportNodeKeys

-- Actual import edges keep SOURCE and ordinary keys distinct. The separate
-- load-deferral graph also pairs boot siblings, but those are not GHC edges.
sourceImportNodeKeys :: ModSummary -> [NodeKey]
sourceImportNodeKeys summary =
  [mkNodeKey (ModuleNode [] summary {ms_mod=owner, ms_hsc_src=sourceKind})
  | (sourceKind, imports) <- [(HsSrcFile,ms_textual_imps summary),(HsBootFile,ms_srcimps summary)]
  , (qualifier,name) <- imports
  , Just owner <- [case qualifier of
      NoPkgQual -> Just (mkModule (moduleUnit (ms_mod summary)) (unLoc name))
      ThisPkg unit -> Just (mkModule (stringToUnit (unitIdString unit)) (unLoc name))
      OtherPkg _ -> Nothing]]

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

-- Publication and reuse share the same unsealed-input closure. A successful
-- target cannot turn a source-less value alias into reusable source evidence.
admittedSourceMemoSnapshot :: PipelineVariant -> HscEnv -> GutsMemo -> GutsMemo
admittedSourceMemoSnapshot variant final snapshot = Map.filter
  ((`Set.notMember` refused) . payloadOwner . gmePayload) snapshot
  where
    unsealed = unsealedSourceInputs variant final
    indirect = Set.fromList [payloadOwner (gmePayload entry)
      | entry <- Map.elems snapshot, usesUnsealedSourceInputs unsealed final entry]
    refused
      | Set.null unsealed = Set.empty
      | otherwise = reverseDependencyClosure (sourceDependencyGraph (hsc_mod_graph final))
          (Set.union unsealed indirect)

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

-- World checking admits quote syntax only after proving that its provider
-- cannot execute. The generated target borrows the same sealed parser proof
-- used by demand; other demanded modules are parsed from downsweep buffers.
-- Each source is proved before its renaming or code-generation phase.
admitWorldQuoteFreeModuleGraph :: Bool -> Maybe (Module,ParsedModuleDemand) -> ModuleGraph -> Ghc ModuleGraph
admitWorldQuoteFreeModuleGraph timing targetProof graph = do
  forM_ summaries $ \summary -> do
    let flags = ms_hspp_opts summary
    when (hasUnconditionallyUntrackedCompileTimeExecution flags)
      (liftIO (throwIO (WorldModulePolicyFailure (ms_hspp_file summary) WorldUntrackedOptions)))
    when (xopt LangExt.StaticPointers flags && backendGeneratesCode (backend flags))
      (liftIO (throwIO (WorldModulePolicyFailure (ms_hspp_file summary) WorldCodeGeneration)))
    when (xopt LangExt.QuasiQuotes flags) $ case targetProof of
      Just (owner,_) | owner == ms_mod summary -> pure ()
      _ -> do
        when (isNothing (ms_hspp_buf summary))
          (liftIO (throwIO (WorldModulePolicyFailure (ms_hspp_file summary) WorldUntrackedOptions)))
        parsed <- parseModule summary
        unless (classifyQuasiQuoteUse parsed == NoQuasiQuotes)
          (liftIO (throwIO (WorldModulePolicyFailure (ms_hspp_file summary) WorldUntrackedOptions)))
  -- Downsweep propagates the target's executable-backend requirement to
  -- dependencies even when those dependencies do not enable quote syntax.
  -- Authored codegen options were refused at FileArgs. Once every summary's
  -- compiler-time inputs are proved inert, lower only that derived backend.
  let narrowed = mapMG (\summary -> summary
        {ms_hspp_opts=(ms_hspp_opts summary) {backend=noBackend}}) graph
  current <- getSession
  setSession current {hsc_mod_graph=narrowed}
  liftIO (emitCount timing "world.quote_free_modules" (toInteger (length summaries)))
  pure narrowed
  where summaries = [summary | ModuleNode _ summary <- mgModSummaries' graph]

-- Diagnostic only; the typed parser fact owns reuse eligibility.
renderQuasiQuoteUse :: QuasiQuoteUse -> String
renderQuasiQuoteUse NoQuasiQuotes = "none"
renderQuasiQuoteUse HasQuasiQuotes = "untracked"

-- Facts needed even when a module contributes no executable body. Keeping
-- these separately lets an unchanged re-export or validation-only module
-- prove its dependents valid without retaining its compiler session graph.
data ModuleFacts = ModuleFacts
  { moduleFactReferences :: Set.Set Module
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
  , productAcquisition :: AcquiredOriginalProduct
  }

-- The completion issuer chooses provenance once. A reused proof never gets
-- attached later to a different source frontend's finalization.
data AcquiredOriginalProduct
  = FreshCompletion !FreshSourceAdmission FinalizedModule
  | ReusedCanonical AdmittedFinalizedOriginal

-- Only the source completion issuer constructs this small ingress fact. Keep
-- parsed trees and phase environments out of dormant product provenance.
data FreshSourceAdmission = FreshSourceAdmission !Module !HscSource !Fingerprint !(Maybe FilePath)

productFinalized :: ModuleProduct -> FinalizedModule
productFinalized = acquiredOriginalFinalization . productAcquisition

acquiredOriginalFinalization :: AcquiredOriginalProduct -> FinalizedModule
acquiredOriginalFinalization acquisition = case acquisition of
  FreshCompletion _ original -> original
  ReusedCanonical original -> admittedOriginalModule original

data MemoPayload
  = ValidationOnly ModuleFacts ModuleOutput AcquiredOriginalProduct
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
  | CanonicalObservation ModSummary CanonicalOriginalInput
  | HydratedObservation ModSummary HomeModInfo AdmittedSourceCandidate

observationSummary :: ModuleObservation -> ModSummary
observationSummary (CachedObservation summary _) = summary
observationSummary (LoadedObservation loaded) = loadedSummary loaded
observationSummary (CanonicalObservation summary _) = summary
observationSummary (HydratedObservation summary _ _) = summary

observationFacts :: ModuleObservation -> IO ModuleFacts
observationFacts (CachedObservation _ entry) = pure (payloadFacts (gmePayload entry))
observationFacts (LoadedObservation loaded) = pure (loadedFacts loaded)
observationFacts (CanonicalObservation _ input) = pure (hydratedModuleFacts (canonicalOriginalPackages input))
observationFacts (HydratedObservation _ _ admitted) = pure
  (hydratedModuleFacts (admittedCandidateRoots admitted))
    { moduleFactReferences = Set.fromList
        [ mkModule (stringToUnit (Text.unpack (symbolUnit identity)))
            (mkModuleName (Text.unpack (symbolModule identity)))
        | group <- candidateGroups (admittedCandidateOriginal admitted)
        , global <- candidateGroupGlobals group
        , let (identity, required) = candidateOriginalGlobalDemand global
        , required ] }

-- Checked HPT metadata alone has no admitted native outline. Executable
-- observations add their already authenticated candidate demand above.
hydratedModuleFacts :: PackageImportEvidence -> ModuleFacts
hydratedModuleFacts roots = ModuleFacts
  { moduleFactReferences = Set.empty
  , moduleFactPackageImports = roots
  , moduleFactHasDependentFiles = False
  , moduleFactQuasiQuoteUse = NoQuasiQuotes
  }

payloadFacts :: MemoPayload -> ModuleFacts
payloadFacts (ValidationOnly facts _ _) = facts
payloadFacts (ExecutableProduct moduleProduct) = productFacts moduleProduct

payloadProduct :: MemoPayload -> Maybe ModuleProduct
payloadProduct (ValidationOnly _ _ _) = Nothing
payloadProduct (ExecutableProduct moduleProduct) = Just moduleProduct

payloadLoaded :: ModSummary -> MemoPayload -> LoadedModule
payloadLoaded summary (ValidationOnly facts output acquisition) =
  LoadedModule summary facts output acquisition
payloadLoaded summary (ExecutableProduct product') =
  LoadedModule summary (productFacts product') (productOutput product') (productAcquisition product')

payloadFinalized :: MemoPayload -> FinalizedModule
payloadFinalized (ValidationOnly _ _ acquisition) = acquiredOriginalFinalization acquisition
payloadFinalized (ExecutableProduct product') = productFinalized product'

payloadOwner :: MemoPayload -> Module
payloadOwner (ValidationOnly _ output _) = moduleOutputModule output
payloadOwner (ExecutableProduct product') = moduleOutputModule (productOutput product')

requireProduct :: LoadedModule -> Maybe PreparedModule -> Ghc ModuleProduct
requireProduct loaded (Just prepared) = pure ModuleProduct
  { productFacts = loadedFacts loaded
  , productOutput = loadedOutput loaded
  , productPrepared = prepared
  , productAcquisition = loadedAcquisition loaded
  }
requireProduct _ Nothing = liftIO $ ioError $ userError
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
    { moduleFactReferences = mfReferencedModules front
    , moduleFactPackageImports = mfPackageImports front
    , moduleFactHasDependentFiles = mfHasDependentFiles front
    , moduleFactQuasiQuoteUse = mfQuasiQuoteUse front
    }

type GutsMemo = Map.Map ModuleName GutsMemoEntry

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
    completionRecoveryRef <- liftIO (newIORef Nothing)
    packageFinder <- case cycleState of
      StandaloneCycle -> getSession >>= liftIO . newPackageFinderFacts
      TransactionCycle _ _ _ _ _ _ _ facts _ -> pure facts
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
      TransactionCycle _ _ _ _ _ _ _ _ attempt -> pure attempt
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
          TransactionCycle cache _ _ _ _ _ _ _ _ -> Just cache
        mMemoRef = case cycleState of
          StandaloneCycle -> Nothing
          TransactionCycle _ memo _ _ _ _ _ _ _ -> Just memo
        capturedCandidates = case cycleState of
          StandaloneCycle -> Nothing
          TransactionCycle _ _ captured _ _ _ _ _ _ -> captured
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
      liftIO (validateExactScopeRequest scope)
      env <- getSession
      verified <- liftIO (validateExactScopeEnvironment env scope)
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
    acquiredCanonicalOriginalsRef <- liftIO (newIORef Map.empty)
    actualOriginalNamesRef <- liftIO (newIORef Map.empty)
    actualOriginalReferencesRef <- liftIO (newIORef Map.empty)
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
    modGraphDownsweep <- withWorldModulePreprocessing (checksPublicModule selection || pvWorldOnly variant) path $
      depanalSourceModules (pvDownsweepExcludes variant)
    policyTarget <- if pvWorldOnly variant && isJust capturedTarget
      then Just <$> liftIO (targetModuleFor modGraphDownsweep path)
      else pure Nothing
    targetQuoteProof <- case (pvWorldSources variant,capturedTarget,policyTarget) of
      (Just (WorldSourceInputBasis _ _ _ proof _ _ _),Just bytes,Just owner)
        | moduleDemandMatchesSource proof path bytes -> pure (Just (owner,proof))
        | otherwise -> liftIO (throwIO WorldCellCorrupt)
      (Nothing,_,_) -> pure Nothing
      _ -> liftIO (throwIO WorldCellCorrupt)
    modGraphRaw <- if checksPublicModule selection || pvWorldOnly variant
      then admitWorldQuoteFreeModuleGraph timing targetQuoteProof modGraphDownsweep
      else elideUnusedQuasiQuoteCodegen timing modGraphDownsweep
    when (checksPublicModule selection || pvWorldOnly variant) $
      forM_ [summary | ModuleNode _ summary <- mgModSummaries' modGraphRaw] $ \summary -> liftIO $ do
        requireWorldHeaderPolicy WorldDownsweepGraph (ms_hspp_file summary) (hsc_dflags previous) (ms_hspp_opts summary)
        when (isReservedSessionModuleName (moduleNameString (ms_mod_name summary))
            && Just (ms_mod summary) /= policyTarget)
          (throwIO (WorldModulePolicyFailure (ms_hspp_file summary) WorldPrivateModule))
    forM_ (pvExactScope variant) $ \scope -> do
      let exactNames = Set.fromList [mkModuleName (exactModule iface)
            | (iface, _, _) <- scopeInterfaces scope]
      when (any (\node -> case node of
          ModuleNode _ summary -> ms_mod_name summary `Set.member` exactNames
          _ -> False) (mgModSummaries' modGraphDownsweep)) $
        liftIO $ ioError $ userError "fresh source collides with an admitted exact owner"
    targetOwner <- liftIO (targetModuleFor modGraphRaw path)
    let targetName = moduleName targetOwner
        prepareTypedTarget summary env tcg = case typedPlanFor selection of
          Just typedPlan | ms_mod_name summary == targetName -> do
            prepare <- maybe (throwIO MissingSegmentPreparation) pure (typedPreparationFor selection)
            segment <- captureTypedSegment typedPlan env tcg
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
      TransactionCycle _ _ _ _ _ _ retainedInterpreter _ _ -> pure retainedInterpreter
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
      TransactionCycle _ memo _ versions _ _ _ _ _ -> liftIO $ do
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
    worldSources <- forM (pvWorldSources variant) $ \(WorldSourceInputBasis owner facts receipt _ privateInputs imports prefixImports) -> do
      when (isJust (pvExactScope variant)) (liftIO (throwIO WorldCellCorrupt))
      selected <- withHeldWorldSourceRefusal (captureHeldWorldSourceSelection owner facts privateInputs imports prefixImports modGraphRaw)
      liftIO (writeIORef receipt (Just selected))
      pure selected
    sourceSelection <- case pvExactScope variant of
      Nothing -> pure Nothing
      Just scope -> withSourceSelectionRefusal
        (selectCurrentSourceOriginals scope (programSourceImports (pvPurpose variant))
          (pvSourceImportIntents variant) (pvGeneratedScaffold variant) modGraphRaw)
    selectedExact <- traverse (either (liftIO . fail) pure . extendSourceSelectedOriginals sourceSelection)
      (pvExactScope variant)
    let canonicalOriginalInputs = case worldSources of
          Just (WorldSourceSelection _ _ _ _ inputs) -> inputs
          Nothing -> Nothing
        canonicalOriginalFor summary
          | ms_hsc_src summary == HsSrcFile = canonicalOriginalInputs >>=
              (\inputs -> lookupCanonicalOriginalInput inputs (ms_mod summary))
          | otherwise = Nothing
    let nativeReference owner = do
          WorldSourceInputBasis _ facts _ _ _ _ _ <- pvWorldSources variant
          worldOriginalReferenceFor facts owner
        nativeCanonical summary = isJust (canonicalOriginalFor summary)
          && isJust (nativeReference (ms_mod summary))
        nativeOriginalInputs =
          [(ms_mod summary,hm_iface (canonicalOriginalHome input),reference) | summary <- sourceOrder
            , input <- maybe [] pure (canonicalOriginalFor summary)
            , reference <- maybe [] pure (nativeReference (ms_mod summary))]
          ++ [(mi_module iface,iface,reference)
             | WorldSourceInputBasis _ _ _ _ _ imports _ <- maybe [] pure (pvWorldSources variant)
             , entry <- worldImportEntries imports
             , let iface = worldTypeEntryInterface entry
             , reference <- maybe [] pure (nativeReference (mi_module iface))]
        referenceForName name = case Set.toAscList (Set.fromList
            [reference | (owner,iface,reference) <- nativeOriginalInputs
              , nameModule_maybe name == Just owner
                || any (name `elem`) (map availNames (mi_exports iface))]) of
          [] -> pure Nothing
          [reference] -> pure (Just reference)
          _ -> throwIO (OriginalNativeNameConflict name)
    let (freshGraph, exactImports) = case worldSources of
          Nothing -> sourceEvidenceGraph selectedExact modGraphRaw
          Just (WorldSourceSelection selected imports _ _ _) ->
            let owners = Set.fromList [key | value <- maybe [] pure selected,(key,_,_,_) <- selectedOriginalRows value]
                -- Strip the same exact imports this completion recorded,
                -- including source-less carried interfaces. Their absence
                -- from source lookup is not a package-resolution witness.
                (ordinary,_) = sourceEvidenceGraphFor
                  (Map.fromList [(mkModuleName name,unit)
                    | (_,edges) <- imports,(_,name,_,unit) <- edges]) modGraphRaw
                fresh (ModuleNode _ summary) =
                  (unitString (moduleUnit (ms_mod summary)),moduleNameString (ms_mod_name summary)) `Set.notMember` owners
                fresh _ = True
            in (mkModuleGraph (filter fresh (mgModSummaries' ordinary)),imports)
        -- Compiled selected-source qualification belongs to its own receipt.
        -- Fresh completion evidence cannot replace an admitted exact owner.
        sourceCaptureGraph = if isJust worldSources then freshGraph else modGraphRaw
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
        (compilerProducerFor variant) selectedExact
        sourceFreeOwners capturedCandidates manifest modGraphRaw targetName
    let acceptedCandidates = if sourceReuseDisabled then Map.empty else normalCandidates
        exactCanonicalInterfaces = maybe Map.empty scopeCanonicalInterfaces selectedExact
        candidateCanonicalInterfaces = Map.fromList
          [((candidateUnit original,candidateModule original),
              ModuleInterfaceAdmission (admittedCandidateProof admitted))
            | admitted <- Map.elems acceptedCandidates
            , let original = admittedCandidateOriginal admitted]
        worldCanonicalInterfaces = case worldSources of
          Nothing -> Map.empty
          Just (WorldSourceSelection _ _ _ selected _) -> selected
        -- Every admission remains paired with its actual issuer. Map precedence
        -- cannot select between independently admitted proofs of the same owner.
        duplicateCanonicalOwners = Set.toAscList (Set.unions
          [ Map.keysSet (Map.intersection exactCanonicalInterfaces candidateCanonicalInterfaces)
          , Map.keysSet (Map.intersection exactCanonicalInterfaces worldCanonicalInterfaces)
          , Map.keysSet (Map.intersection candidateCanonicalInterfaces worldCanonicalInterfaces)
          ])
        selectedCanonicalInterfaces = Map.unions
          [exactCanonicalInterfaces,candidateCanonicalInterfaces,worldCanonicalInterfaces]
    unless (null duplicateCanonicalOwners) $ liftIO $ fail
      ("canonical interface admissions duplicate an owner: " ++ show duplicateCanonicalOwners)
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
                      (completedModuleHome node)
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
                     TransactionCycle _ _ _ versions _ _ _ _ _ ->
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
      TransactionCycle _ _ _ _ _ activateRecovery _ _ _ -> liftIO $ do
        selectedVersions <- readIORef selectedVersionsRef
        recovery <- activateRecovery (Map.restrictKeys selectedVersions (Map.keysSet validatedMemo))
        writeIORef completionRecoveryRef (Just recovery)
    let recompiledOwners = Set.fromList [ms_mod summary | summary <- sourceOrder
          , ms_mod summary `Map.notMember` validatedMemo
          , ms_mod_name summary `Map.notMember` acceptedCandidates]
        freshSource summary = ms_hsc_src summary == HsSrcFile
          && ms_mod summary `Set.member` recompiledOwners
          && isNothing (canonicalOriginalFor summary)
        sourceFlags = Map.fromList [(ms_mod summary, ms_hspp_opts summary) | summary <- sourceOrder]
        forceFreshSource summary
          | freshSource summary = summary
              {ms_hspp_opts=gopt_set (ms_hspp_opts summary) Opt_ForceRecomp}
          | otherwise = summary
        restoreFreshSource summary
          | freshSource summary
          , Just original <- Map.lookup (ms_mod summary) sourceFlags
          , not (gopt Opt_ForceRecomp original) = summary
              {ms_hspp_opts=gopt_unset (ms_hspp_opts summary) Opt_ForceRecomp}
          | otherwise = summary
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
      TransactionCycle cache _ _ _ _ _ _ _ _ -> do
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
              TransactionCycle _ _ _ versions _ _ _ _ _
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
    observedPlan <- pvPlan variant compilerViewDirectory timing targetName modGraphRaw selectedExact
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
        candidateKeys = Set.fromList [mkNodeKey (ModuleNode [] (admittedCandidateSummary candidate))
          | candidate <- Map.elems acceptedCandidates]
        deferred = if loadRequired then Set.empty else
          reverseDependencyClosure (sourceLoadDependencyGraph modGraphRaw) candidateKeys
        candidateLoadGraph = mkModuleGraph
          [node | node <- mgModSummaries' (cpLoadGraph originalPlan)
            , mkNodeKey node `Set.notMember` deferred]
        plan | Set.null acceptedNames = originalPlan
             | otherwise = originalPlan
                 { cpLoadGraph = candidateLoadGraph
               , cpAfterLoad = \freshIngress -> do
                     cpAfterLoad originalPlan freshIngress
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
                    (mkModule (toUnitId (moduleUnit targetOwner)) targetName))
              [] -> (plannedLoadGraph, LoadAllTargets)
          _ -> (plannedLoadGraph, LoadAllTargets)
    -- Only the closed final make graph needs disposable physical interfaces.
    -- Interface-only peers still need views when another candidate needs make.
    liftIO (emitCount timing "candidate_compiler_view_attempt" 0)
    liftIO (emitCount timing "candidate_compiler_view_complete" 0)
    let loadedCandidateOwners = Set.fromList
          [ms_mod summary | ModuleNode _ summary <- mgModSummaries' loadGraph
            , ms_hsc_src summary == HsSrcFile]
    candidateViews <- fmap Map.fromList $ forM
      [(index,candidate) | (index,(_,candidate)) <- zip [0..] (Map.toAscList acceptedCandidates)
        , ms_mod (admittedCandidateSummary candidate) `Set.member` loadedCandidateOwners] $ \(index,candidate) -> do
          let summary = admittedCandidateSummary candidate
              originalFlags = ms_hspp_opts summary
              canonicalSummary = summary {ms_hspp_opts =
                (canonicalizeDFlags originalFlags)
                  {backend = backend originalFlags, ghcLink = ghcLink originalFlags}}
          liftIO (emitCount timing "candidate_compiler_view_attempt" 1)
          view <- liftIO $ materializeCandidateCompilerView compilerViewDirectory
            index certifiedEnv (admittedCandidateProof candidate) canonicalSummary
          liftIO (emitCount timing "candidate_compiler_view_complete" 1)
          when (admittedCandidateLoading candidate == CandidateLoadForExecution) $ liftIO $
            emitCount timing "candidate_finalized_core_make_view" 1
          pure (ms_mod summary,view)
    canonicalViews <- fmap Map.fromList $ forM
      [(index,summary,input) | (index,ModuleNode _ summary) <- zip [0..] (mgModSummaries' loadGraph)
        , ms_hsc_src summary == HsSrcFile
        , Just input <- [canonicalOriginalFor summary]] $ \(index,summary,input) -> do
          installPreparedInterface (ms_mod_name summary) (canonicalOriginalHome input)
          view <- liftIO $ materializeCanonicalOriginalView compilerViewDirectory
            (index + Map.size candidateViews) certifiedEnv input summary
          pure (ms_mod summary,view)
    let canonicalizeLoadSummary summary =
          let original = ms_hspp_opts summary
              selected
                | ms_hsc_src summary == HsSrcFile
                , Just candidate <- Map.lookup (ms_mod_name summary) acceptedCandidates
                , ms_mod summary == ms_mod (admittedCandidateSummary candidate) =
                    candidateViews Map.! ms_mod summary
                | ms_hsc_src summary == HsSrcFile
                , Just canonical <- Map.lookup (ms_mod summary) canonicalViews = canonical
                | otherwise = summary
              flags = (canonicalizeDFlags original)
                { backend = backend original, ghcLink = ghcLink original }
          in forceFreshSource selected { ms_hspp_opts = flags }
    targetLoadFailure <- liftIO (newIORef Nothing)
    targetInstanceFailure <- liftIO (newIORef Nothing)
    canonicalFailureRef <- liftIO (newIORef Nothing)
    frontendQuasiQuotesRef <- liftIO (newIORef Map.empty)
    pendingFinalizationsRef <- liftIO (newIORef Map.empty)
    requiredLoadedOwnersRef <- liftIO (newIORef (Set.fromList
      [ms_mod summary | ModuleNode _ summary <- mgModSummaries' loadGraph, freshSource summary]))
    beforeLoad <- getSession
    let selectedMemoHomes = Map.fromList
          [(mi_module (hm_iface home),home) | home <- eltsHpt (hsc_HPT beforeLoad)
            , mi_module (hm_iface home) `Map.member` validatedMemo]
        freshIngress summary = case Map.lookup (ms_mod summary) selectedMemoHomes of
          Just home -> pure (Left home)
          Nothing -> do
            when (freshSource summary) $ liftIO $
              modifyIORef' requiredLoadedOwnersRef (Set.insert (ms_mod summary))
            pure (Right (canonicalizeLoadSummary summary))
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
        canonicalSummary summary = (restoreFreshSource summary)
          { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts (restoreFreshSource summary)) }
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
          actualNames <- readIORef actualOriginalNamesRef
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
            pure (Map.union known bindings)) (pvWorldOriginalBindings variant) (actualNames : candidateBindings ++ nativeBindings)
        retainLoaded pending iface = do
          unless (mi_module iface == ms_mod (pendingSummary pending)) $
            throwIO LoadedFinalizationOwnerMismatch
          let skinny = set_mi_extra_decls Nothing iface
          finalized <- newFinalizedModule (pendingEnvironment pending)
            (HomeModInfo skinny (pendingDetails pending) emptyHomeModInfoLinkable)
            (pendingTidyGuts pending)
          let loaded = freshLoadedModule (pendingSummary pending) (pendingFacts pending)
                (pendingOutput pending) finalized
              name = ms_mod_name (pendingSummary pending)
          -- A candidate's bytecode may be needed by a splice. Its recompilation
          -- may not supply a different interface to an importer while the old
          -- certified product remains selected downstream.
          validateCandidateInterface (pendingEnvironment pending) name skinny
          let owner = ms_mod (pendingSummary pending)
          duplicate <- atomicModifyIORef' loadedModulesRef (\known ->
            if Map.member owner known then (known, True)
            else (Map.insert owner loaded known, False))
          when duplicate (throwIO (DuplicateLoadedFinalization owner))
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
              when (ms_mod_name summary `Map.member` acceptedCandidates
                  || isJust (canonicalOriginalFor summary)) $
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
                transformed <- liftIO (pvTransformParsed variant targetName current summaryC parsed)
                ((tcg, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName current summaryC parsed
                  (typecheckNativeModuleWithDiagnostics current transformed)
                liftIO (validateCompilationFamilies current tcg)
                liftIO $ do
                  atomicModifyIORef' frontendQuasiQuotesRef (\known ->
                    (Map.insert (ms_mod summaryC) (quotes, current) known, ()))
                  when (ms_mod_name summaryC == targetName) (writeIORef targetEnvironmentRef (Just tcg))
                  reuseEvent SourceFrontend ReuseWork Absent summaryC
                  when (pvWorldOnly variant) $ emitCount timing
                    ("world.cell.source_frontends." ++ unitString (moduleUnit (ms_mod summaryC))
                      ++ ":" ++ moduleNameString (ms_mod_name summaryC)) 1
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
                  facts = ModuleFacts (moduleRefs desugared) roots
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
          | ms_hsc_src summary == HsSrcFile = timeTaskDetailPhase timing
              "compile" "source_frontend_task_service" (Just reuseContext) Nothing
              (TaskOwner (ms_mod summary) (Just (show (ms_hs_hash summary))) Nothing WholeModuleTask)
              (canonicalLoadPhase phase)
        observedCanonicalLoad phase@(T_HscPostTc _ summary (FrontendTypecheck _) _ _)
          | ms_hsc_src summary == HsSrcFile = timeTaskDetailPhase timing
              "compile" "source_finalization_task_service" (Just reuseContext) Nothing
              (TaskOwner (ms_mod summary) (Just (show (ms_hs_hash summary))) Nothing WholeModuleTask)
              (canonicalLoadPhase phase)
        observedCanonicalLoad phase = canonicalLoadPhase phase
    when (isJust originalPhaseHook) (liftIO (throwIO CustomLoadPhaseHook))
    when (isJust (hscFrontendHook (hsc_hooks beforeLoad))) (liftIO (throwIO CustomLoadFrontendHook))
    -- The outer extraction flags use NoLink, so GHC Make's LinkInMemory
    -- unload branch is inactive. This owner already selected the compatible
    -- executable epoch before planning; per-file TH backends do not change it.
    -- Make prefers its cached interface over the current summary's staged
    -- interface. An admitted original owns that choice now; remove its prior
    -- interface and linkable together. Only this cycle's selected memo
    -- interfaces may enter Make through its cache; an unowned disk interface
    -- cannot stand in for a fresh canonical finalization.
    let candidateOwners = Set.fromList
          [ms_mod (admittedCandidateSummary candidate) | candidate <- Map.elems acceptedCandidates]
        currentMemoInterfaces = Map.map (hm_iface . finalizedHomeModInfo .
          loadedFinalized . (\entry -> payloadLoaded (sourceSummaries Map.! payloadOwner (gmePayload entry)) (gmePayload entry))) validatedMemo
        sourceSummaries = Map.fromList [(ms_mod summary,summary) | ModuleNode _ summary <- mgModSummaries' modGraphRaw]
        keepCurrent iface = mi_module iface `Set.notMember` candidateOwners && case Map.lookup (mi_module iface) currentMemoInterfaces of
          Nothing -> False
          Just original -> mi_hsc_src iface == mi_hsc_src original
            && mi_iface_hash (mi_final_exts iface) == mi_iface_hash (mi_final_exts original)
        loadCache = fmap (filterModIfaceCache keepCurrent) mCache
    -- Fresh providers compile after exact hydration under the same canonical
    -- frontend/finalization owner as GHC Make. Restore only this owner's
    -- temporary fields; the plan's Core-expression link hook remains in force
    -- until its existing cpFinalEnv boundary. Direct provider ingress consumes
    -- the selected memo's refreshed HMI, a staged original or a fresh source;
    -- GHC's disk recompilation status cannot choose that owner.
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
          current <- getSession
          setSession current {hsc_mod_graph=mapMG restoreFreshSource (hsc_mod_graph current)}
          cpAfterLoad plan freshIngress
          -- Make can replace a successful source home with an interface-only
          -- details knot. World completion retains the actual finalized source
          -- owner before any declaration or native consumer observes it.
          when (pvWorldOnly variant) $ do
            finalized <- liftIO (readIORef finalizedModulesRef)
            current <- getSession
            completed <- liftIO $ foldM (\environment (name,source) -> do
              let home = finalizedHomeModInfo source
                  iface = hm_iface home
                  owner = mi_module iface
                  summaries = [summary | ModuleNode _ summary <- mgModSummaries' modGraphRaw
                    , ms_mod summary == owner, ms_hsc_src summary == HsSrcFile]
              unless (name == moduleName owner && mi_hsc_src iface == HsSrcFile
                  && moduleUnit owner == homeUnitAsUnit (hsc_home_unit environment)
                  && length summaries == 1) (throwIO WorldCellCorrupt)
              selected <- case lookupHpt (hsc_HPT environment) name of
                Nothing -> pure home
                Just previous -> do
                  unless (sameWorldCellHome previous home
                      && mi_hsc_src (hm_iface previous) == HsSrcFile) (throwIO WorldCellCorrupt)
                  pure home {hm_linkable=hm_linkable previous}
              pure (hscUpdateHPT (\table -> addToHpt table name selected) environment))
              current (Map.toList finalized)
            setSession completed
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
            (ValidationOnly (loadedFacts loaded) (loadedOutput loaded) (loadedAcquisition loaded))
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
          let targetModName' = targetName
              targetModName = moduleNameString targetModName'
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
                    transformed <- liftIO (pvTransformParsed variant targetName typecheckEnv modSum parsed)
                    ((tcg, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName typecheckEnv modSum parsed
                      (typecheckNativeModuleWithDiagnostics typecheckEnv transformed)
                    let flags = ms_hspp_opts modSum
                    liftIO (printOrThrowDiagnostics (hsc_logger typecheckEnv)
                      (initPrintConfig flags) (initDiagOpts flags) warnings)
                    familyEnvironment <- getSession
                    liftIO (validateCompilationFamilies familyEnvironment tcg)
                    pure (tcg, quotes)
                liftIO (reuseEvent SourceFrontend ReuseWork Absent modSum)
                when (pvWorldOnly variant) $ liftIO $ emitCount timing
                  ("world.cell.source_frontends." ++ unitString (moduleUnit (ms_mod modSum))
                    ++ ":" ++ moduleNameString (ms_mod_name modSum)) 1
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
                captured <- liftIO (atomicModifyIORef' loadedModulesRef (\known ->
                  (Map.delete (ms_mod summary) known, Map.lookup (ms_mod summary) known)))
                case captured of
                  Just loaded -> pure loaded
                  Nothing -> do
                    required <- liftIO (readIORef requiredLoadedOwnersRef)
                    when (ms_mod summary `Set.member` required) $
                      liftIO (throwIO MissingLoadedFinalization)
                    front <- compileFront summary
                    (_, output, finalized) <- compileBack interfaceUse front
                    facts <- liftIO (frontFacts front)
                    pure (freshLoadedModule (mfSummary front) facts output
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
              freezeSiblings observations = liftIO $ do
                let finalized = [product | observation <- observations
                      , product <- case observation of
                          LoadedObservation loaded -> [loadedFinalized loaded]
                          CachedObservation summary entry ->
                            [loadedFinalized (payloadLoaded summary (gmePayload entry))]
                          HydratedObservation{} -> []
                          CanonicalObservation{} -> []]
                    bindings = concatMap (cg_binds . finalizedTidyGuts) finalized
                    rhs (NonRec _ value) = [value]
                    rhs (Rec pairs) = map snd pairs
                    -- Imported Originals are global Ids. GHC's ordinary
                    -- exprFreeVars deliberately omits those occurrences.
                    identifiers = [identifier | binding <- bindings,value <- rhs binding
                      , identifier <- nonDetEltsUniqSet (exprSomeFreeVars isId value)]
                selected <- fmap catMaybes $ forM identifiers $ \identifier -> do
                  reference <- referenceForName (idName identifier)
                  pure ((\value -> (idName identifier,preparedRootIdentity identifier,value)) <$> reference)
                let names = Map.fromList [(name,identity) | (name,identity,_) <- selected]
                    references = Map.fromList [(identity,reference) | (_,identity,reference) <- selected]
                writeIORef actualOriginalNamesRef names
                writeIORef actualOriginalReferencesRef references
                writeIORef preparedSiblingsRef (Map.unions (map resolvePreparedSiblings
                  [cg_binds (finalizedTidyGuts product) | product <- finalized]))
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
                  recovery <- liftIO (readIORef completionRecoveryRef)
                  acquiredCanonical <- liftIO (readIORef acquiredCanonicalOriginalsRef)
                  let taskNames = Set.fromList (map fst tasks)
                      sourceOwners = Set.fromList
                        [ ms_mod summary | observation <- observations
                        , let summary = observationSummary observation
                        , ms_mod_name summary `Set.member` taskNames ]
                      -- Native availability and exact compiler Names come
                      -- from the same issued selection, including prior World
                      -- Originals. Neither projection selects runtime roots.
                      externalOriginals = Set.fromList (Map.elems externalBindings)
                      inputs = PreparedModuleCompletionInputs recovery sourceOwners siblings externalOriginals externalBindings reuseContext selectedCanonicalInterfaces
                        canonicalOriginalInputs acquiredCanonical
                  liftIO (factory (env {hsc_logger = loweringLogger}) interfaces
                    targetOwner (pvExactScope variant) inputs)
              lowerTasks preparedNormallyReusable observer tasks = liftIO $ timePhase timing "prepared_graph" $ do
                let summariesByName = Map.fromList [(ms_mod_name summary,summary) | summary <- summaries]
                    lower attempt input = do
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
                      let action = either runPreparedModuleTask pure (snd input)
                      prepared <- case Map.lookup (fst input) summariesByName of
                        Nothing -> action
                        Just summary -> timeTaskDetailPhase timing "prepared_graph" "prepared_stg_task_service"
                          (Just reuseContext) (Just attempt)
                          (TaskOwner (ms_mod summary) (Just (show (ms_hs_hash summary))) Nothing WholeModuleTask)
                          action
                      observePreparedModule observer prepared
                      pure prepared
                    lowerWith executor = runCompilerTasksWithAttempt executor lower
                      (\_ prepared -> completedPreparedModule observer prepared) tasks
                case executorFor selection of
                  Just executor -> lowerWith executor
                  Nothing -> withCompilerExecutor (const (pure ())) executionGrant lowerWith >>= finishCompletion
          canonicalEnvironmentForTasks <- getSession
          canonicalRecovery <- liftIO (readIORef completionRecoveryRef)
          canonicalPrepare <- liftIO $ case canonicalOriginalInputs of
            Nothing -> pure Nothing
            Just inputs -> do
              cache <- maybe newPreparedBodyCache (pure . compilerPreparedBodies) canonicalRecovery
              Just <$> newPreparedCanonicalModuleTaskPreparer canonicalEnvironmentForTasks cache inputs
          let prepareCanonical summary input = do
                forM_ (canonicalOriginalVersion input) $ \version -> forM_ canonicalRecovery $ \recovery ->
                  liftIO (inheritWorldCanonicalRecovery (cycleCanonicalRecovery cycleState) recovery version)
                prepare <- maybe (liftIO (throwIO WorldCellCorrupt)) pure canonicalPrepare
                siblings <- liftIO (readIORef preparedSiblingsRef)
                result <- liftIO (prepare siblings (ms_mod summary))
                (admitted,reuse,task) <- maybe (liftIO (throwIO CandidateCoreMissing)) pure result
                unless (admittedOriginalProof admitted == canonicalOriginalProof input)
                  (liftIO (throwIO CandidateOriginalOwnerMismatch))
                liftIO (modifyIORef' acquiredCanonicalOriginalsRef (Map.insert (ms_mod summary) admitted))
                liftIO (emitCount timing "world.cell.canonical_body_acquisitions" 1)
                when (reuse == PreparedBodyReused) $ liftIO
                  (emitCount timing "world.cell.canonical_body_cache_hits" 1)
                -- The selected input fixes metadata; the completed task owns
                -- executable preparation under the current site witness.
                pure (ms_mod_name summary,Left task)
              loadedCanonical summary input = do
                admitted <- liftIO (Map.lookup (ms_mod summary) <$> readIORef acquiredCanonicalOriginalsRef)
                  >>= maybe (liftIO (throwIO WorldCellCorrupt)) pure
                let finalized = admittedOriginalModule admitted
                    guts = finalizedTidyGuts finalized
                    output = ModuleOutput (ms_mod summary) (cg_binds guts) Nothing [] Nothing
                    facts = (hydratedModuleFacts (canonicalOriginalPackages input))
                      {moduleFactReferences=bindingModuleRefs (cg_binds guts)}
                pure (LoadedModule summary facts output (ReusedCanonical admitted))
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
                  Nothing | Just canonical <- canonicalOriginalFor modSum ->
                    pure (CanonicalObservation modSum canonical)
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
                    CanonicalObservation summary input
                      | nativeCanonical summary -> pure Nothing
                      | otherwise -> Just <$> prepareCanonical summary input
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
                    moduleProduct <- requireProduct loaded prepared
                    forM_ mMemoRef $ \ref -> liftIO (modifyIORef' ref
                      (Map.insert (ms_mod_name summary) (GutsMemoEntry
                        (memoValidity summary finalized)
                        (ExecutableProduct moduleProduct) requestIdentity
                        (memoDiagnosticWitnesses summary))))
                    pure (Just output, prepared)
              pairs <- forM observations' $ \observation -> case observation of
                HydratedObservation{} -> pure (Nothing, Nothing)
                CanonicalObservation summary input
                  | nativeCanonical summary -> pure (Nothing,Nothing)
                  | otherwise -> loadedCanonical summary input >>= collectPrepared
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
                  Nothing | Just canonical <- canonicalOriginalFor modSum ->
                    pure (CanonicalObservation modSum canonical)
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
                        LoadedObservation{} -> False
                        CanonicalObservation{} -> False ]
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
                      CanonicalObservation canonicalSummary input
                        | nativeCanonical canonicalSummary -> pure Nothing
                        | otherwise -> Just <$> prepareCanonical canonicalSummary input
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
                  rememberExecutable loaded prepared = do
                    let modSum = loadedSummary loaded
                        finalized = loadedFinalized loaded
                    moduleProduct <- requireProduct loaded prepared
                    case mMemoRef of
                      Just ref -> liftIO (modifyIORef' ref
                        (Map.insert (ms_mod_name modSum)
                          (GutsMemoEntry
                            (memoValidity modSum finalized)
                            (ExecutableProduct moduleProduct)
                            requestIdentity
                            (memoDiagnosticWitnesses modSum))))
                      Nothing -> pure ()
                  compileReachable loaded = do
                    rememberFinalized loaded
                    let r = loadedOutput loaded
                    prepared <- prepareReachable loaded
                    rememberExecutable loaded prepared
                    pure [(r, prepared)]
                  validationOnly loaded moduleFacts = do
                    let modSum = loadedSummary loaded
                    case mMemoRef of
                      Just ref -> liftIO (modifyIORef' ref
                        (Map.insert (ms_mod_name modSum)
                          (GutsMemoEntry
                            (memoValidity modSum (loadedFinalized loaded))
                            (ValidationOnly moduleFacts (loadedOutput loaded) (loadedAcquisition loaded))
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
                      compileReachable (payloadLoaded modSum (gmePayload entry))
                    LoadedObservation loaded -> compileReachable loaded
                    CanonicalObservation summary input
                      | nativeCanonical summary -> recordExecutableValidity summary True >> pure []
                      | otherwise -> do
                          loaded <- loadedCanonical summary input
                          compileReachable loaded
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
                    CanonicalObservation{} -> pure []
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
          moduleFacts <- liftIO (mapM observationFacts observations)
          hscFinal0 <- getSession
          hscFinal <- liftIO (includeTypedSession hscFinal0)
          setSession hscFinal
          let allBinds  = concatMap moduleOutputBinds depOutputs
                ++ moduleOutputBinds targetOutput
          targetEnvironment <- liftIO (readIORef targetEnvironmentRef) >>= maybe
            (liftIO (ioError (userError (pvLabel variant ++ ": target frontend environment is absent")))) pure
          warnings <- liftIO (nub . reverse <$> readIORef warnRef)
          injectedBaseline <- liftIO (cpInjectedSessionInterfaces plan)
          injectedTyped <- liftIO (readIORef typedSessionInterfacesRef)
          let pipelineResult = PipelineResult
                { ownedPrBinds  = allBinds
                , ownedPrHscEnv = (cpFinalEnv plan hscFinal) {hsc_logger = loweringLogger}
                , ownedPrInjectedSessionInterfaces = injectedBaseline
                , ownedPrProducedSessionInterfaces = injectedTyped
                , ownedPrCanonicalInterfaceAdmissions = selectedCanonicalInterfaces
                , ownedPrCapturedType = capturedType
                , ownedPrCheckedBinderPins = checkedBinderPins
                , ownedPrResultType   = resultTy
                , ownedPrWarnings     = nub (map fst warnings)
                , ownedPrTargetRdrEnv = tcg_rdr_env targetEnvironment
                , ownedPrTargetTcGblEnv = targetEnvironment
                }
          capturedSources <- liftIO (captureDependencySources sourceCaptureGraph)
          dependencies <- liftIO (dependencyEvidenceFor hscFinal capturedSources freshGraph
            (zip (map (ms_mod_name . observationSummary) observations) moduleFacts))
          productInterfaces <- liftIO (readIORef productInterfacesRef)
          finalizedModules <- liftIO (readIORef finalizedModulesRef)
          traceOwner <- liftIO (lookupEnv "TIDEPOOL_FINALIZED_OWNER_TRACE")
          forM_ traceOwner $ \requested -> liftIO $ do
            let key owner = (unitString (moduleUnit owner),moduleNameString (moduleName owner))
                matches owner = let (unit,name) = key owner in requested == unit ++ ":" ++ name
                summaries graph owner = [summary | ModuleNode _ summary <- mgModSummaries' graph
                  , ms_mod summary == owner]
                selectedSources = [owner | WorldSourceSelection selected _ _ _ _ <- maybe [] pure worldSources
                  , selection <- maybe [] pure selected,(owner,_,_,_) <- selectedOriginalRows selection]
                observed owner = [case observation of
                    CachedObservation _ entry -> "memo:" ++ show (gmeCycle entry)
                    LoadedObservation {} -> "loaded"
                    CanonicalObservation {} -> "canonical-input"
                    HydratedObservation {} -> "admitted-candidate"
                  | observation <- observations,ms_mod (observationSummary observation) == owner]
                sourcePaths graph owner = take 2
                  [take 256 source | summary <- summaries graph owner
                    , source <- maybe [] pure (ml_hs_file (ms_location summary))]
                selected = [mi_module (hm_iface (finalizedHomeModInfo finalized))
                  | finalized <- Map.elems finalizedModules
                  , matches (mi_module (hm_iface (finalizedHomeModInfo finalized)))]
            hPutStrLn stderr ("e6-finalized-role-request owner=" ++ show (take 256 requested)
              ++ " finalized_matches=" ++ show (length selected))
            forM_ (take 1 selected) $ \owner -> do
              let (unit,name) = key owner
                  nodes = [node | node <- dependencyModules dependencies
                    , dependencyModuleUnit node == unit,dependencyModuleName node == name]
                  paths = map dependencyModuleSource nodes
                  consumed = [source | source <- dependencySources dependencies
                    , dependencySourcePath source `elem` paths]
              hPutStrLn stderr ("e6-finalized-role owner=" ++ show (unit,name)
                ++ " fresh_graph=" ++ show (length (summaries freshGraph owner))
                ++ " source_capture_graph=" ++ show (length (summaries sourceCaptureGraph owner))
                ++ " fresh_paths=" ++ show (sourcePaths freshGraph owner)
                ++ " selected_world_source=" ++ show (key owner `elem` selectedSources)
                ++ " canonical_admission=" ++ show (Map.member (key owner) selectedCanonicalInterfaces)
                ++ " validated_memo=" ++ show (Map.member owner validatedMemo)
                ++ " observations=" ++ show (take 2 (observed owner))
                ++ " native_prepared=" ++ show (any ((== owner) . pmModule) preparedModules)
                ++ " dependency_nodes=" ++ show (length nodes)
                ++ " dependency_paths=" ++ show (map (take 256) (take 2 paths))
                ++ " consumed_sources=" ++ show (length consumed))
          let packageRoots = Map.union
                (Map.fromList [(ms_mod_name (observationSummary observation), moduleFactPackageImports facts)
                  | (observation, facts) <- zip observations moduleFacts])
                (Map.fromList [(ms_mod_name (loadedSummary loaded), moduleFactPackageImports (loadedFacts loaded))
                  | loaded <- Map.elems loadedModules])
          forM_ (Set.toAscList (Map.keysSet finalizedModules `Set.difference` Map.keysSet packageRoots)) $
            liftIO . throwIO . MissingFinalizedFacts
          pure (pipelineResult, preparedModules, dependencies, productInterfaces, finalizedModules, packageRoots)
    let compileChecked :: Bool -> Ghc (CheckedEnvironmentResult, [(ModuleName, ModuleFacts)])
        compileChecked retainPublicTarget = do
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
                  forM_ loaded $ \_ -> do
                    let facts = hydratedModuleFacts (maybe emptyPackageImports admittedCandidateRoots
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
                      transformed <- liftIO (pvTransformParsed variant targetName current summary parsed)
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
                        let linkable
                              | retainPublicTarget && isTarget = emptyHomeModInfoLinkable
                              | otherwise = maybe emptyHomeModInfoLinkable hm_linkable
                                  (lookupHpt (hsc_HPT env) (ms_mod_name summary))
                            hmi = HomeModInfo (set_mi_extra_decls Nothing iface) details linkable
                        setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name summary) hmi) ambient)
                        when timing $ liftIO $ hPutStrLn stderr $
                          "tidepool-checked-interface-retained module="
                            ++ moduleNameString (ms_mod_name summary) ++ reason
                  dependentFiles <- liftIO (readIORef (tcg_dependent_files tcg))
                  liftIO (modifyIORef' checkedFactsRef ((ms_mod_name summary, ModuleFacts
                    { moduleFactReferences = Set.empty
                    , moduleFactPackageImports = emptyPackageImports
                    , moduleFactHasDependentFiles = not (null dependentFiles)
                    , moduleFactQuasiQuoteUse = quotes }) :))
                  if retainPublicTarget && isTarget
                    then retainInterface " reason=public-module-query"
                    else case homeInterfaceUse summary laterConsumers of
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
              valid <- liftIO (revalidateAcceptedCandidates env (Map.elems acceptedCandidates))
              unless valid $ liftIO $ ioError $ userError
                "accepted metadata candidate changed before checked receipt"
              forM_ exactCompilation $ \compilation -> do
                captured <- liftIO (captureDependencySources sourceCaptureGraph)
                facts <- liftIO (readIORef checkedFactsRef)
                evidence <- liftIO (dependencyEvidenceFor env captured freshGraph facts)
                liftIO (writeCheckedExactCompilation env compilation evidence)
              let result = CheckedEnvironmentResult
                    { crHscEnv = cpFinalEnv plan env
                    , crTargetTcGblEnv = tcg
                    , crTargetRdrEnv = tcg_rdr_env tcg
                    , crInspectionProbes = probes
                    , crResultType = foldr (<|>) Nothing [capturedBindingType name tcg | name <- cpResultBinders plan]
                    , crCheckedBinderPins = capturedCellBinderPins (cpFinalEnv plan env) tcg
                    , crWarnings = map snd warnings
                    }
              facts <- liftIO (readIORef checkedFactsRef)
              pure (result,facts)
            _ -> liftIO $ ioError $ userError "metadata target missing from checked module graph"
    let finish :: PipelineSelection output -> Ghc output
        finish selected = case selected of
          PreparedStg -> do
            (result, modules, dependencies, productInterfaces, finalizedModules, packageRoots) <- compileExecutable
            capturedDependencies <- liftIO (preparedDependencies (ownedPrHscEnv result)
              variant dependencies exactCompilation)
            acquiredCanonicalOriginals <- liftIO (readIORef acquiredCanonicalOriginalsRef)
            pure PreparedPipelineResult
              { ownedPprPipelineResult = result
              , ownedPprModules = modules
              , ownedPprDependencies = capturedDependencies
              , ownedPprProductInterfaces = productInterfaces
              , ownedPprFinalizedModules = finalizedModules
              , ownedPprPackageImports = packageRoots
              , ownedPprCandidateAdmissions = []
              , ownedPprOriginalReferences = Map.empty
              , ownedPprOriginalBindings = Map.empty
              , ownedPprReuseContext = reuseContext
              , ownedPprCanonicalOriginalInputs = canonicalOriginalInputs
              , ownedPprAcquiredCanonicalOriginals = acquiredCanonicalOriginals
              }
          PreparedProducts _ -> do
            (result, modules, dependencies, productInterfaces, finalizedModules, packageRoots) <- compileExecutable
            valid <- liftIO $ revalidateAcceptedCandidates (ownedPrHscEnv result) (Map.elems acceptedCandidates)
            when (not valid) $ liftIO $ ioError $ userError
              "accepted module candidate changed before artifact publication"
            originalBindings <- liftIO (availableOriginalBindings (ownedPrHscEnv result))
            originalReferences <- liftIO (readIORef actualOriginalReferencesRef)
            capturedDependencies <- liftIO (preparedDependencies (ownedPrHscEnv result)
              variant dependencies exactCompilation)
            acquiredCanonicalOriginals <- liftIO (readIORef acquiredCanonicalOriginalsRef)
            pure PreparedPipelineResult
              { ownedPprPipelineResult = result
              , ownedPprModules = modules
              , ownedPprDependencies = capturedDependencies
              , ownedPprProductInterfaces = productInterfaces
              , ownedPprFinalizedModules = finalizedModules
              , ownedPprPackageImports = packageRoots
              , ownedPprCandidateAdmissions = Map.elems acceptedCandidates
              , ownedPprOriginalReferences = originalReferences
              , ownedPprOriginalBindings = originalBindings
              , ownedPprReuseContext = reuseContext
              , ownedPprCanonicalOriginalInputs = canonicalOriginalInputs
              , ownedPprAcquiredCanonicalOriginals = acquiredCanonicalOriginals
              }
          PreparedSegmentProducts _ candidatePath -> do
            products <- finish (PreparedProducts candidatePath)
            segment <- liftIO (readIORef targetTypedSegmentRef) >>= maybe
              (liftIO (throwIO MissingCaptureRoot)) pure
            pure (PreparedSegmentProductsResult products segment)
          CheckedEnvironment -> fst <$> compileChecked False
          CheckedEnvironmentProducts _ -> fst <$> compileChecked False
          CheckedPublicModule -> do
            (checked,facts) <- compileChecked True
            let environment = crHscEnv checked
                actualTarget = tcg_mod (crTargetTcGblEnv checked)
            home <- case lookupHpt (hsc_HPT environment) targetName of
              Just selected | mi_module (hm_iface selected) == actualTarget -> pure selected
              _ -> liftIO (fail "checked public module target interface is absent")
            captured <- liftIO (captureDependencySources sourceCaptureGraph)
            evidence <- liftIO (dependencyEvidenceFor environment captured freshGraph facts)
            unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence)
              (liftIO (throwIO (WorldModulePolicyFailure path WorldUncapturedDependency)))
            pure (CheckedPublicModuleResult home (hsc_HPT environment) evidence (crWarnings checked))
          WithCompilerExecution _ _ inner -> finish inner
          WithPreparedModuleCompletion _ inner -> finish inner
          WithTypedSegmentPreparation _ inner -> finish inner
    result <- finish selection
    liftIO $ do
      emitReuseComplete timing reuseContext SourceFrontend
      emitReuseComplete timing reuseContext Interface
      emitReuseComplete timing reuseContext FinalizedCore
      case preparation of
        PrepareStg -> pure ()
        CheckOnly -> emitCheckOnlyReuseApplicability timing reuseContext
    getSession >>= liftIO . recordCompilerInterpreter interpreterState selectedExecutables
    pure result

compilePurposeLabel :: CompilePurpose -> String
compilePurposeLabel purpose = case purpose of
  TypedSegmentCompile _ _ _ -> "typed_segment"
  GeneralCompile -> "general"
  InspectionTypeCompile -> "inspection_type"
  WorldObservationCompile -> "world_observation"
  CertifyHomeProductsCompile -> "certify_home_products"
  OriginalDeclarationCompile -> "original_declaration"
  WorldPlannedDeclarationCompile{} -> "world_original_declaration"
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

candidateInputBody :: ModuleCandidate -> FilePath -> String -> Int -> IO ArtifactBytes
candidateInputBody candidate path sha bound = case candidateInputCustody candidate of
  Just custody -> do
    body <- capturedRequestInputToken custody path sha
    when (artifactLength body > bound) (fail "captured candidate input exceeds its consumer bound")
    pure body
  Nothing -> do
    bytes <- readFileAtMost path (bound + 1)
    when (BS.length bytes > bound) (fail "fresh candidate input exceeds its artifact bound")
    pure (captureArtifactBytes bytes)

data AdmittedSourceCandidate = AdmittedSourceCandidate
  { admittedCandidateOriginal :: ModuleCandidate
  , admittedCandidateRoots :: PackageImportEvidence
  , admittedCandidateLoading :: CandidateLoading
  , admittedCandidateProof :: CanonicalInterfaceProof
  , admittedCandidateSummary :: ModSummary
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
  :: Word64 -> (HscEnv -> IO HscEnv) -> Maybe String -> Maybe ExactScope -> Set.Set ModuleName -> Maybe (Either String CapturedCandidateManifest) -> FilePath -> ModuleGraph -> ModuleName
  -> Ghc (Map.Map ModuleName AdmittedSourceCandidate)
certifyModuleCandidates requestIdentity forkContext expectedProducer exactScope sourceFreeOwners captured manifest graph targetName = do
  timing <- liftIO readTimingEnabled
  observations <- liftIO (newIORef Map.empty)
  let boundedDetail detail = case splitAt 192 detail of
        (prefix, []) -> prefix
        (prefix, _) -> take 176 prefix ++ "...[truncated]"
      recordIO owner reason detail = when timing $
        modifyIORef' observations (Map.insert owner (reason, fmap boundedDetail detail))
      record owner reason detail = liftIO (recordIO owner reason detail)
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
      let recordCandidateIO candidate = recordIO (candidateUnit candidate, candidateModule candidate)
      -- This scope contains reads and resolution only. No observation is carried
      -- into hydration, compiler work or the later publication proof.
      preflight <- liftIO $ withPackageRootObservations env $ \packageObservations ->
        fmap catMaybes $ forM candidates $ \candidate ->
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
                reason : _ -> recordCandidateIO candidate reason (admissionDetail candidate reason) >> pure Nothing
                [] -> do
                  source <- traverse canonicalizePath
                    (ml_hs_file (ms_location summary))
                  if source /= Just (candidateSource candidate)
                    then recordCandidateIO candidate CandidateSourcePath
                      (Just ("historical=" ++ show (candidateSource candidate) ++ ";current=" ++ show source))
                      >> pure Nothing
                    else do
                      inspected <- (try (sourceEvidenceWithFingerprint
                        (candidateSource candidate))
                        :: IO (Either IOException (DependencySource, Fingerprint)))
                      packageCapture <- (try (candidateInputBody candidate
                        (candidatePackageImports candidate) (candidatePackageImportsSha256 candidate) (4 * 1024 * 1024))
                        :: IO (Either IOException ArtifactBytes))
                      let packageWitness = case packageCapture of
                            Left failure -> Left (show failure)
                            Right body | artifactSha256 body /= candidatePackageImportsSha256 candidate ->
                              Left "candidate package sidecar differs from its seal"
                            Right body -> decodeCapturedPackageImports
                              (ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
                                (candidateInterface candidate) (candidateInterfaceSha256 candidate) []) (artifactBytes body)
                      originalProduct <- (try (candidateInputBody candidate
                        (candidateProductPath candidate) (candidateProductSha256 candidate) (64 * 1024 * 1024))
                        :: IO (Either IOException ArtifactBytes))
                      packageSelected <- case packageWitness of
                        Left _ -> pure False
                        Right roots -> do
                          recorded <- and <$> mapM (fmap (either (const False) (const True))
                            . validatePackageImportRootWithin packageObservations) (packageInterfaces roots)
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
                                | otherwise -> fmap Just <$> packageImportRootWithin packageObservations owner
                              _ -> pure (Left "direct import does not resolve")
                          pure $ recorded && case sequence selectedImports of
                            Left _ -> False
                            Right selected -> all (`elem` packageInterfaces roots) (catMaybes selected)
                      case (inspected, packageWitness, originalProduct) of
                        (Right (evidence, fingerprint), Right roots, Right productBytes)
                          | dependencySourceSha256 evidence /= candidateSourceSha256 candidate ->
                              recordCandidateIO candidate CandidateSourceSha Nothing >> pure Nothing
                          | fingerprint /= ms_hs_hash summary ->
                              recordCandidateIO candidate CandidateSummaryHash Nothing >> pure Nothing
                          | artifactSha256 productBytes /= candidateProductSha256 candidate ->
                              recordCandidateIO candidate CandidateNativeSha Nothing >> pure Nothing
                          | not packageSelected ->
                              recordCandidateIO candidate CandidatePackageSelection Nothing >> pure Nothing
                          | otherwise -> pure (Just (ms_mod_name summary, (candidate, summary, node, roots)))
                        (Left _, _, _) -> recordCandidateIO candidate CandidateSourceRead Nothing >> pure Nothing
                        (_, Left reason, _) -> recordCandidateIO candidate CandidatePackageRead (Just reason) >> pure Nothing
                        (_, _, Left _) -> recordCandidateIO candidate CandidateNativeRead Nothing >> pure Nothing
            (Nothing, _) -> recordCandidateIO candidate CandidateMissingSummary Nothing >> pure Nothing
            (_, Nothing) -> recordCandidateIO candidate CandidateMissingNode Nothing >> pure Nothing
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
            Right descriptors -> do
              promoted <- liftIO (captureCanonicalProofs exactScope
                [(descriptors Map.! name, if backendGeneratesCode (backend (ms_hspp_opts summary))
                    then ExecutableInterfaceUse else MetadataInterfaceUse)
                | (name,(_,summary,_,_)) <- Map.toAscList admitted])
              let proofs = Map.fromList (zip (Map.keys admitted) promoted)
              captured <- liftIO $ readVerifiedExactIfaceClosureWithCheckedValuesReader
                (\artifact -> case exactScope of
                  Just scope | artifact `elem` (originals ++ values) -> scopeInterfaceBytes scope artifact
                  _ -> case Map.lookup (mkModuleName (exactModule artifact)) proofs of
                    Just proof | artifact `elem` candidates' -> canonicalProofInterfaceBytes proof artifact
                    _ -> fail "candidate hydration leaves its captured interface closure") env
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
                          liftIO $ forM_ (Map.elems admitted) $ \(_,summary,_,_) ->
                            validateCandidateCompilerViewProfile summary
                          recordAdmitted CandidateAccepted Nothing
                          pure (Map.mapWithKey (\name (candidate, summary, _, roots) -> AdmittedSourceCandidate
                            candidate roots (if backendGeneratesCode (backend (ms_hspp_opts summary))
                              then CandidateLoadForExecution else CandidateInterfaceOnly)
                            (proofs Map.! name) summary) admitted)
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

-- Every promoted candidate shares the batch's captured owner. Observe its
-- union once, then check fresh sources and the current selected package roots.
-- No current observation survives this terminal operation.
revalidateAdmittedCandidateInputs :: HscEnv -> [AdmittedSourceCandidate] -> IO ()
revalidateAdmittedCandidateInputs env candidates = do
  revalidateCanonicalProofInputs (map admittedCandidateProof candidates) >>= either fail pure
  forM_ candidates $ \admission -> do
    let candidate = admittedCandidateOriginal admission
    (source, _) <- sourceEvidenceWithFingerprint (candidateSource candidate)
    unless (dependencySourceSha256 source == candidateSourceSha256 candidate)
      (fail "accepted module candidate source changed before artifact publication")
  let roots = Set.toAscList (Set.fromList
        (concatMap (packageInterfaces . admittedCandidateRoots) candidates))
  forM_ roots $ \root -> validatePackageImportRoot env root >>= either fail pure

revalidateAcceptedCandidates :: HscEnv -> [AdmittedSourceCandidate] -> IO Bool
revalidateAcceptedCandidates env candidates = do
  validated <- try (revalidateAdmittedCandidateInputs env candidates) :: IO (Either IOException ())
  pure (either (const False) (const True) validated)

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
sourceEvidenceGraph (Just scope) graph = sourceEvidenceGraphFor
  (Map.fromList [(mkModuleName (exactModule iface),exactUnit iface) | (iface,_,_) <- scopeInterfaces scope]) graph

sourceEvidenceGraphFor :: Map.Map ModuleName String -> ModuleGraph
  -> (ModuleGraph,[((String,String,Bool),[(DependencyQualifier,String,Bool,String)])])
sourceEvidenceGraphFor exactOwners graph =
  (mkModuleGraph (map strip nodes), map importsFor summaries)
  where
    nodes = mgModSummaries' graph
    summaries = [summary | ModuleNode _ summary <- nodes]
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
      -- GHC's finder selects imports under this request's search order.
      -- Reused summaries retain preprocessing flags from their issuing
      -- request; concatenating those flags invents a different search order.
      roots = nubOrd (importPaths (hsc_dflags env))
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

-- Issued only after this scope has parsed the recipe and matched its slots.
-- The existing phase reference identifies the issuing scope; immutable facts
-- may be observed after close, but cannot authorize another scope or allowance.
data HeldWorldCellAdmission = HeldWorldCellAdmission (IORef CompilerPhase)
  HeldWorldQueryBinding WorldCellPrepareRequest CellSourcePlan CompilerExecutionGrant CompilerExecutor
  (IORef WorldCellPrefix) (IORef (Maybe WorldCellOperandInputs))

-- Private compiler facts from successfully completed earlier stages. They grant
-- neither runtime values nor permission to publish a partial cell.
data WorldCellPrefix = WorldCellPrefix !Int HomePackageTable ModuleGraph
  (Maybe WorldPrivateCompletion) [PlannedDeclarationInventory] (SegmentDemandScope WorldDemandBinding) [LocatedImport] WorldCellStageState

data WorldCellStageState = WorldStagePending | WorldStageRendered HeldWorldCellStage
  | WorldStageCompiled HeldWorldCellStage PendingWorldDependencies | WorldStageRetry CellSourcePlan

-- The admitted authored item and its reservation survive stage rendering and
-- generated-instance retries. Generated source has separate compilation
-- provenance; it cannot replace this original input fingerprint.
data HeldWorldCellItem = HeldWorldCellItem !Int PlannedCellSlot !BS.ByteString

heldWorldCellItemOrdinal :: HeldWorldCellItem -> Int
heldWorldCellItemOrdinal (HeldWorldCellItem ordinal _ _) = ordinal

heldWorldCellItemSlot :: HeldWorldCellItem -> PlannedCellSlot
heldWorldCellItemSlot (HeldWorldCellItem _ slot _) = slot

heldWorldCellItemSourceDigest :: HeldWorldCellItem -> BS.ByteString
heldWorldCellItemSourceDigest (HeldWorldCellItem _ _ digest) = digest

data HeldWorldCellStage = HeldWorldCellStage CellSourcePlan [HeldWorldCellItem] !Int WorldRenderedStage

data WorldRenderedStage
  = WorldRenderedTyped PreparedTypedSegmentSource CompilePurpose FilePath
  | WorldRenderedOriginal PlannedDeclaration CompilePurpose FilePath

heldWorldCellStagePlan :: HeldWorldCellStage -> CellSourcePlan
heldWorldCellStagePlan (HeldWorldCellStage plan _ _ _) = plan

heldWorldCellStageItems :: HeldWorldCellStage -> [HeldWorldCellItem]
heldWorldCellStageItems (HeldWorldCellStage _ items _ _) = items

heldWorldCellStageOffset :: HeldWorldCellStage -> Int
heldWorldCellStageOffset (HeldWorldCellStage _ _ offset _) = offset

heldWorldCellStagePath :: HeldWorldCellStage -> FilePath
heldWorldCellStagePath (HeldWorldCellStage _ _ _ (WorldRenderedTyped _ _ path)) = path
heldWorldCellStagePath (HeldWorldCellStage _ _ _ (WorldRenderedOriginal _ _ path)) = path

heldWorldCellStageTypedSource :: HeldWorldCellStage -> Maybe PreparedTypedSegmentSource
heldWorldCellStageTypedSource (HeldWorldCellStage _ _ _ (WorldRenderedTyped source _ _)) = Just source
heldWorldCellStageTypedSource _ = Nothing

heldWorldCellStageOriginal :: HeldWorldCellStage -> Maybe PlannedDeclaration
heldWorldCellStageOriginal (HeldWorldCellStage _ _ _ (WorldRenderedOriginal original _ _)) = Just original
heldWorldCellStageOriginal _ = Nothing

worldCellStagePurpose :: HeldWorldCellStage -> CompilePurpose
worldCellStagePurpose (HeldWorldCellStage _ _ _ (WorldRenderedTyped _ purpose _)) = purpose
worldCellStagePurpose (HeldWorldCellStage _ _ _ (WorldRenderedOriginal _ purpose _)) = purpose

heldWorldCellQuestion :: HeldWorldCellAdmission -> WorldCellPrepareRequest
heldWorldCellQuestion (HeldWorldCellAdmission _ _ question _ _ _ _ _) = question

heldWorldCellPlan :: HeldWorldCellAdmission -> CellSourcePlan
heldWorldCellPlan (HeldWorldCellAdmission _ _ _ plan _ _ _ _) = plan

heldWorldCellSlots :: HeldWorldCellAdmission -> [(Word64,PlannedCellSlot)]
heldWorldCellSlots = worldCellPrepareSlots . heldWorldCellQuestion

-- | Transaction-issued operations share one thread/phase guard. A released
-- capability cannot access contexts retained by the worker for later requests.
data CompilerScope = CompilerScope
  { ownedScopedCompile :: ResidentCompiler
  , ownedScopedCompileCompleted :: CompletedResidentCompiler
  , ownedScopedReadExactScope :: FilePath -> IO (Either String ExactScope)
  , ownedScopedEnsureWorld :: ExactInputsRequest -> IO EnsureExactInputsOutcome
  , ownedScopedReadHeldWorld :: String -> String -> IO (Maybe HeldWorldQueryBinding)
  , ownedScopedQueryWorld :: Maybe HeldWorldQueryBinding -> CompilerExecutionGrant -> CompilerExecutor -> WorldQueryRequest -> IO BS.ByteString
  , ownedScopedPlanWorldCell :: Maybe HeldWorldQueryBinding -> CompilerExecutionGrant -> CompilerExecutor -> WorldCellPlanRequest -> IO BS.ByteString
  , ownedScopedValidateWorldCell :: CompilerExecutionGrant -> CompilerExecutor -> WorldCellPrepareRequest -> IO (Either WorldCellRefusal HeldWorldCellAdmission)
  , ownedScopedPrepareWorldCellStage :: HeldWorldCellAdmission -> IO (Maybe HeldWorldCellStage)
  , ownedScopedCompileWorldCell :: forall result. HeldWorldCellAdmission
      -> PipelineSelection result -> Set.Set SymbolIdentity
      -> IO (CompletedCompilerResult result)
  , ownedScopedCommitWorldDependencies :: HeldWorldCellAdmission -> PendingWorldDependencies -> IO ()
  , ownedScopedRunGhc :: forall result. Ghc result -> IO result
  , ownedScopedNativeProductProvenance :: IO NativeProductProvenance
  , ownedScopedParserFlags :: DynFlags
  }

-- Ordinary functions deliberately expose no record-update labels.
scopedCompile :: CompilerScope -> ResidentCompiler
scopedCompile = ownedScopedCompile

scopedNativeProductProvenance :: CompilerScope -> IO NativeProductProvenance
scopedNativeProductProvenance = ownedScopedNativeProductProvenance

scopedCompileCompleted :: CompilerScope -> CompletedResidentCompiler
scopedCompileCompleted = ownedScopedCompileCompleted

scopedReadExactScope :: CompilerScope -> FilePath -> IO (Either String ExactScope)
scopedReadExactScope = ownedScopedReadExactScope

-- Human evidence only; the existing typed acquisition result controls refusal.
-- Limit each untrusted field to 4 KiB UTF-8 BEFORE JSON escaping/allocation. Even
-- worst-case JSON escaping then stays below 74 KiB of the 16 MiB response allowance.
-- Diagnostic failure cannot replace the original rejection; cancellation still
-- follows the enclosing request's existing async/custody policy.
emitWorldAcquisitionRejection :: String -> FilePath -> String -> IO ()
emitWorldAcquisitionRejection reason manifest detail = do
  _ <- trySynchronous $ do
    let bounded = boundedDiagnosticText 4096
        message = "world input acquisition rejected: " ++ bounded reason
          ++ "\nmanifest: " ++ bounded manifest ++ "\ndetail: " ++ bounded detail
    hPutStrLn stderr (renderDiag (Diag Nothing DiagError message))
  pure ()

-- Failure evidence uses the existing bounded diagnostic channel so temporary
-- World retirement cannot erase the actual rejected source. The 64 KiB source
-- allowance bounds reporting only; it does not restrict accepted source size.
-- Even worst-case JSON escaping stays below the 16 MiB response allowance.
emitWorldObservationSourceRejection :: DynFlags -> FilePath -> BS.ByteString
  -> TypedSegmentFailure -> IO ()
emitWorldObservationSourceRejection flags path source reason = do
  _ <- trySynchronous $ do
    let extensions = [show extension | extension <- [minBound .. maxBound], xopt extension flags]
        message = "world observation source rejected: " ++ boundedDiagnosticText 4096 (moduleDemandFailureDetail reason)
          ++ "\nsource path: " ++ boundedDiagnosticText 4096 path
          ++ "\nsource bytes: " ++ show (BS.length source)
          ++ "\nsource sha256: " ++ hexBytes (SHA256.hash source)
          ++ "\neffective extensions: " ++ boundedDiagnosticText 16384 (unwords extensions)
          ++ "\ncaptured source:\n" ++ boundedDiagnosticText 65536
            (Text.unpack (TextEncoding.decodeUtf8 source))
    hPutStrLn stderr (renderDiag (Diag Nothing DiagError message))
  pure ()

moduleDemandFailureDetail :: TypedSegmentFailure -> String
moduleDemandFailureDetail failure = case failure of
  ModuleDemandParseRejected path detail -> "module parser rejected " ++ path ++ ":\n" ++ detail
  ModuleDemandQuasiQuoteRejected path names ->
    "module quasiquote policy rejected " ++ path ++ ": " ++ intercalate ", " names
  other -> show other

boundedDiagnosticText :: Int -> String -> String
boundedDiagnosticText limit value = case boundedUtf8 (toInteger limit) value of
  Just bytes -> Text.unpack (TextEncoding.decodeUtf8 bytes)
  Nothing -> prefix (limit - length truncated) value ++ truncated
  where
    truncated = "\n[diagnostic truncated]"
    prefix _ [] = []
    prefix remaining (character:rest)
      | width > remaining = []
      | otherwise = character : prefix (remaining-width) rest
      where
        point = ord character
        width | point <= 0x7f = 1
              | point <= 0x7ff = 2
              | point <= 0xffff = 3
              | otherwise = 4

scopedEnsureWorld :: CompilerScope -> ExactInputsRequest -> IO EnsureExactInputsOutcome
scopedEnsureWorld = ownedScopedEnsureWorld

scopedReadHeldWorld :: CompilerScope -> String -> String -> IO (Maybe HeldWorldQueryBinding)
scopedReadHeldWorld = ownedScopedReadHeldWorld

scopedQueryWorld :: CompilerScope -> Maybe HeldWorldQueryBinding -> CompilerExecutionGrant -> CompilerExecutor -> WorldQueryRequest -> IO BS.ByteString
scopedQueryWorld = ownedScopedQueryWorld

scopedPlanWorldCell :: CompilerScope -> Maybe HeldWorldQueryBinding -> CompilerExecutionGrant -> CompilerExecutor -> WorldCellPlanRequest -> IO BS.ByteString
scopedPlanWorldCell = ownedScopedPlanWorldCell

scopedValidateWorldCell :: CompilerScope -> CompilerExecutionGrant -> CompilerExecutor
  -> WorldCellPrepareRequest -> IO (Either WorldCellRefusal HeldWorldCellAdmission)
scopedValidateWorldCell = ownedScopedValidateWorldCell

scopedPrepareWorldCellStage :: CompilerScope -> HeldWorldCellAdmission -> IO (Maybe HeldWorldCellStage)
scopedPrepareWorldCellStage = ownedScopedPrepareWorldCellStage

-- This is an internal compiler operation on a fresh admitted prepare request,
-- never a generic Ghc callback or a second current-world/publication owner.
scopedCompileWorldCell :: CompilerScope -> HeldWorldCellAdmission
  -> PipelineSelection result -> Set.Set SymbolIdentity
  -> IO (CompletedCompilerResult result)
scopedCompileWorldCell scope = ownedScopedCompileWorldCell scope

scopedCommitWorldDependencies :: CompilerScope -> HeldWorldCellAdmission -> PendingWorldDependencies -> IO ()
scopedCommitWorldDependencies = ownedScopedCommitWorldDependencies

scopedRunGhc :: CompilerScope -> Ghc result -> IO result
scopedRunGhc scope = ownedScopedRunGhc scope

scopedParserFlags :: CompilerScope -> DynFlags
scopedParserFlags = ownedScopedParserFlags

type ResidentCompiler = forall result.
  PipelineSelection result -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope
  -> FilePath -> [FilePath] -> Maybe FilePath -> IO result

-- Issued by the same guarded operation that completes the actual compiler result.
-- Recovery references belong to that selection, independent of later operations.
data WorldCellInputs = WorldCellInputs
  { worldCellRetainedGenerations :: Map.Map SymbolIdentity Word64
  , worldCellCompilerInputReferences :: [WorldCellInputReference]
  , worldCellSourceSnapshot :: (FilePath,BS.ByteString)
  , worldCellExactImports :: [((String,String,Bool),[(DependencyQualifier,String,Bool,String)])]
  , worldCellSourceSelection :: Maybe SourceSelectedOriginals
  -- Ordered physical aliases from this completion's held source backing.
  -- They retain no lease and grant no source identity or filesystem authority.
  , worldCellSourceRootAliases :: [FilePath]
  , worldCellTypeInterfaces :: [(ExactIfaceArtifact,ArtifactBytes)]
  }

-- | The facts issued by a World compilation, always together. The completion
-- route is a sum: an ordinary completion has no World data, while a World
-- completion carries both its input basis and the one pending dependency
-- completion issued by the same request. There is no empty World shape.
data WorldCompilerCompletion = WorldCompilerCompletion WorldCellInputs PendingWorldDependencies
  (Maybe (PlannedDeclaration,PlannedDeclarationInventory))

-- A private next stage may consume only the immediately preceding actual
-- completion borrowed by its admitted compiler cursor, not a same-World sibling.
requireWorldProductPredecessor :: PreparedPipelineResult -> WorldCompilerCompletion -> IO ()
requireWorldProductPredecessor prepared previous = requireWorldProductInputs prepared (Just previous)

requireWorldProductInputs :: PreparedPipelineResult -> Maybe WorldCompilerCompletion -> IO ()
requireWorldProductInputs prepared previous = case (ownedPprDependencies prepared,previous) of
  (WorldScoped _ _ _ Nothing _,Nothing) -> pure ()
  (WorldScoped _ _ _ (Just (WorldPrivateCompletion actual _)) _,Just (WorldCompilerCompletion _ expected _))
    | samePendingWorldDependencies actual expected -> pure ()
  _ -> throwIO WorldCellCorrupt

samePendingWorldDependencies :: PendingWorldDependencies -> PendingWorldDependencies -> Bool
samePendingWorldDependencies (PendingWorldDependencies executor world reference _ _ _ pending)
    (PendingWorldDependencies issued selected realization _ _ _ expected) =
  sameCompilerExecutor executor issued && world == selected && reference == realization && pending == expected

worldCompilerCompletionInputs :: WorldCompilerCompletion -> WorldCellInputs
worldCompilerCompletionInputs (WorldCompilerCompletion inputs _ _) = inputs

worldCompilerCompletionDependencies :: WorldCompilerCompletion -> PendingWorldDependencies
worldCompilerCompletionDependencies (WorldCompilerCompletion _ pending _) = pending

worldCompilerCompletionPlannedDeclaration :: WorldCompilerCompletion -> Maybe (PlannedDeclaration,PlannedDeclarationInventory)
worldCompilerCompletionPlannedDeclaration (WorldCompilerCompletion _ _ original) = original

-- A live compiled stage retains its actual result only until the paired writer
-- seals it. The completed predecessor below retains metadata, never this handle.
data CompiledWorldStage = CompiledWorldStage PreparedPipelineResult WorldCompilerCompletion WorldInputFacts
  !WorldNativeCoordinates CompilerRecoveryCaches (IO ()) (IO ())

data WorldNativeCoordinates = WorldNativeCoordinates !BS.ByteString !BS.ByteString !BS.ByteString !Word64

withCompiledWorldStage :: CompiledWorldStage -> (PreparedPipelineResult -> WorldCompilerCompletion -> IO a) -> IO a
withCompiledWorldStage (CompiledWorldStage prepared completion _ _ _ guard _) use = guard >> use prepared completion

withCompiledWorldStageRecovery :: CompiledWorldStage
  -> (PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor -> IO a) -> IO a
withCompiledWorldStageRecovery (CompiledWorldStage prepared completion _ _ recovery guard _) use = do
  guard
  let PendingWorldDependencies executor _ _ _ _ _ _ = worldCompilerCompletionDependencies completion
  use prepared recovery executor

withCompiledWorldInputs :: CompiledWorldStage -> (WorldInputFacts -> IO a) -> IO a
withCompiledWorldInputs (CompiledWorldStage _ _ facts _ _ guard _) use = guard >> use facts

withCompiledWorldNativeCoordinates :: CompiledWorldStage
  -> (BS.ByteString -> BS.ByteString -> BS.ByteString -> Word64 -> IO a) -> IO a
withCompiledWorldNativeCoordinates (CompiledWorldStage _ _ _ (WorldNativeCoordinates admission world inputs stage) _ guard _) use =
  guard >> use admission world inputs stage

inheritCompiledWorldCanonicalRecovery :: CompiledWorldStage -> OriginalVersion -> IO ()
inheritCompiledWorldCanonicalRecovery (CompiledWorldStage _ completion _ _ recovery guard _) version = do
  guard
  let PendingWorldDependencies _ _ _ _ roots _ _ = worldCompilerCompletionDependencies completion
  inheritWorldCanonicalRecovery roots recovery version

requireSameCompiledWorldStage :: CompiledWorldStage -> CompiledWorldStage -> IO ()
requireSameCompiledWorldStage first second = withCompiledWorldStage first $ \_ actual ->
  withCompiledWorldStage second $ \_ expected ->
    unless (samePendingWorldDependencies (worldCompilerCompletionDependencies actual)
      (worldCompilerCompletionDependencies expected)) (throwIO WorldCellCorrupt)

compiledWorldStageCompletion :: CompiledWorldStage -> WorldCompilerCompletion
compiledWorldStageCompletion (CompiledWorldStage _ completion _ _ _ _ _) = completion

finishCompiledWorldStage :: CompiledWorldStage -> [OriginalVersion] -> IO WorldCompilerCompletion
finishCompiledWorldStage (CompiledWorldStage prepared completion _ _ recovery guard finish) versions = do
  guard
  let target = tcg_mod (prTargetTcGblEnv (pprPipelineResult prepared))
  evictCompilerRecovery recovery (== target)
  sealWorldCanonicalRecovery (worldCompilerCompletionDependencies completion) versions
  finish
  pure completion

data CompletedCompilerResult result
  = CompletedOrdinaryCompilerResult result CompilerRecoveryCaches (IO ())
  | CompletedWorldCompilerResult result CompilerRecoveryCaches (IO ()) WorldCompilerCompletion (Maybe CompiledWorldStage)

completedCompilerResult :: CompletedCompilerResult result -> result
completedCompilerResult (CompletedOrdinaryCompilerResult result _ _) = result
completedCompilerResult (CompletedWorldCompilerResult result _ _ _ _) = result

completedCompilerRecovery :: CompletedCompilerResult result -> CompilerRecoveryCaches
completedCompilerRecovery (CompletedOrdinaryCompilerResult _ recovery _) = recovery
completedCompilerRecovery (CompletedWorldCompilerResult _ recovery _ _ _) = recovery

completedCompilerWorldCompletion :: CompletedCompilerResult result -> Maybe WorldCompilerCompletion
completedCompilerWorldCompletion (CompletedOrdinaryCompilerResult _ _ _) = Nothing
completedCompilerWorldCompletion (CompletedWorldCompilerResult _ _ _ world _) = Just world

completedCompilerWorldStage :: CompletedCompilerResult result -> Maybe CompiledWorldStage
completedCompilerWorldStage (CompletedOrdinaryCompilerResult _ _ _) = Nothing
completedCompilerWorldStage (CompletedWorldCompilerResult _ _ _ _ stage) = stage

-- This closure retains only the issuing thread and its existing phase cell.
-- It does not keep the resident universe or a usable compiler capability alive.
completedCompilerGuard :: CompletedCompilerResult result -> IO ()
completedCompilerGuard (CompletedOrdinaryCompilerResult _ _ guard) = guard
completedCompilerGuard (CompletedWorldCompilerResult _ _ guard _ _) = guard

type CompletedResidentCompiler = forall result.
  PipelineSelection result -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope
  -> FilePath -> [FilePath] -> Maybe FilePath -> IO (CompletedCompilerResult result)

type RequestRunner = forall requestResult.
  IO () -> (ResidentCompiler -> IO requestResult) -> IO requestResult

type CompilerScopeRunner = forall requestResult.
  IO () -> (CompilerScope -> IO requestResult) -> IO requestResult

-- Metadata-only observation uses precisely the package setup of the resident
-- compiler. It has no source target and cannot run compile-time user code.
captureSupportedPackageProfile :: IO BS.ByteString
captureSupportedPackageProfile = do
  requireObservationPackageEnvironment
  libdir <- lookupEnv "TIDEPOOL_GHC_LIBDIR" >>= maybe
    (fail "package profile capture requires TIDEPOOL_GHC_LIBDIR") pure
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (gopt_set (extractionDynFlags flags []) Opt_ExternalInterpreter)
    env <- getSession
    liftIO (captureSupportedPackageProfileWith libdir env)

withResidentCompilerScopes :: [FilePath] -> (CompilerScopeRunner -> IO a) -> IO a
withResidentCompilerScopes baseIncludes useRequests = do
  packageBoot <- readCompilerPackageBoot
  producer <- captureCompilerProducerIdentity
  timing <- readTimingEnabled
  (libdir,startupMs) <- timeSection (compilerPackageBootLibdir packageBoot)
  emitPhase timing "startup" startupMs
  -- Internal interpreters cannot retire a dynamic object epoch. The selected
  -- pinned GHC closure owns this executable alongside its libraries.
  let iserv = libdir </> ".." </> "bin" </> "ghc-iserv"
  available <- doesFileExist iserv
  unless available (fail "resident compiler GHC closure lacks ghc-iserv")
  runGhc (Just libdir) $ do
    parserFlags <- getSessionDynFlags
    liftIO (validateCompilerPackageBootConfiguration packageBoot parserFlags)
    let bootstrapFlags = gopt_set (extractionDynFlags parserFlags baseIncludes) Opt_ExternalInterpreter
    _ <- setSessionDynFlags bootstrapFlags
    baseEnv <- getSession
    -- GHC initializes the target's platform constants with its package universe.
    -- Retained views must reset to that initialized profile, not the bootstrap.
    let baseFlags = hsc_dflags baseEnv
    packageAssociation <- liftIO (validateCompilerPackageBoot packageBoot libdir baseEnv)
    worldDirectory <- liftIO $ case packageAssociation of
      Nothing -> pure Nothing
      Just _ -> do
        directory <- lookupEnv "TIDEPOOL_COMPILER_WORLD_DIRECTORY" >>= maybe
          (fail "rooted compiler requires its owned world directory") pure
        unless (isAbsolute directory) (fail "compiler world directory must be absolute")
        canonical <- canonicalizePath directory
        exists <- doesDirectoryExist directory
        unless (exists && canonical == directory) (fail "compiler world directory is not its canonical owned directory")
        pure (Just directory)
    auxiliaryRecovery <- liftIO freshCompilerRecoveryCaches
    initialRecovery <- liftIO (RecoveryContext <$> RequestUnique.newUnique <*> pure auxiliaryRecovery)
    interpreter <- liftIO (newIORef (CompilerInterpreterState 0 Map.empty))
    packageFinder <- liftIO (newPackageFinderFacts baseEnv)
    originalInputs <- liftIO newExactInputOwner
    universe <- liftIO (newIORef (CompilerUniverse baseEnv Map.empty Map.empty initialRecovery interpreter packageFinder originalInputs packageAssociation emptyCompilerWorldCache worldDirectory))
    active <- liftIO (newIORef Nothing)
    availability <- liftIO (newIORef ResidentAvailable)
    ownerThread <- liftIO myThreadId
    -- This is the same freshly initialized NameCache/package owner that backs
    -- the universe. Readiness cannot precede actual package comparison.
    liftIO (emitCompilerPackageBootAcknowledgement packageAssociation)
    reifyGhc $ \session ->
      let restoreBase = reflectGhc (setSession baseEnv) session
          runRequest :: CompilerScopeRunner
          runRequest clearRecovery action =
            withCompletionRelease CompilerScopeRestore acquire release (\() -> do
            phase <- newIORef CompilerReady
            interrupted <- newIORef False
            interpreterAttempt <- newIORef InterpreterConfirmed
            operationPurpose <- newIORef "request"
            operationIdentity <- newTimingRequestIdentity >>= newIORef
            heldWorld <- newIORef Nothing
            let requireReady = do
                  caller <- myThreadId
                  unless (caller == ownerThread) (throwIO CompilerTransactionWrongThread)
                  state <- readIORef phase
                  case state of
                    CompilerReady -> pure ()
                    CompilerRunning -> throwIO CompilerTransactionBusy
                    CompilerFailed -> throwIO CompilerTransactionFailed
                    CompilerClosed -> throwIO CompilerTransactionReleased
                runGuarded :: forall result. IO result -> IO result
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
                        mutation <- readIORef interpreterAttempt
                        failures <- attemptReleases $
                          [(CompilerScopeRestore, clearRecovery)
                          ,(CompilerScopeRestore, writeIORef active Nothing)
                          ,(CompilerScopeRestore, recoverCompilerInterpreter interpreter baseEnv
                            (mutation == InterpreterMutationPending))] ++
                          (if mutation == InterpreterMutationPending then
                            [(CompilerScopeRestore, do
                                purpose <- readIORef operationPurpose
                                identity <- readIORef operationIdentity
                                emitReuse timing (ReuseContext identity purpose)
                                  NativeImage ReuseEpochRotated Recovery Nothing 1 Nothing)]
                          else []) ++ [(CompilerScopeRestore, restoreBase)]
                        when (null failures) $ do
                          writeIORef phase CompilerReady
                          writeIORef availability ResidentBusy
                        completed <- captureCompletion (throwIO failure)
                        finishCompletion (appendCleanupFailures completed failures)
                  writeIORef interpreterAttempt InterpreterConfirmed
                  writeIORef phase CompilerReady
                  pure result
                compile :: ResidentCompiler
                compile selection retained purpose mscope path includes products =
                  completedCompilerResult <$> compileCompleted selection retained purpose mscope path includes products
                compileCompleted :: CompletedResidentCompiler
                compileCompleted selection retained purpose mscope path includes products = runGuarded $ do
                  requestIdentity <- newTimingRequestIdentity
                  writeIORef operationIdentity requestIdentity
                  writeIORef operationPurpose (compilePurposeLabel purpose)
                  reflectGhc
                    (residentCompileOne requireReady producer selection retained universe active interpreterAttempt baseEnv baseFlags
                      timing requestIdentity purpose mscope path includes products) session
                readScope manifest = runGuarded $ do
                  selected <- readIORef universe
                  readExactScopeWithOwner (universeOriginalInputs selected) manifest
                readHeldWorld root reference = runGuarded $ do
                  held <- readIORef heldWorld
                  case held of
                    Just (HeldCompilerWorld facts issued _ _)
                      | root == worldInputsIdentity facts && reference == worldRealizationText issued -> do
                          let identity = worldInputsIdentity facts
                          _ <- evaluate (length identity + length (worldRealizationText issued))
                          pure (Just (HeldWorldQueryBinding identity issued))
                    _ -> pure Nothing
                ensureWorld request
                  | not (validExactInputsRequest request) = runGuarded (pure (ExactInputsRejected InvalidRealization))
                  | otherwise = ensureAdmittedWorld request
                ensureAdmittedWorld (ExactInputsRequest root acquisition) = runGuarded $ do
                  selected <- readIORef universe
                  held <- readIORef heldWorld
                  let install event world@(HeldCompilerWorld facts reference _ _) = do
                        writeIORef heldWorld (Just world)
                        observeCompilerWorld timing selected event root (worldRealizationText reference) (Just world)
                        pure (ExactInputsReady (worldInputsIdentity facts) (worldRealizationText reference)
                          (worldAdmissionResidency (optionalInputByteLimit (exactInputRetentionLimits (universeOriginalInputs selected))) world))
                      publish world@(HeldCompilerWorld facts reference _ _) = do
                        selected <- readIORef universe
                        let (nextWorlds,evicted) = admitCompilerWorld
                              (optionalInputByteLimit (exactInputRetentionLimits (universeOriginalInputs selected))) root world (universeWorlds selected)
                            retiredWorlds = removeCompilerWorld root nextWorlds
                        _ <- evaluate (toInteger (length evicted) + fst (compilerWorldCacheCharge retiredWorlds))
                        retired <- evaluate (selected {universeWorlds=retiredWorlds})
                        held <- evaluate (Just world)
                        ready <- evaluate (ExactInputsReady (worldInputsIdentity facts) (worldRealizationText reference)
                          (worldAdmissionResidency (optionalInputByteLimit (exactInputRetentionLimits (universeOriginalInputs selected))) world))
                        -- Retire old private owners before unlinking their backing.
                        -- The shared reservation remains conservative until cleanup
                        -- succeeds; failure retains it and retires this worker.
                        writeIORef universe retired
                        failures <- attemptReleases
                          [(CompilerScopeRestore, do
                              observeCompilerWorld timing retired WorldEvicted
                                (worldInputsIdentity oldFacts) (worldRealizationText oldReference) (Just old)
                              retireCompilerWorldBacking timing universe old)
                          | old@(HeldCompilerWorld oldFacts oldReference _ _) <- evicted]
                        released <- captureCompletion (pure ())
                        finishCompletion (appendCleanupFailures released failures)
                        settled <- readIORef universe
                        let (settledWorlds,unexpected) = admitCompilerWorld
                              (optionalInputByteLimit (exactInputRetentionLimits (universeOriginalInputs settled))) root world (universeWorlds settled)
                            finalNext = settled {universeWorlds=settledWorlds}
                            settledBytes = fst (compilerWorldCacheCharge settledWorlds)
                        unless (null unexpected) (throwIO WorldQueryCorrupt)
                        _ <- evaluate finalNext
                        finalResult <- evaluate (ready,world,finalNext)
                        -- No fallible or interruptible work follows decoder commit.
                        commitWorldInputRetention (universeOriginalInputs selected) settledBytes $ do
                          writeIORef universe finalNext
                          writeIORef heldWorld held
                        pure finalResult
                      conflicting = case held of
                        Just (HeldCompilerWorld facts _ _ _) -> worldInputsIdentity facts /= root
                        Nothing -> False
                  case universePackageAssociation selected of
                    Nothing -> pure (ExactInputsRejected UnsupportedOperation)
                    Just _ | conflicting -> pure (ExactInputsRejected InvalidRealization)
                    Just association -> case acquisition of
                      SuccessorExactInputs predecessor reference manifest -> do
                        let previous = case held <|> lookupCompilerWorld predecessor (universeWorlds selected) of
                              Just world@(HeldCompilerWorld facts issued _ _)
                                | worldInputsIdentity facts == predecessor && worldRealizationText issued == reference -> Just world
                              _ -> Nothing
                        case previous of
                          Nothing -> do
                            observeCompilerWorld timing selected WorldNeedsAcquisition root reference Nothing
                            pure (ExactInputsNeedsAcquisition root (if take 64 reference /= hexBytes (servicePackageWorkerInstance association) then WorldDifferentWorker else WorldNotResidentOnThisWorker))
                          Just base@(HeldCompilerWorld baseFacts _ _ _) -> do
                            acquired <- try (readWorldSuccessorWithOwner (universeOriginalInputs selected) baseFacts manifest)
                            case acquired of
                              Left (WorldInputsFailure cause detail) -> do
                                emitWorldAcquisitionRejection ("successor read/decode " ++ show cause) manifest detail
                                pure (ExactInputsRejected AcquisitionRejected)
                              Right successor
                                | worldInputsIdentity (worldSuccessorFacts successor) /= root -> do
                                    emitWorldAcquisitionRejection "successor world identity mismatch" manifest
                                      ("expected " ++ root ++ "; captured " ++ worldInputsIdentity (worldSuccessorFacts successor))
                                    pure (ExactInputsRejected InvalidRoot)
                                | otherwise -> case lookupCompilerWorld root (universeWorlds selected) of
                                    Just world -> install WorldCompleteReused world
                                    Nothing -> do
                                      issued <- newWorldRealization (servicePackageWorkerInstance association)
                                      let facts = worldSuccessorFacts successor
                                      (ready,world,committed) <- retainHeldWorldSuccessor universe base successor issued publish
                                      emitCount timing "world.successor.inherited_realizations" 1
                                      let event = case lookupCompilerWorld root (universeWorlds committed) of
                                            Just _ -> WorldAdmittedRetained
                                            Nothing -> WorldAdmittedActiveOnly
                                      observeCompilerWorld timing committed event root (worldRealizationText issued) (Just world)
                                      pure ready
                      ResidentExactInputs reference ->
                        case held of
                          Just world@(HeldCompilerWorld _ issued _ _) | worldRealizationText issued == reference -> install WorldResidentHeldHit world
                          _ -> case lookupCompilerWorld root (universeWorlds selected) of
                            Just world@(HeldCompilerWorld _ issued _ _) | worldRealizationText issued == reference -> install WorldResidentRetainedHit world
                            _ -> do
                              observeCompilerWorld timing selected WorldNeedsAcquisition root reference Nothing
                              pure (ExactInputsNeedsAcquisition root (if take 64 reference /= hexBytes (servicePackageWorkerInstance association) then WorldDifferentWorker else WorldNotResidentOnThisWorker))
                      CompleteExactInputs manifest -> do
                        acquired <- try (readWorldInputsWithOwner (universeOriginalInputs selected) manifest)
                        case acquired of
                          Left (WorldInputsFailure cause detail) -> do
                            emitWorldAcquisitionRejection ("read/decode " ++ show cause) manifest detail
                            pure (ExactInputsRejected AcquisitionRejected)
                          Right facts
                            | worldInputsIdentity facts /= root -> do
                                emitWorldAcquisitionRejection "world identity mismatch" manifest
                                  ("expected " ++ root ++ "; captured " ++ worldInputsIdentity facts)
                                pure (ExactInputsRejected InvalidRoot)
                            | worldInputsProfile facts /= servicePackageProfile association
                              || Just (CompilerProducerIdentity (worldInputsProducer facts)) /= producer -> do
                                when (worldInputsProfile facts /= servicePackageProfile association) $
                                  emitWorldAcquisitionRejection "package profile mismatch" manifest
                                    ("expected " ++ hexBytes (servicePackageProfile association)
                                      ++ "; captured " ++ hexBytes (worldInputsProfile facts))
                                when (Just (CompilerProducerIdentity (worldInputsProducer facts)) /= producer) $
                                  emitWorldAcquisitionRejection "compiler producer mismatch" manifest
                                    ("expected " ++ maybe "<unset compiler producer>" (\(CompilerProducerIdentity value) -> hexBytes value) producer
                                      ++ "; captured " ++ hexBytes (worldInputsProducer facts))
                                pure (ExactInputsRejected AcquisitionRejected)
                            | otherwise -> case held <|> lookupCompilerWorld root (universeWorlds selected) of
                                Just world -> install WorldCompleteReused world
                                Nothing -> do
                                    reference <- newWorldRealization (servicePackageWorkerInstance association)
                                    directory <- maybe (throwIO WorldQueryUnavailable) pure (universeWorldDirectory selected)
                                    (ready,world,committed) <- materializeWorldSourcesWithRelease
                                      (void . releaseRegisteredWorldMaterialization universe) directory (worldInputsSources facts)
                                      (worldInputsSourceModuleHints facts) $ \materialized -> do
                                        table <- newIORef emptyWorldPreparedModules
                                        let world = HeldCompilerWorld facts reference materialized table
                                        _ <- evaluate (length (worldRealizationText reference))
                                        current <- readIORef universe
                                        writeIORef universe current
                                          {universeWorlds=registerWorldBacking materialized (universeWorlds current)}
                                        publish world
                                    -- The unpublished-directory owner has transferred custody.
                                    -- Observation cannot make that owner unlink this world.
                                    let event = case lookupCompilerWorld root (universeWorlds committed) of
                                          Just _ -> WorldAdmittedRetained
                                          Nothing -> WorldAdmittedActiveOnly
                                        HeldCompilerWorld _ reference _ _ = world
                                    observeCompilerWorld timing committed event root (worldRealizationText reference) (Just world)
                                    pure ready
                queryWorld expected grant executor question = runGuarded $ do
                  requireCompilerExecutorOpen executor
                  held <- readIORef heldWorld
                  case (expected,held) of
                    (Just (HeldWorldQueryBinding identity reference),Just world@(HeldCompilerWorld current issued _ _))
                      | worldInputsIdentity current == identity && issued == reference
                        && worldQueryIdentity question == identity
                        && worldQueryRealization question == worldRealizationText reference -> do
                          selected <- readIORef universe
                          let requireQueryActive = do
                                caller <- myThreadId
                                unless (caller == ownerThread) (throwIO CompilerTransactionWrongThread)
                                currentPhase <- readIORef phase
                                unless (currentPhase == CompilerRunning) (throwIO CompilerTransactionReleased)
                                requireCompilerExecutorOpen executor
                                currentWorld <- readIORef heldWorld
                                case currentWorld of
                                  Just (HeldCompilerWorld facts heldReference _ _)
                                    | worldInputsIdentity facts == identity && heldReference == reference -> pure ()
                                  _ -> throwIO WorldCellUnavailable
                          bracket (reflectGhc getSession session) (\original -> reflectGhc (setSession original) session) $ \_ ->
                            reflectGhc (case worldQueryQuestion question of
                              WorldPublicModuleQuery operand -> runWorldPublicModule producer selected baseEnv timing world grant executor operand
                              WorldScopedQuery operand -> handleWorldObservationFailure operand (worldInputsIdentityBytes current)
                                (runWorldObservation requireQueryActive producer selected baseEnv timing interpreterAttempt world grant executor operand)) session
                    _ -> pure (encodeUnavailableWorldQuery question)
                planWorldCell expected _grant executor question = runGuarded $ do
                  requireCompilerExecutorOpen executor
                  -- The request retains only a scope-issued small binding. Read
                  -- the held payload after the actual phase/executor checks.
                  held <- readIORef heldWorld
                  case (expected,held) of
                    (Just (HeldWorldQueryBinding identity reference),Just (HeldCompilerWorld facts issued _ _))
                      | worldInputsIdentity facts == identity && issued == reference
                        && worldCellPlanIdentity question == identity
                        && worldCellPlanRealization question == worldRealizationText reference ->
                          planHeldWorldCell parserFlags facts (worldCellPlanRecipe question) (worldCellPlanOperand question)
                    _ -> throwIO WorldCellUnavailable
                validateWorldCell grant executor question = runGuarded $ do
                  requireCompilerExecutorOpen executor
                  held <- readIORef heldWorld
                  case held of
                    Just (HeldCompilerWorld facts issued _ _)
                      | worldCellPrepareIdentity question == worldInputsIdentity facts
                        && worldCellPrepareRealization question == worldRealizationText issued -> do
                          admitted <- validateHeldWorldCell parserFlags facts question
                          case admitted of
                            Left refusal -> pure (Left refusal)
                            Right plan -> do
                              world <- readIORef heldWorld >>= maybe (throwIO WorldCellUnavailable) pure
                              selected <- readIORef universe
                              basis <- heldWorldDemandScope selected timing world
                              lexical <- analyzeOrderedCellWithFlags parserFlags (worldCellTemplate (worldCellPrepareRecipe question))
                                (unlines (worldLexicalImportLines (worldInputsLexicalSurface facts))) >>= either (fail . renderCellSplitError) pure
                              let selectedDeclaration = case worldLexicalDeclaration (worldInputsLexicalSurface facts) of
                                    Nothing -> []
                                    Just facade -> let artifact = worldDeclarationInterface facade in
                                      [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ exactModule artifact)
                                        RetainedGeneratedImport [mkModuleName (exactModule artifact)]]
                                  inherited = map (\imported -> imported {locatedImportIntent=RetainedGeneratedImport})
                                    (prologueImports (cellPlanPrologue lexical)) ++ selectedDeclaration
                              prefix <- newIORef (WorldCellPrefix 0 emptyHomePackageTable (mkModuleGraph []) Nothing [] basis inherited WorldStagePending)
                              operand <- newIORef Nothing
                              pure (Right (HeldWorldCellAdmission phase
                                (HeldWorldQueryBinding (worldInputsIdentity facts) issued) question plan grant executor prefix operand))
                    _ -> pure (Left WorldCellUnavailable)
                prepareWorldCellStage (HeldWorldCellAdmission issuingPhase expected question plan _ executor prefixRef _) = runGuarded $ do
                  unless (issuingPhase == phase) (throwIO CheckedPurposeMismatch)
                  requireCompilerExecutorOpen executor
                  held <- readIORef heldWorld
                  case (expected,held) of
                    (HeldWorldQueryBinding identity reference,Just (HeldCompilerWorld facts issued _ _))
                      | identity == worldInputsIdentity facts && reference == issued -> do
                          WorldCellPrefix cursor homes graph dependencies inventories scope inherited state <- readIORef prefixRef
                          let stages = cellInferenceSegments plan
                              offset = sum (map (length . cellPlanItems) (take cursor stages))
                          case (state,drop cursor stages) of
                            (_,[]) -> pure Nothing
                            (WorldStageRendered stage,_) -> pure (Just stage)
                            (WorldStageCompiled _ _,_) -> throwIO CheckedPurposeMismatch
                            (_,current:_) -> do
                              let count = length (cellPlanItems current)
                                  slots = take count (drop offset (worldCellPrepareSlots question))
                                  authored = take count (drop offset (cellPlanItems plan))
                                  items = zipWith (\(ordinal,slot) item ->
                                    HeldWorldCellItem (fromIntegral ordinal) slot
                                      (SHA256.hash (TextEncoding.encodeUtf8 (Text.pack (cellAnalysisSource item)))))
                                    slots authored
                                  earlier = [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ snd (plannedOriginalOwner inventory))
                                    RetainedGeneratedImport [mkModuleName (snd (plannedOriginalOwner inventory))] | inventory <- inventories]
                                  prologue = cellPlanPrologue current
                                  base = current {cellPlanPrologue=prologue
                                    {prologueImports=inherited ++ earlier ++ prologueImports prologue}}
                                  selected = case state of WorldStageRetry retry -> retry; _ -> base
                              unless (length slots == count && length authored == count)
                                (throwIO WorldCellCorrupt)
                              stage <- renderHeldWorldCellStage parserFlags question selected items offset
                              writeIORef prefixRef (WorldCellPrefix cursor homes graph dependencies inventories scope inherited (WorldStageRendered stage))
                              pure (Just stage)
                    _ -> throwIO WorldCellUnavailable
                compileWorldCell :: forall result. HeldWorldCellAdmission
                  -> PipelineSelection result -> Set.Set SymbolIdentity
                  -> IO (CompletedCompilerResult result)
                compileWorldCell admission@(HeldWorldCellAdmission issuingPhase expected question _ grant executor prefixRef operandRef)
                    selection retained = runGuarded $ do
                  unless (issuingPhase == phase) (throwIO CheckedPurposeMismatch)
                  requireCompilerExecutorOpen executor
                  held <- readIORef heldWorld
                  case (expected,held) of
                    (HeldWorldQueryBinding identity reference,Just world@(HeldCompilerWorld facts issued _ _))
                      | worldInputsIdentity facts == identity && issued == reference
                        && worldCellPrepareIdentity question == identity
                        && worldCellPrepareRealization question == worldRealizationText reference -> do
                          -- Request binding already parsed and admitted this
                          -- immutable recipe. Consumption rechecks held-world
                          -- liveness but does not reparse the same plan.
                          prefix@(WorldCellPrefix cursor homes graph dependencies inventories scope inherited state) <- readIORef prefixRef
                          stage <- case state of
                            WorldStageRendered value -> pure value
                            _ -> throwIO CheckedPurposeMismatch
                          let current = heldWorldCellStagePlan stage
                              offset = heldWorldCellStageOffset stage
                              purpose = worldCellStagePurpose stage
                          unless (worldPurposeMatchesStage question offset current purpose) (throwIO CheckedPurposeMismatch)
                          selected <- readIORef universe
                          bracket (reflectGhc getSession session) (\original -> reflectGhc (setSession original) session) $ \_ -> do
                            result <- reflectGhc (compileHeldWorldCell requireReady producer selected baseEnv timing interpreterAttempt world grant executor
                              question operandRef prefix stage selection retained) session
                              `catch` \failure@(GeneratedInstanceRejection _ rejected) -> do
                                let retry = retryCellInstancePlan rejected current
                                when (retry /= current) $
                                  writeIORef prefixRef (WorldCellPrefix cursor homes graph dependencies inventories scope inherited (WorldStageRetry retry))
                                throwIO failure
                            completion <- maybe (throwIO WorldCellCorrupt) pure (completedCompilerWorldCompletion result)
                            writeIORef prefixRef (WorldCellPrefix cursor homes graph dependencies inventories scope inherited
                              (WorldStageCompiled stage (worldCompilerCompletionDependencies completion)))
                            prepared <- maybe (throwIO CheckedPurposeMismatch) pure
                              (worldPreparedResult selection (completedCompilerResult result))
                            let check = do
                                  requireReady
                                  requireCompilerExecutorOpen executor
                                  WorldCellPrefix _ _ _ _ _ _ _ currentState <- readIORef prefixRef
                                  case currentState of
                                    WorldStageCompiled _ actual
                                      | samePendingWorldDependencies actual (worldCompilerCompletionDependencies completion) -> pure ()
                                    _ -> throwIO CheckedPurposeMismatch
                            case result of
                              CompletedWorldCompilerResult value recovery fence owner _ -> do
                                let coordinates = WorldNativeCoordinates (worldCellPrepareAdmission question)
                                      (worldInputsIdentityBytes facts) (worldInputsCompilerIdentityBytes facts)
                                      (fromIntegral (heldWorldCellStageOffset stage))
                                    compiled = CompiledWorldStage prepared completion facts coordinates recovery check
                                      (completeWorldCellStage admission prepared completion)
                                pure (CompletedWorldCompilerResult value recovery fence owner (Just compiled))
                              _ -> throwIO WorldCellCorrupt
                    _ -> throwIO WorldCellUnavailable
                completeWorldCellStage (HeldWorldCellAdmission issuingPhase expected question _ _ executor prefixRef _) prepared completion = runGuarded $ do
                  unless (issuingPhase == phase) (throwIO CheckedPurposeMismatch)
                  requireCompilerExecutorOpen executor
                  held <- readIORef heldWorld
                  case (expected,held) of
                    (HeldWorldQueryBinding identity reference,Just (HeldCompilerWorld facts issued _ _))
                      | worldInputsIdentity facts == identity && issued == reference -> do
                          prefix@(WorldCellPrefix _ _ _ _ _ _ _ state) <- readIORef prefixRef
                          stage <- case state of
                            WorldStageCompiled selected pending
                              | samePendingWorldDependencies pending (worldCompilerCompletionDependencies completion) -> pure selected
                            _ -> throwIO CheckedPurposeMismatch
                          advanced <- advanceWorldCellPrefix executor facts question (heldWorldCellStageOffset stage)
                            (heldWorldCellStagePlan stage) prepared completion prefix
                          writeIORef prefixRef advanced
                    _ -> throwIO WorldCellUnavailable
                commitWorldDependencies (HeldWorldCellAdmission issuingPhase expected question _ _ executor _ _)
                    pending@(PendingWorldDependencies issuingExecutor _ _ _ _ _ _) = runGuarded $ do
                  unless (issuingPhase == phase && sameCompilerExecutor executor issuingExecutor)
                    (throwIO CheckedPurposeMismatch)
                  requireCompilerExecutorOpen executor
                  held <- readIORef heldWorld
                  case (expected,held) of
                    (HeldWorldQueryBinding identity reference,Just world@(HeldCompilerWorld facts issued _ _))
                      | worldInputsIdentity facts == identity && issued == reference
                        && worldCellPrepareIdentity question == identity
                        && worldCellPrepareRealization question == worldRealizationText reference ->
                          commitHeldWorldDependencies pending world
                    _ -> throwIO WorldCellUnavailable
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
                finish = do
                  writeIORef phase CompilerClosed
                  retained <- readIORef heldWorld
                  selected <- readIORef universe
                  let uncached = case retained of
                        Just world@(HeldCompilerWorld facts reference _ _) ->
                          case lookupCompilerWorld (worldInputsIdentity facts) (universeWorlds selected) of
                            Just (HeldCompilerWorld _ issued _ _) | issued == reference -> []
                            _ -> [world]
                        Nothing -> []
                  cancelled <- readIORef interrupted
                  initialFailures <- attemptReleases $
                    (if cancelled then
                      [(CompilerScopeRestore, recoverCompilerInterpreter interpreter baseEnv True)
                      ,(CompilerScopeRestore, do
                          purpose <- readIORef operationPurpose
                          identity <- readIORef operationIdentity
                          emitReuse timing (ReuseContext identity purpose)
                            NativeImage ReuseEpochRotated Recovery Nothing 1 Nothing)
                      ]
                    else []) ++
                    [(CompilerScopeRestore, writeIORef heldWorld Nothing)
                    ,(CompilerScopeRestore, writeIORef active Nothing)
                    ,(CompilerScopeRestore, restoreBase)]
                  endObservations <- attemptReleases
                    [(CompilerScopeRestore, do
                        let event
                              | not (null initialFailures) = WorldHeldEndQuarantined
                              | null uncached = WorldHeldEndRetained
                              | otherwise = WorldHeldEndRetiring
                        observeCompilerWorld timing selected event (worldInputsIdentity facts) (worldRealizationText reference) (Just world))
                    | world@(HeldCompilerWorld facts reference _ _) <- maybe [] pure retained]
                  -- Oversize/nonresident worlds remain active through actual
                  -- scope release. A failed restore keeps backing with the
                  -- worker namespace for confirmed-reap cleanup instead.
                  backingFailures <- if null initialFailures && null endObservations then attemptReleases
                    [(CompilerScopeRestore,retireCompilerWorldBacking timing universe world) | world <- uncached]
                    else pure []
                  let failures = initialFailures ++ endObservations ++ backingFailures
                  if null failures
                    then when cancelled (writeIORef availability ResidentBusy)
                    else writeIORef availability ResidentPoisoned
                  completed <- captureCompletion (pure ())
                  finishCompletion (appendCleanupFailures completed failures)
            let nativeProvenance = do
                  requireReady
                  case (producer,packageAssociation) of
                    (Just (CompilerProducerIdentity compiler),Just association) ->
                      pure (NativeProductProvenance compiler (servicePackageProfile association))
                    _ -> throwIO NativeProductConfigurationUnavailable
            withCompletionRelease CompilerScopeRestore (pure ()) (const finish)
              (const (captureCompletion (action (CompilerScope compile compileCompleted readScope ensureWorld readHeldWorld queryWorld planWorldCell validateWorldCell prepareWorldCellStage compileWorldCell commitWorldDependencies runOperation nativeProvenance parserFlags)))))
              >>= finishCompletion
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
      in withCompletionRelease CompilerScopeRestore (pure ())
        (\() -> do
          writeIORef availability ResidentClosed
          forM_ (hsc_interp baseEnv) retireCompilerInterpreter)
        (const (captureCompletion (useRequests runRequest))) >>= finishCompletion

-- | Auxiliary exact-interface operations borrow the same resident owner. Home
-- visibility starts empty; installed-package facts retain their matched closure.
withScopedExactInterfaceTransaction :: CompilerScope -> [FilePath] -> (ExactInterfaceOperations -> IO a) -> IO a
withScopedExactInterfaceTransaction compiler includes use =
  use (exactInterfaceOperations (scopedRunGhc compiler) includes)

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
  :: IO () -> Maybe CompilerProducerIdentity -> PipelineSelection result -> Set.Set SymbolIdentity
  -> IORef CompilerUniverse -> IORef (Maybe ActiveCompilerAttempt) -> IORef InterpreterAttemptState
  -> HscEnv -> DynFlags -> Bool -> Word64 -> CompilePurpose -> Maybe SessionScope
  -> FilePath -> [FilePath] -> Maybe FilePath -> Ghc (CompletedCompilerResult result)
residentCompileOne guard producer selection retained universeRef active interpreterAttempt baseEnv baseFlags timing requestIdentity purpose mscope path includes products = do
  sessionT0 <- monotonicTime
  resources <- beginResourceTiming timing
  selected <- liftIO $ case mscope of
    Just scope | isSessionScopeActive scope -> sessionVariant purpose scope
    _ -> normalVariant purpose
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
  -- Only currently admitted original nodes become initially visible. Fresh
  -- source nodes are installed after this graph's dependency validation.
  let originalNodes = Map.fromList [(owner,node)
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
        writeIORef active (Just (ActiveCompilerAttempt attempt))
        pure caches
  setSession (hscUpdateFlags flags attempt {hsc_targets=[]})
  result <- runCompileCycle selection
    (TransactionCycle cache memo candidates
      (universeSourceVersions universe) emptyWorldCanonicalRecovery
      activateRecovery (universeInterpreter universe) (universePackageFinder universe) interpreterAttempt)
    policy incarnation timing requestIdentity sessionT0 resources variant path
  final <- getSession
  targetOwner <- liftIO (targetModuleFor (hsc_mod_graph final) path)
  recovery <- liftIO (readIORef recoveryRef)
  let context = ActiveCompilerAttempt final
      recoveryContext = RecoveryContext recoveryIdentity recovery
  snapshot <- liftIO (readIORef memo)
  let admittedSnapshot = admittedSourceMemoSnapshot variant final snapshot
  -- Commit after the complete compiler operation. Failed/cancelled targets or
  -- partial dependency additions never enter this inventory.
  epoch <- liftIO (compilerInterpreterEpoch <$> readIORef (universeInterpreter universe))
  let sourceNodes = Map.fromListWith Map.union
        [(owner,Map.singleton (memoSelectionKey (gmeValidity entry))
            (Map.singleton (gmeValidity entry) (CompletedModuleVersion entry home recoveryContext epoch (maybe [] (\product' -> [product' | preparedSiteDependenciesEquivalent
                (productPrepared product') (productPrepared product')]) (payloadProduct (gmePayload entry))))))
        | entry <- Map.elems admittedSnapshot
        , let owner = payloadOwner (gmePayload entry)
        , owner /= targetOwner
        , let home = retainCompletedModuleHome (lookupHpt (hsc_HPT final) (moduleName owner))]
      originalVersions = Map.fromList [((owner,digest,localIncarnation owner),
          CompletedOriginalVersion home recoveryContext epoch)
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
    emitCount timing "original_inputs.context_variants_before" (fromIntegral (Map.size (universeOriginalVersions universe)))
    emitCount timing "original_inputs.context_variants_selected" (fromIntegral (Map.size originalVersions))
    modifyIORef' universeRef (\current -> current
      { universeEnvironment=retainedView, universeRecovery=recoveryContext
      , universeSourceVersions=Map.unionWith (Map.unionWith (Map.unionWith preferModuleVersion))
          sourceNodes (universeSourceVersions current)
      , universeOriginalVersions=if admitted then Map.unionWith preferOriginalVersion
          originalVersions (universeOriginalVersions current)
          else universeOriginalVersions current })
  pure (CompletedOrdinaryCompilerResult result recovery guard)

-- Each key owns one immutable product. Upgrade incomplete acceleration once,
-- retaining the original completed facts on ordinary hits across changing views.
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
  -- The selected libdir supplies the global database. Producer identity binds
  -- its path; package reuse requires a retained consumed-source owner.
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
  finalized <- liftIO (newFinalizedModule hscEnv hmi cgGuts)
  pure (tidyMs + ifaceMs, finalized)

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
normalVariant :: CompilePurpose -> IO PipelineVariant
normalVariant purpose = do
  let effectivePurpose = originalPurpose purpose
  case effectivePurpose of
    HostActivationPreviewCompile {} -> fail "activation preview requires its sealed session admission"
    _ -> pure ()
  pure PipelineVariant
   { pvLabel = "runPipeline"
   , pvPurpose = purpose
   , pvExactScope = Nothing
   , pvWorldOnly = False
   , pvWorldOriginalBindings = Map.empty
   , pvWorldCompletedDependencies = Nothing
   , pvWorldSources = Nothing
   , pvCompilerProducer = Nothing
   , pvGeneratedScaffold = generatedRecipe purpose
   , pvGeneratedInstanceCheck = generatedInstanceRecipe purpose
   , pvSourceImportIntents = sourceImportIntents purpose
   , pvDownsweepExcludes = []
   , pvTransformParsed = transformFor purpose
   , pvPlan = \_compilerViewDirectory _timing _targetName modGraphRaw _selectedExact -> pure CompilePlan
      { cpLoadGraph = modGraphRaw
      , cpAfterLoad = \_ -> pure ()
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

-- GHC's summary walk reaches this phase hook for every actually demanded
-- source. FileArgs parses the module's real LANGUAGE/OPTIONS before either
-- preprocessing phase can run; a frontend-only check would be too late.
withWorldModulePreprocessing :: Bool -> FilePath -> Ghc a -> Ghc a
withWorldModulePreprocessing enabled path action
  | not enabled = action
  | otherwise = reifyGhc $ \session -> bracket
      (reflectGhc install session)
      (\original -> reflectGhc (do
        current <- getSession
        setSession current {hsc_hooks=(hsc_hooks current) {runPhaseHook=original}}) session)
      (const (reflectGhc action session))
  where
    install = do
      env <- getSession
      liftIO (observeWorldHeaderFailure WorldPreprocessingEntry path (hsc_dflags env) (hsc_dflags env)
        (requireWorldModuleHeaderFlags path (hsc_dflags env)))
      let admitted = hsc_dflags env
          original = runPhaseHook (hsc_hooks env)
          delegate :: forall value. TPhase value -> IO value
          delegate phase = case original of
            Nothing -> runPhase phase
            Just (PhaseHook operation) -> operation phase
          guarded :: forall value. TPhase value -> IO value
          guarded phase = case phase of
            T_Unlit _ _ source -> throwIO (WorldModulePolicyFailure source WorldLiterateSource)
            T_Cpp _ _ source -> throwIO (WorldModulePolicyFailure source WorldPreprocessing)
            T_HsPp _ _ source _ -> throwIO (WorldModulePolicyFailure source WorldPreprocessing)
            T_FileArgs sourceEnv source -> do
              requireWorldHeaderPolicy WorldBeforeFileArguments source admitted (hsc_dflags sourceEnv)
              result@(effective,_,_) <- delegate phase
              requireWorldHeaderPolicy WorldAfterFileArguments source admitted effective
              pure result
            _ -> delegate phase
      -- Existing load admission already refuses arbitrary custom phase hooks.
      -- Refuse them before preprocessing too, rather than execute an opaque
      -- callback and only later discover that its effects were unsupported.
      when (isJust original) (liftIO (throwIO CustomLoadPhaseHook))
      setSession env {hsc_hooks=(hsc_hooks env) {runPhaseHook=Just (PhaseHook guarded)}}
      pure original

-- QuasiQuotes enables syntax; only a parsed occurrence can run its provider.
-- Header admission still precedes preprocessing and refuses every option that
-- can execute before the immutable parser proof is available.
requireWorldModuleHeaderFlags :: FilePath -> DynFlags -> IO ()
requireWorldModuleHeaderFlags path flags = do
  when (hasUnconditionallyUntrackedCompileTimeExecution flags)
    (throwIO (WorldModulePolicyFailure path WorldUntrackedOptions))
  -- GHC also records bytecode generation independently of the current backend.
  -- Keep every demanded source in the checking policy before make can invoke
  -- a compiler or read uncaptured foreign headers.
  when (backendGeneratesCode (backend flags)
      || any (`gopt` flags) [Opt_ByteCode,Opt_ByteCodeAndObjectCode])
    (throwIO (WorldModulePolicyFailure path WorldCodeGeneration))
  when (any (`gopt` flags) [Opt_WriteHie,Opt_DumpToFile,Opt_Hpc])
    (throwIO (WorldModulePolicyFailure path WorldSideOutputs))

-- Demand renaming can load interfaces before make starts. Both stages use
-- the same admitted header policy before any interface or external input use.
data WorldHeaderPhase
  = WorldAuthoredHeader | WorldPreprocessingEntry | WorldBeforeFileArguments
  | WorldAfterFileArguments | WorldDownsweepGraph | WorldCanonicalGraph
  deriving (Show)

requireWorldHeaderPolicy :: WorldHeaderPhase -> FilePath -> DynFlags -> DynFlags -> IO ()
requireWorldHeaderPolicy phase path admitted effective = observeWorldHeaderFailure phase path admitted effective $ do
  requireWorldModuleHeaderFlags path effective
  requireWorldOutputPolicy path admitted effective
  unless (importPaths effective == importPaths admitted
      && workingDirectory effective == workingDirectory admitted
      && not (packageFlagsChanged admitted effective))
    (throwIO (WorldModulePolicyFailure path WorldSearchInputsChanged))

observeWorldHeaderFailure :: WorldHeaderPhase -> FilePath -> DynFlags -> DynFlags -> IO value -> IO value
observeWorldHeaderFailure phase path admitted effective action = action `catch` \(failure :: WorldModulePolicyFailure) -> do
  emitWorldAcquisitionRejection "world header policy" path
    ("phase=" ++ show phase ++ " admitted=" ++ properties admitted
      ++ " effective=" ++ properties effective ++ " refusal=" ++ show failure)
  throwIO failure
  where
    properties flags = show
      [ ("generates-code",backendGeneratesCode (backend flags))
      , ("writes-files",backendWritesFiles (backend flags))
      , ("bytecode",gopt Opt_ByteCode flags)
      , ("bytecode-and-object",gopt Opt_ByteCodeAndObjectCode flags)
      , ("quasiquotes",xopt LangExt.QuasiQuotes flags)
      , ("template-haskell",xopt LangExt.TemplateHaskell flags)
      , ("static-pointers",xopt LangExt.StaticPointers flags)
      , ("external-preprocessor",gopt Opt_Pp flags) ]

-- Compare with the operation's fixed admitted policy, not a source environment
-- that may already contain another module's options. GHC sets dumpPrefix from
-- each filename itself; only the explicit override belongs to source policy.
requireWorldOutputPolicy :: FilePath -> DynFlags -> DynFlags -> IO ()
requireWorldOutputPolicy path admitted effective =
  unless (policy effective == policy admitted)
    (throwIO (WorldModulePolicyFailure path WorldOutputPolicyChanged))
  where
    policy flags =
      ( (hiDir flags,objectDir flags,hieDir flags,stubDir flags,dumpDir flags,hpcDir flags,temp flags)
      , (outputFile_ flags,dynOutputFile_ flags,outputHi flags,dynOutputHi flags,dumpPrefixForce flags)
      , (hiSuf_ flags,hieSuf flags,objectSuf_ flags,dynHiSuf_ flags,dynObjectSuf_ flags,hcSuf flags)
      , ghcLink flags
      , map (`gopt` flags)
          [Opt_WriteInterface,Opt_WriteIfSimplifiedCore,Opt_WriteHie,Opt_DumpToFile,Opt_Hpc
          ,Opt_KeepTmpFiles,Opt_KeepHscppFiles,Opt_KeepHcFiles,Opt_KeepSFiles,Opt_KeepLlvmFiles
          ,Opt_KeepHiFiles,Opt_KeepOFiles,Opt_BuildDynamicToo])
    temp flags = case tmpDir flags of TempDir directory -> directory

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

-- Explicit authored imports demand current canonical source proof even for a
-- captured lexical owner. An issued published root and its selected original
-- dependencies retain the same completed products;
-- reload inspection separately demands current proof for the entire original scope.
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
      reloadInspection = case scopePurpose admitted of
        ExactReloadInspectionPurpose{} -> True
        _ -> False
      publishedOwners = if reloadInspection then Set.empty else scopeCompletedSourceOriginals admitted
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
      reloadOwners = if reloadInspection then Map.keysSet (scopeSourceOriginalInterfaces admitted)
        `Set.intersection` lexicalOwners else Set.empty
      requestedOwners = reloadOwners `Set.union` Set.fromList
        [key | AuthoredSourceImport owner qualifier <- currentIntents
        , Just key <- [localOwner (renameRawPkgQual (hsc_unit_env initial) owner qualifier, noLoc owner)]
        , key `Set.notMember` publishedOwners]
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
  if null hiddenImports && Set.null reloadOwners then pure Nothing else do
    closure' <- liftIO (readScopedInterfaceClosure initial admitted
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
    let roots = Set.toAscList (reloadOwners `Set.union` Set.fromList [key | (summary,imported,key) <- hiddenImports
          , key `Set.notMember` publishedOwners
          , key `Set.member` authoredOwners || not (permitsGeneratedScaffoldImport scaffold summary key imported)])
    case roots of
      [] -> pure Nothing
      root : _ -> do
        includes <- maybe (liftIO (throwIO (ExecutionSourceUnavailable root))) pure
          (scopeIncludePaths admitted)
        -- The Finder uses this request's session roots. Unchanged summaries
        -- can retain flags from an earlier request; those historical roots
        -- neither extend nor replace the current sealed search order.
        unless (importPaths (hsc_dflags initial) == includes) $
          liftIO (throwIO (ExecutionSourceSearchChanged root (importPaths (hsc_dflags initial))))
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
              dependencyResolutionBoot resolution) `Set.member`
                (relevantImports `Set.difference` validatedOriginalExactImports validation)
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
                  , isNothing (dependencyImportSelected edge)
                  , (dependencyImportQualifier edge,dependencyImportName edge,dependencyImportBoot edge)
                      `Set.notMember` validatedOriginalExactImports validation]) }
            selected = SourceSelectedOriginals
              [(key,canonicalCertificateSha256 proof,exactSha256 (exactOwners Map.! key),canonicalSourceSha256 proof)
              | key <- Set.toAscList selectedKeys, let proof = scopeModuleInterfaceProofs admitted Map.! key] evidence
        setSession initial
        pure (Just selected)

-- Actual GHC downsweep names demand Context-selected canonical source rows.
-- Type requirements walk only their exact acquired associations. Source files
-- are resolved from the held world's closed roots and checked by the same
-- canonical validator as the ordinary exact-scope route.
withHeldWorldSourceRefusal :: Ghc value -> Ghc value
withHeldWorldSourceRefusal action = reifyGhc $ \session ->
  reflectGhc action session `catch` refuseSource `catch` refuseInput
  where
    refuseSource :: ExecutionSourceFailure -> IO value
    refuseSource failure = do
      emitWorldAcquisitionRejection "world source import" "<held captured source>" (show failure)
      throwIO $ case failure of
        ExecutionSourceUnsupported _ -> WorldCellUnsupported
        _ -> WorldCellUnavailable
    refuseInput :: IOException -> IO value
    refuseInput failure = do
      emitWorldAcquisitionRejection "world source import" "<held captured source>" (show failure)
      throwIO WorldCellUnavailable

captureHeldWorldSourceSelection :: ExactInputOwner -> WorldInputFacts -> [(ExactIfaceArtifact,ModIface)]
  -> WorldImportBasis -> WorldPrefixLexicalOwners -> ModuleGraph
  -> Ghc WorldSourceSelection
captureHeldWorldSourceSelection owner facts privateInputs imported (WorldPrefixLexicalOwners prefixOwners) graph = do
  environment <- getSession
  let ownerKey owner = (unitString (moduleUnit owner),moduleNameString (moduleName owner))
      home = unitString (homeUnitId (hsc_home_unit environment))
      -- This classifies actual lexical edges; TYPE dependencies do not
      -- themselves become imports or current-source claims.
      importedOwners = Map.fromList
        [(mkModuleName (exactModule artifact),exactUnit artifact)
        | entry <- worldImportEntries imported
        , let artifact = worldTypeEntryArtifact entry
        , exactUnit artifact == home]
      demanded = Set.fromList [ownerKey (ms_mod summary)
        | ModuleNode _ summary <- mgModSummaries' graph
        , worldHasSelectedSource facts (ownerKey (ms_mod summary))]
      unsupported = [ownerKey (ms_mod summary) | ModuleNode _ summary <- mgModSummaries' graph
        , worldHasSelectedPrimary facts (ownerKey (ms_mod summary))
        , not (worldHasSelectedSource facts (ownerKey (ms_mod summary)))]
  unless (null unsupported) $ liftIO $ do
    emitWorldAcquisitionRejection "world source import" "<held Context association>"
      ("selected primary owners lack admitted source associations (first 16): " ++ show (take 16 unsupported))
    throwIO WorldCellUnsupported
  carriedOwners <- liftIO (mergeWorldLexicalOwners [importedOwners,prefixOwners])
  if Set.null demanded then pure (WorldSourceSelection Nothing (snd (sourceEvidenceGraphFor carriedOwners graph)) [] Map.empty Nothing) else do
    initial <- getSession
    captured <- liftIO (readWorldSourceInputs owner initial facts (Set.toAscList demanded))
    (entries,typeEntries) <- case captured of
      Left reason -> do
        liftIO (emitWorldAcquisitionRejection "world source import" "<held Context association>" reason)
        liftIO (throwIO WorldCellUnsupported)
      Right inputs -> pure inputs
    let artifacts = [worldTypeEntryArtifact entry | entry <- Map.elems typeEntries] ++ map fst privateInputs
        interfaces = [(worldTypeEntryArtifact entry,worldTypeEntryInterface entry)
          | entry <- Map.elems typeEntries] ++ privateInputs
        proofs = Map.map worldSourceEntryProof entries
        readPackages key = maybe (throwIO (ExecutionSourcePackageChanged key))
          (pure . worldSourceEntryPackages) (Map.lookup key entries)
    -- The selected SOURCE roots bring their entire exact TYPE requirement
    -- closure. The shared validator derives source-less Finder locations and
    -- downsweep exclusions from every noncanonical artifact in that closure.
    -- Checked values are never promoted into a SOURCE module's TYPE closure.
    (validation,selectedKeys) <- validateCanonicalSourceInputs True (Just (map worldTypeEntryVerifiedInterface (Map.elems typeEntries))) proofs interfaces artifacts
      [] readPackages graph
      (Set.toAscList demanded)
    let selectedGraph = mkModuleGraph [node | node@(ModuleNode _ summary) <-
          mgModSummaries' (validatedOriginalGraph validation),ownerKey (ms_mod summary) `Set.member` selectedKeys]
        current = validatedOriginalEvidence validation
        modules = [node | node <- dependencyModules current
          , (dependencyModuleUnit node,dependencyModuleName node) `Set.member` selectedKeys]
    exactOwners <- liftIO $ mergeWorldLexicalOwners (carriedOwners :
      [Map.singleton (mkModuleName name) unit | (unit,name) <- Set.toAscList selectedKeys])
    let (_,allImports) = sourceEvidenceGraphFor exactOwners graph
        imports = filter (\((unit,name,_),_) -> Set.notMember (unit,name) selectedKeys) allImports
        relevantImports = Set.fromList [(dependencyImportQualifier edge,dependencyImportName edge,dependencyImportBoot edge)
          | node <- modules,edge <- dependencyModuleImports node]
          `Set.union` Set.fromList [(qualifier,name,boot) | (_,edges) <- imports,(qualifier,name,boot,_) <- edges]
        relevant resolution = (dependencyResolutionQualifier resolution,dependencyResolutionModule resolution,
          dependencyResolutionBoot resolution) `Set.member`
            (relevantImports `Set.difference` validatedOriginalExactImports validation)
    (sources,complete) <- liftIO (captureDependencySources selectedGraph)
    unless complete (liftIO (throwIO WorldCellUnavailable))
    let evidence = current
          {dependencyCacheSafe=True,dependencySelectionComplete=True,dependencySources=sources,dependencyModules=modules
          ,dependencyResolutions=filter relevant (dependencyResolutions current)
          ,dependencyPackages=Set.toAscList (Set.fromList
            [dependencyImportName edge | node <- modules,edge <- dependencyModuleImports node
            ,isNothing (dependencyImportSelected edge)
            ,(dependencyImportQualifier edge,dependencyImportName edge,dependencyImportBoot edge)
              `Set.notMember` validatedOriginalExactImports validation])}
        claims = [(key,canonicalCertificateSha256 (worldSourceEntryProof entry)
          ,exactSha256 (worldSourceEntryArtifact entry)
          ,canonicalSourceSha256 (worldSourceEntryProof entry))
          | (key,entry) <- Map.toAscList entries,key `Set.member` selectedKeys]
        selectedProofs = Map.map (ModuleInterfaceAdmission . worldSourceEntryProof)
          (Map.restrictKeys entries selectedKeys)
    closure <- maybe (liftIO (throwIO WorldCellCorrupt)) pure (validatedOriginalInputs validation)
    canonical <- liftIO $ captureCanonicalOriginalInputs (hexBytes (worldInputsProducer facts)) closure
      [(worldSourceEntryProof entry,worldSourceEntryPackages entry)
      | (key,entry) <- Map.toAscList entries,key `Set.member` selectedKeys]
    pure (WorldSourceSelection (Just (SourceSelectedOriginals claims evidence)) imports
      (Map.elems typeEntries) selectedProofs (Just canonical))

data ValidatedOriginalSources = ValidatedOriginalSources
  { validatedOriginalGraph :: ModuleGraph
  , validatedOriginalEvidence :: DependencyEvidence
  , validatedOriginalExactImports :: Set.Set (DependencyQualifier,String,Bool)
  , validatedOriginalInputs :: Maybe HydratedExactInterfaces
  }

-- Current source lookup authenticates canonical interfaces, never native
-- products or execution recipes. GHC's exact-interface compatibility check
-- retains usage/type requirements without making them lexical source imports.
validateCurrentCanonicalSources
  :: ExactScope -> [(ExactIfaceArtifact, ModIface)] -> ModuleGraph -> [(String,String)]
  -> Ghc (ValidatedOriginalSources,Set.Set (String,String))
validateCurrentCanonicalSources admitted interfaces sourceGraph roots =
  validateCanonicalSourceInputs False Nothing
    (Map.filter (isSourceOriginal . canonicalOrigin) (scopeModuleInterfaceProofs admitted))
    interfaces ([artifact | (artifact,_,_) <- scopeInterfaces admitted] ++ scopeValueInterfaces admitted)
    (map (mkModuleName . exactModule) (scopeValueInterfaces admitted)) readPackages sourceGraph roots
  where
    readPackages key = case [(artifact,path,sha) | (artifact,path,sha) <- scopeInterfaces admitted
        , (exactUnit artifact,exactModule artifact) == key] of
      [(artifact,path,sha)] -> do
        bytes <- scopeOriginalBytes admitted path sha
        either (const (throwIO (ExecutionSourcePackageChanged key))) pure
          (decodeCapturedPackageImports artifact bytes)
      _ -> throwIO (ExecutionSourceIncomplete key)

-- Both legacy exact-scope and held-world readers supply already authenticated
-- canonical facts/captured package bytes. The current-source compatibility
-- checks below have one owner; this basis grants no native execution rights.
validateCanonicalSourceInputs
  :: Bool -> Maybe [VerifiedExactInterface] -> Map.Map (String,String) CanonicalInterfaceProof -> [(ExactIfaceArtifact,ModIface)]
  -> [ExactIfaceArtifact] -> [ModuleName] -> ((String,String) -> IO PackageImportEvidence)
  -> ModuleGraph -> [(String,String)] -> Ghc (ValidatedOriginalSources,Set.Set (String,String))
validateCanonicalSourceInputs worldOnly verifiedInputs proofs interfaces inputArtifacts sourceLess readPackages sourceGraph roots = do
  initial <- getSession
  let ownerKey owner = (unitString (moduleUnit owner),moduleNameString (moduleName owner))
      originals = Map.fromList [((exactUnit artifact,exactModule artifact),iface) | (artifact,iface) <- interfaces]
      excluded = sourceLess ++ [mkModuleName (exactModule artifact) | artifact <- inputArtifacts
        , Map.notMember (exactUnit artifact,exactModule artifact) proofs]
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
  swept <- withWorldModulePreprocessing worldOnly "<canonical source qualification>" (depanalSourceModules excluded)
  graph <- if worldOnly then admitWorldQuoteFreeModuleGraph False Nothing swept else pure swept
  when worldOnly $ forM_ [summary | ModuleNode _ summary <- mgModSummaries' graph] $ \summary -> liftIO $ do
    requireWorldHeaderPolicy WorldCanonicalGraph (ms_hspp_file summary) (hsc_dflags initial) (ms_hspp_opts summary)
  env <- getSession
  current <- liftIO (dependencyEvidenceFor env ([],True) graph [])
  -- Fresh downsweep excludes admitted source-less owners. Restore only those
  -- verified exact locations after capturing ordinary source resolution;
  -- their imports still have to match the original canonical adjacency.
  let exactLocations = Map.fromList
        [((exactUnit artifact,exactModule artifact),artifact)
        | artifact <- inputArtifacts
        , Map.notMember (exactUnit artifact,exactModule artifact) proofs
        , exactUnit artifact == unitString (homeUnitId (hsc_home_unit env))]
  liftIO $ forM_ (Map.elems exactLocations) $ \artifact ->
    addHomeModuleToFinder (hsc_FC env) (hsc_home_unit env)
      (GWIB (mkModuleName (exactModule artifact)) NotBoot)
      (ms_location (exactInterfaceSummary env artifact))
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
  (native,hydratedInputs) <- liftIO $ case verifiedInputs of
    Nothing -> do
      ready <- hydrateExactScope env interfaces
      pure (ready,Nothing)
    Just verified -> do
      let keys = Set.fromList [(exactUnit artifact,exactModule artifact)
            | input <- verified,let (artifact,_) = verifiedExactInterface input]
          additional = [(artifact,iface) | (artifact,iface) <- interfaces
            , (exactUnit artifact,exactModule artifact) `Set.notMember` keys]
      (ready,completed) <- hydrateExactScopeWithProducts env verified additional
      pure (ready,Just completed)
  let nativeEnv = native {hsc_mod_graph=mapMG (\summary -> summary
        {ms_hspp_opts=canonicalizeDFlags (ms_hspp_opts summary)}) graph}
  exactImports <- fmap Set.unions $ forM (Set.toAscList selected) $ \key -> do
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
    packageProof <- liftIO (readPackages key)
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
    pure (Set.fromList [(qualifier,name,boot)
      | (qualifier,name,boot,Just unit) <- currentImports
      , Map.member (unit,name) exactLocations])
  setSession env {hsc_targets=hsc_targets initial}
  pure (ValidatedOriginalSources graph current exactImports hydratedInputs,selected)


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
      ordinaryKeys = Set.fromList (map mkNodeKey (mgModSummaries' ordinaryLoad))
      freshProviders = [summary | not (null roots)
        , node@(ModuleNode _ summary) <- flattenSCCs (topSortModuleGraph True sourceGraph Nothing)
        , mkNodeKey node `Set.notMember` ordinaryKeys
        , ms_mod_name summary /= targetName
        , backendGeneratesCode (backend (ms_hspp_opts summary))]
      admittedOwners = Set.fromList [ms_mod (retainedCompilerSummary selected) | selected <- modules]
      excludedKeys = Set.fromList [mkNodeKey (ModuleNode [] (exactInterfaceSummary initial artifact))
        | artifact <- [iface | (iface,_,_) <- scopeInterfaces admitted] ++ scopeValueInterfaces admitted
        , mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))
            `Set.notMember` admittedOwners]
      targetKeys = Set.fromList [mkNodeKey node | node@(ModuleNode _ summary) <- mgModSummaries' sourceGraph
        , ms_mod_name summary == targetName]
      blocked = reverseDependencyClosure (sourceLoadDependencyGraph sourceGraph)
        (targetKeys `Set.union` excludedKeys)
  forM_ freshProviders $ \summary -> unless
      (mkNodeKey (ModuleNode [] summary) `Set.notMember` blocked
        && null (ms_srcimps summary)
        && not (any (\case ModuleNode _ boot -> ms_hsc_src boot == HsBootFile && ms_mod boot == ms_mod summary
                           _ -> False) (mgModSummaries' sourceGraph))
        && not (xopt LangExt.StaticPointers (ms_hspp_opts summary))) $
      liftIO (throwIO (ExecutionSourceUnsupported (ownerKey (ms_mod summary))))
  pure (ExactExecutionPlan ordinaryLoad modules freshProviders (mkModuleGraph moduleNodes))

sessionVariant :: CompilePurpose -> SessionScope -> IO PipelineVariant
sessionVariant purpose scope = do
  completedValuesRef <- newIORef Nothing
  let effectivePurpose = originalPurpose purpose
      completedValues = case effectivePurpose of
        CheckedItemCompile _ _ values -> values
        ProgramItemCompile _ _ _ values -> values
        _ -> []
  exact <- case purposeExactScope purpose of
    Just admitted -> do
      -- Bind the captured input owner to this SessionScope offer. Current
      -- package selection remains environment-specific; terminal publication
      -- independently observes original-path and manifest drift.
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
   , pvWorldOnly = False
   , pvWorldOriginalBindings = Map.empty
   , pvWorldCompletedDependencies = Nothing
   , pvWorldSources = Nothing
   , pvCompilerProducer = Nothing
   , pvGeneratedScaffold = generatedRecipe purpose
   , pvGeneratedInstanceCheck = generatedInstanceRecipe purpose
   , pvSourceImportIntents = sourceImportIntents purpose
   , pvDownsweepExcludes = excludedOwners
   , pvTransformParsed = \targetModName' env summary parsed -> do
       captured <- readIORef completedValuesRef
       transformed <- transformWithCompletedValues captured purpose targetModName' env summary parsed
       case scopeRequestTypes =<< exact of
         Just (recipe, signatures) | ms_mod_name summary == targetModName' ->
           thenNativeModule transformed (rewriteRequestTypes env recipe signatures)
         _ -> pure transformed
   , pvPlan = \compilerViewDirectory timing targetModName' modGraphRaw selectedExact -> do
      current <- getSession
      let targetKeys = Set.fromList [mkNodeKey node
            | node@(ModuleNode _ summary) <- mgModSummaries' modGraphRaw
            , ms_mod_name summary == targetModName']
          excludedKeys = Set.fromList [mkNodeKey (ModuleNode [] (exactInterfaceSummary current artifact))
            | admitted <- maybe [] pure selectedExact
            , artifact <- map (\(iface,_,_) -> iface) (scopeInterfaces admitted) ++ scopeValueInterfaces admitted]
            `Set.union` Set.fromList [NodeKey_Module
                (ModNodeKeyWithUid (GWIB name NotBoot) (homeUnitId (hsc_home_unit current)))
              | name <- excludedVal]
          deferredMods = reverseDependencyClosure (sourceLoadDependencyGraph modGraphRaw)
            (targetKeys `Set.union` excludedKeys)
          depGraph = mkModuleGraph
            [node | node <- mgModSummaries' modGraphRaw, mkNodeKey node `Set.notMember` deferredMods]
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
          closure' <- liftIO (readScopedInterfaceClosure env admitted originals values)
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
        , cpAfterLoad = \freshIngress -> do
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
                     liftIO (validateCapturedAdmittedCore (retainedCompilerAdmission selectedModule))
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
                     ingress <- freshIngress summary
                     hmi <- case ingress of
                       Left selected -> pure selected
                       Right fresh -> liftIO (compileOne' (Just batchMsg) (exactHomeInstancesFor fresh current)
                         fresh 1 1 Nothing emptyHomeModInfoLinkable)
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
            when (mkNodeKey (ModuleNode [] modSum) `Set.member` deferredMods) $ do
              injected <- liftIO (readIORef injectedRef)
              let directImports = sourceImportOwners modSum
                  needed =
                    [ valueModule
                    | valueModule <- ssValIfaces scope
                    , let moduleName = renderSessionModule valueModule
                    , mkModule (homeUnitAsUnit (hsc_home_unit current)) moduleName `Set.member` directImports
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
                   verified <- liftIO (validateExactScopeEnvironment env admitted)
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
-- the effect-row sentinel, @:type@ probes, and the type-search binder:
-- all are target-local and must not be resolved back through an intentionally
-- elided target interface.
capturedInspectionProbes :: TcGblEnv -> Map.Map String Id
capturedInspectionProbes tcg = Map.fromList
  [ (occurrence, identifier)
  | identifier <- typeEnvIds (tcg_type_env tcg)
      ++ collectDataIds (tcg_binds tcg)
  , let occurrence = occNameString (nameOccName (idName identifier))
  , occurrence == "__tidepool_inspection_row"
      || occurrence == "__tidepool_type_search_query"
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
moduleRefs guts = bindingModuleRefs (mg_binds guts)

bindingModuleRefs :: [CoreBind] -> Set.Set Module
bindingModuleRefs bindings = Set.unions (map rhsModules bindings)
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
