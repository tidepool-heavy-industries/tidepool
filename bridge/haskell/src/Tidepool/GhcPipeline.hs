{-# LANGUAGE GADTs #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}

module Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), CheckedEnvironmentResult(..)
  , runPipelineSelected, runPipelineSessionSelected
  , runPipelineSelectedRetaining
  , CompilerProducerIdentity, captureCompilerProducerIdentity
  , runPipelineSessionSelectedWithProducer
  , CompilePurpose(..), PipelineResult(..)
  , generatedScaffoldRecipe
  , FinalizedModule, finalizedHomeModInfo, finalizedTidyGuts
    -- * Bound-value type analysis
  , stripMonadHead, isClosureType, renderType
  , splitTupleType
  , cellExpressionPlans, cellExpressionEvidence, cellCheckedBinderSignatures, satisfiesCapturedConstraint
  , checkCellInstances, activationPreviewInputType
  , GeneratedInstanceRecipe, generatedInstanceRecipe, cellGeneratedInstanceRecipe
  , withGeneratedInstanceRecovery
    -- * Resident session
  , withResidentPipelineSelected
  , withResidentPipelineSelectedRequests
  , withExactInterfaceTransaction
  ) where

import GHC hiding (typeKind)
import GHC.Driver.Main (hscDesugar, hscDesugar', hscSimplify, hscMaybeWriteIface, batchMsg, hscTidy, hscCompileCoreExpr')
import GHC.Driver.Pipeline (compileOne')
import GHC.Driver.Pipeline.Execute (runPhase)
import GHC.Driver.Pipeline.Phases (TPhase(..), PhaseHook(..))
import GHC.Driver.Hooks (hscCompileCoreExprHook, hscFrontendHook, runPhaseHook)
import GHC.Data.StringBuffer (stringToStringBuffer)
import qualified GHC.Data.Maybe as MaybeErr
import GHC.Driver.Backend (backendGeneratesCode, backendWritesFiles, backendCanReuseLoadedCode, noBackend)
import GHC.Driver.Env (hscUpdateFlags, hscUpdateHPT, hsc_HPT, hsc_home_unit, hscSetFlags, runHsc')
import GHC.Driver.Env.Types (HscEnv(hsc_mod_graph, hsc_unit_env, hsc_logger, hsc_dflags, hsc_FC, hsc_targets, hsc_hooks, hsc_interp))
import GHC.Driver.Monad (reflectGhc, reifyGhc, Session(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), HomeModLinkable(..), emptyHomeModInfoLinkable, addToHpt, lookupHpt)
import GHC.Unit.Module.ModDetails (ModDetails, md_types, md_insts)
import GHC.Unit.Module.Status (HscBackendAction(..))
import GHC.Types.ForeignStubs (ForeignStubs(NoStubs))
import GHC.Driver.Config.Diagnostic (initDiagOpts, initPrintConfig)
import GHC.Driver.Errors (printOrThrowDiagnostics)
import GHC.Types.Avail (availNames)
import GHC.Iface.Load (loadInterface, WhereFrom(..))
import GHC.Rename.Names (renameRawPkgQual)
import GHC.Driver.Make (load', ModIfaceCache, newIfaceCache)
import qualified GHC.Linker.Loader as Linker
import GHC.Iface.Make (mkIfaceTc, mkPartialIface, mkFullIface)
import GHC.Iface.Recomp (MaybeValidated(..), checkOldIface)
import GHC.Unit.Finder (initFinderCache)
import GHC.Unit.Module.ModIface (set_mi_extra_decls)
import GHC.Unit.Module.Deps (imp_mods, Usage(..))
import GHC.Unit.Finder (FindResult(Found), findImportedModule)
import GHC.Iface.Tidy (mkBootModDetailsTc)
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.Error (MessageClass(..), mkLocMessage, getMessages, errMsgDiagnostic, unionMessages)
import GHC.Types.SourceError (SourceError, srcErrorMessages)
import GHC.Driver.Errors.Types (GhcMessage(..))
import GHC.Tc.Errors.Types (TcRnMessage(..), TcRnMessageDetailed(..), DeriveInstanceErrReason(..))
import GHC.Utils.Logger (LogAction)
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), InputRejection(..), DependencyLoadFailure(..), dependencyDiagnostic, spanOf)
import GHC.Data.FastString (unpackFS, mkFastString)
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Unit.Module.Graph (mgModSummaries', ModuleGraphNode(..), mkNodeKey, nodeDependencies)
import GHC.Unit.Home (homeUnitId, isHomeUnit)
import GHC.Unit.Types (unitString)
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Driver.Session
  ( updOptLevel, gopt_set, gopt_unset, xopt
  , WarningFlag
      ( Opt_WarnMissingFields
      , Opt_WarnIncompletePatterns
      , Opt_WarnIncompleteUniPatterns
      )
  , wopt_set
  , PackageFlag(..), PackageArg(..), ModRenaming(..) )
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
  ( renderWithContext, defaultSDocContext, ppr, SDocContext(..)
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
import GHC.Core.InstEnv (is_cls, is_tys, instEnvElts)
import GHC.Core.FamInstEnv (fi_fam, fi_tys)
import GHC.Core.Predicate (mkClassPred, getClassPredTys_maybe)
import GHC.Builtin.Names (gHC_PRIM, fUNTyConKey, unrestrictedFunTyConKey, genClassKey, repTyConKey)
import GHC.Builtin.Types (zonkAnyTyCon)
import GHC.Core.DataCon (dataConOrigArgTys)
import GHC.Data.Bag (listToBag)
import GHC.Tc.Solver (tcCheckGivens, tcCheckWanteds)
import GHC.Tc.Solver.InertSet (emptyInert)
import GHC.Tc.Utils.Monad (initTcWithGbl, initIfaceCheck)
import GHC.Tc.Utils.TcMType (newEvVars)
import GHC.Core.TyCo.Rep (Scaled(..), Type(..))
import GHC.Types.Unique.Set (UniqSet, emptyUniqSet, addOneToUniqSet, elementOfUniqSet)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.TypeEnv (typeEnvIds, typeEnvTyCons)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Tc.Types (FrontendResult(..), TcGblEnv, tcg_th_coreplugins, tcg_import_decls, tcg_dependent_files, tcg_binds, tcg_rdr_env, tcg_type_env, tcg_imports, tcg_keep)
import GHC.Types.Name.Set (extendNameSetList)
import GHC.Types.Name.Reader (GlobalRdrEnv, rdrNameOcc)
import GHC.Types.Name.Ppr (mkNamePprCtx)
import GHC.Types.Name (nameOccName, nameUnique, mkExternalName, mkInternalName, nameModule_maybe)
import GHC.Types.Name.Occurrence (OccName, mkOccName, mkTyVarOcc, occNameSpace, occNameString, isTcOcc)
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
import Data.Maybe (fromMaybe, isJust, isNothing, catMaybes)
import Data.List (find, isPrefixOf, nub, nubBy, sort, sortOn, intercalate)
import Data.Containers.ListUtils (nubOrd)
import Data.IORef (IORef, atomicModifyIORef', newIORef, modifyIORef', readIORef, writeIORef)
import Numeric (showHex)
import System.Environment (lookupEnv)
import System.FilePath (takeBaseName, takeFileName, normalise, pathSeparator, (</>))
import System.Directory (canonicalizePath, makeAbsolute, doesFileExist, getModificationTime, getTemporaryDirectory)
import System.IO (hPutStrLn, stderr, readFile', IOMode(ReadMode), withBinaryFile)
import Control.Monad.IO.Class (liftIO)
import Control.Monad (forM, forM_, when, unless, filterM)
import Data.Data (Data, cast, gmapQ)
import Data.Generics (everything, mkQ)
import Data.Foldable (toList)
import Data.Word (Word64)
import Tidepool.Binders (CheckedBinderPin(..), CellSourcePlan(..), CellGenericDeclaration(..), CellStructuralDisplayTarget(..), CellExpressionPlan(..), ExpressionLiftPlan(..), ExpressionPresentation(..), omitCellGenericDeclarations, omitCellStructuralDisplayDeclarations)
import Tidepool.CheckedCell (CheckedSignature, captureCheckedSignature, rewriteCheckedAnnotations, rewriteHostInputType, rewriteRequestTypes
  , NativeParsedModule, unannotatedModule, mapNativeModule, thenNativeModule, typecheckNativeModuleWithDiagnostics)
import Tidepool.FinalizedModule (FinalizedModule(..))
import Tidepool.HomeProducts
  ( hydrateCandidateHomeProductsWithOriginals, hydrateCandidateExecutable
  , validateCandidateInterfaceRequirements )
import Tidepool.CompileInputPolicy (pluginInputIssues)
import Tidepool.PlannedDeclaration
  ( PlannedDeclarationInventory, transformPlannedDeclarationImports, transformPlannedDeclarationImportsWithCompleted, transformProgramDeclarationImports, hydratePlannedDeclarationInventory )
import Tidepool.CheckedPrefixImports
  ( CompletedValueImport(..), CompletedValueImports, hydrateCompletedValueImportsWithVerifiedDependencies, transformCompletedValueImports, selectedImportNames )
import Tidepool.FamilyConsistency (validateCompilationFamilies, validateEnvironmentFamilies)
import Tidepool.TypePolicy (nominalHeadsOfType, stabilizeEffectRows)
import Tidepool.ExtractUtil (getLibdir, capitalize)
import Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences)
import Tidepool.Introspection (normalizeLookupWildcards)
import Tidepool.Session
  ( SessionModule(..), SessionModuleKind(..), SessionScope(..)
  , isSessionScopeActive, injectSessionScope, registerSessionInterfaceLocation, renderSessionModule
  , scaffoldTargetName, scaffoldOutputBase, evalUserBinder, parseSessionModule )
import Tidepool.Timing
  ( readTimingEnabled, timeSection, timePhase, emitPhase, emitCount
  , timeDetailPhase, ResourceTimingStart, beginResourceTiming, endResourceTiming
  , monotonicTime, elapsedMs
  , emitCompileSummary, emitModuleTiming, emitModuleInterfaceTiming
  , InterfaceStage(..), InterfaceReuse(..), measureModuleInterface
  , newTimingRequestIdentity
  , readMemoTraceEnabled, emitMemoCycleGraph, emitMemoMissTrace )
import Tidepool.PreparedStg (PreparedElaboration(..), PreparedModule(..), prepareModule)
import Tidepool.PreparedSites
  ( elaboratePreparedSites, resolvePreparedSiblings, resolvePreparedInterfaceSiblings, resolveSiteAuthority
  , siteAuthorityEffectRequestTypeIds )
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.RetainedUnfoldings
  ( RetainedContext, retainedContext, emptyRetainedContext
  , installRetainedUnfoldingsPlugin, retainedDefinedBy
  , scopeRetainedModuleGraph, scopeRetainedHscEnv )
import Tidepool.TurnSource (extractModuleName)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyResolution(..)
  , DependencyModule(..), DependencyImport(..), DependencyQualifier(..), ProductAvailability(..)
  , sourceEvidenceWithFingerprint )
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), freshExactState, hydrateExactScope, serializeOriginalInterface
  , readVerifiedExactIfaceClosureWithCheckedValues, selectVerifiedExactInterfaces, selectVerifiedValueInterfaces
  , checkedValueImportAuthorityFromVerified
  , GeneratedScaffoldRecipe, generatedScaffoldRecipe, captureGeneratedScaffoldTarget
  , noGeneratedScaffoldImports, readGeneratedScaffoldImportAuthority, permitsGeneratedScaffoldImport, installExactLexicalGraphWithScaffold )
import Tidepool.ExactScope
  ( ExactScope(..), ExactCompilation(..), SourceSelectedOriginals(..), extendSourceSelectedOriginals, CheckedCellAdmission(..), CheckedCellPurpose(..), CheckedItemAdmission(..), CheckedItemPurpose(..), readExactScope, revalidateExactScope, scopeValueInterfaces
  , writeExactCompilation, scopeExecutionNativeOwners )
import Tidepool.ExactScope
  ( CanonicalInterfaceProof, validateCandidateCanonicalInterfaceProof
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalCoreArtifact
  , canonicalCorePath, canonicalCoreSha256 )
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceNode(..), ExecutionSourceIdentity(..)
  , ExecutionSourceFailure(..), ExecutionSourceRef(..), executionSourceClosure, executionIdentityKey )
import Tidepool.PackageWitness
  ( PackageImportRoot(..), PackageImportEvidence(..), CompilerProvidedImport(..), emptyPackageImports, packageImportRoot, readPackageImports
  , validatePackageImportRoot )
import Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateImport(..), CandidateQualifier(..)
  , readModuleCandidatesWithGraphs, candidateExecutionSources, candidateOriginalIdentity )

-- | Selects the compiler representation produced at the internal GHC API
-- boundary. Metadata consumers stop at the checked environment.
data PipelineSelection result where
  PreparedStg :: PipelineSelection PreparedPipelineResult
  PreparedProducts :: Maybe FilePath -> PipelineSelection PreparedPipelineResult
  CheckedEnvironment :: PipelineSelection CheckedEnvironmentResult
  CheckedEnvironmentProducts :: FilePath -> PipelineSelection CheckedEnvironmentResult

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
  , pprDependencies :: DependencyEvidence
  , pprProductInterfaces :: Map.Map ModuleName ModIface
  , pprFinalizedModules :: Map.Map ModuleName FinalizedModule
  , pprPackageImports :: Map.Map ModuleName PackageImportEvidence
  , pprAcceptedCandidates :: [ModuleCandidate]
  , pprExactCompilation :: Maybe ExactCompilation
  }

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
selectionKind CheckedEnvironment = CheckOnly
selectionKind (CheckedEnvironmentProducts _) = CheckOnly

capturesProductInterfaces :: PipelineSelection result -> Bool
capturesProductInterfaces (PreparedProducts _) = True
capturesProductInterfaces _ = False

data PipelineResult = PipelineResult
  { prBinds  :: [CoreBind]
  , prTyCons :: [TyCon]
  , prHscEnv :: HscEnv
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
  :: (CellSourcePlan -> IO CheckedEnvironmentResult)
  -> CellSourcePlan
  -> IO (CellSourcePlan, CheckedEnvironmentResult)
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
        , expressionPlanPresentation = ExpressionOpaque
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

-- The input witness comes from the independently typechecked preview binder,
-- rather than the placeholder result or a supplied rendered type.
activationPreviewInputType :: TcGblEnv -> Either String Type
activationPreviewInputType environment = do
  preview <- unique "__activationPreview"
  checked <- unique "__result"
  case splitFunTys preview of
    ([Scaled _ input], _) | eqType input (stripMonadHead checked) -> Right input
    ([Scaled _ _], _) -> Left "activation preview input differs from its checked binder type"
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
  , pvExactScope :: Maybe ExactScope
    -- ^ Admitted immutable declaration owners, independent of live values.
  , pvCompilerProducer :: Maybe CompilerProducerIdentity
  , pvGeneratedScaffold :: Maybe GeneratedScaffoldRecipe
    -- ^ The protected target and its compiler-owned support import.
  , pvDownsweepExcludes :: [ModuleName]
    -- ^ Modules @depanal@ must NOT try to summarise (the session path's
    -- source-less @Val.G\<g\>@ ifaces). Empty on the normal path.
  , pvPlan :: Bool -> ModuleGraph -> Maybe ExactScope -> Ghc CompilePlan
    -- ^ @pvPlan timingEnabled downsweepGraph selectedExactScope@.
  , pvGeneratedInstanceCheck :: Maybe GeneratedInstanceRecipe
  , pvTransformParsed :: HscEnv -> ModSummary -> ParsedModule -> IO NativeParsedModule
  }

candidateManifestFor :: PipelineSelection result -> Maybe FilePath
candidateManifestFor (PreparedProducts path) = path
candidateManifestFor (CheckedEnvironmentProducts path) = Just path
candidateManifestFor _ = Nothing

compilerProducerFor :: PipelineVariant -> Maybe String
compilerProducerFor variant = case pvCompilerProducer variant of
  Just (CompilerProducerIdentity producer) -> Just producer
  Nothing -> scopeProducerSha256 <$> pvExactScope variant

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

data ResidentStateOrigin = OrdinarySourceState | LegacySourceFreeState | ExactState
  deriving (Eq)

residentStateOrigin :: PipelineSelection result -> PipelineVariant -> ResidentStateOrigin
residentStateOrigin selection variant
  | exactCompileCycle selection variant = ExactState
  | not (null (pvDownsweepExcludes variant)) = LegacySourceFreeState
  | otherwise = OrdinarySourceState

data CompilePurpose = GeneralCompile | LookupTypeCompile | CertifyHomeProductsCompile | OriginalDeclarationCompile
  | CheckedItemCompile [(String,CheckedSignature)] (Maybe ((String,String),String)) [CompletedValueImport]
  | HostActivationCheck CheckedSignature
  | HostActivationInputCompile [(String,CheckedSignature)] (Maybe ((String,String),String)) [CompletedValueImport]
  | ProgramItemCompile Bool [(String,CheckedSignature)] [((String,String),String)] [CompletedValueImport]
  | PlannedDeclarationCheck PlannedDeclarationInventory ExactScope
  | CellProgramCompile CompilePurpose ExactScope
  | GeneratedScaffoldCompile GeneratedScaffoldRecipe CompilePurpose
  | GeneratedInstanceCheck GeneratedInstanceRecipe CompilePurpose
  deriving (Eq, Show)

generatedInstanceRecipe :: CompilePurpose -> Maybe GeneratedInstanceRecipe
generatedInstanceRecipe (GeneratedInstanceCheck recipe _) = Just recipe
generatedInstanceRecipe (GeneratedScaffoldCompile _ inner) = generatedInstanceRecipe inner
generatedInstanceRecipe (CellProgramCompile inner _) = generatedInstanceRecipe inner
generatedInstanceRecipe _ = Nothing

withoutGeneratedInstanceCheck :: CompilePurpose -> CompilePurpose
withoutGeneratedInstanceCheck (GeneratedInstanceCheck _ inner) = withoutGeneratedInstanceCheck inner
withoutGeneratedInstanceCheck purpose = purpose

generatedRecipe :: CompilePurpose -> Maybe GeneratedScaffoldRecipe
generatedRecipe (GeneratedScaffoldCompile recipe _) = Just recipe
generatedRecipe (CellProgramCompile inner _) = generatedRecipe inner
generatedRecipe (GeneratedInstanceCheck _ inner) = generatedRecipe inner
generatedRecipe _ = Nothing

originalPurpose :: CompilePurpose -> CompilePurpose
originalPurpose (GeneratedScaffoldCompile _ inner) = originalPurpose inner
originalPurpose (CellProgramCompile inner _) = originalPurpose inner
originalPurpose (GeneratedInstanceCheck _ inner) = originalPurpose inner
originalPurpose purpose = purpose

transformFor :: CompilePurpose -> ModuleName -> HscEnv -> ModSummary -> ParsedModule -> IO NativeParsedModule
transformFor GeneralCompile _ _ _ = pure . unannotatedModule
transformFor OriginalDeclarationCompile _ _ _ = pure . unannotatedModule
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
transformFor (HostActivationCheck signature) target env summary
  | ms_mod_name summary == target = rewriteHostInputType env 1 signature
  | otherwise = pure . unannotatedModule
transformFor (HostActivationInputCompile annotations original values) target env summary
  | ms_mod_name summary == target = \parsed -> do
      signature <- hostInputSignature annotations
      typed <- rewriteHostInputType env 2 signature parsed
      thenNativeModule typed (transformFor (CheckedItemCompile annotations original values) target env summary)
  | otherwise = pure . unannotatedModule
transformFor (ProgramItemCompile _ annotations originals _) target env summary
  | ms_mod_name summary == target = \parsed -> do
      annotated <- rewriteCheckedAnnotations env annotations parsed
      inventories <- mapM (\(owner,fingerprint) -> hydratePlannedDeclarationInventory owner fingerprint env >>= either fail pure) originals
      mapNativeModule (transformProgramDeclarationImports inventories Nothing env) annotated
  | otherwise = pure . unannotatedModule
transformFor (PlannedDeclarationCheck inventory _) target env summary
  | ms_mod_name summary == target = fmap unannotatedModule . transformPlannedDeclarationImports inventory env
  | otherwise = pure . unannotatedModule
transformFor (CellProgramCompile purpose _) target env summary = transformFor purpose target env summary
transformFor (GeneratedScaffoldCompile _ purpose) target env summary = transformFor purpose target env summary
transformFor (GeneratedInstanceCheck _ purpose) target env summary = transformFor purpose target env summary

transformWithCompletedValues :: Maybe CompletedValueImports -> CompilePurpose -> ModuleName
  -> HscEnv -> ModSummary -> ParsedModule -> IO NativeParsedModule
transformWithCompletedValues captured purpose target env summary = case purpose of
  CellProgramCompile inner _ -> transformWithCompletedValues captured inner target env summary
  GeneratedScaffoldCompile _ inner -> transformWithCompletedValues captured inner target env summary
  GeneratedInstanceCheck _ inner -> transformWithCompletedValues captured inner target env summary
  HostActivationInputCompile annotations original requested
    | ms_mod_name summary == target -> \parsed -> do
        signature <- hostInputSignature annotations
        typed <- rewriteHostInputType env 2 signature parsed
        thenNativeModule typed (transformWithCompletedValues captured (CheckedItemCompile annotations original requested) target env summary)
  CheckedItemCompile annotations original requested
    | ms_mod_name summary == target && not (null requested) -> \parsed -> do
        values <- maybe (fail "completed value interfaces were not installed in this request") pure captured
        annotated <- rewriteCheckedAnnotations env annotations parsed
        case original of
          Nothing -> mapNativeModule (transformCompletedValueImports values env) annotated
          Just (owner, fingerprint) -> do
            inventory <- hydratePlannedDeclarationInventory owner fingerprint env >>= either fail pure
            mapNativeModule (transformPlannedDeclarationImportsWithCompleted inventory values env) annotated
  ProgramItemCompile _ annotations originals requested
    | ms_mod_name summary == target -> \parsed -> do
        values <- if null requested then pure Nothing else
          Just <$> maybe (fail "program completed interfaces were not installed") pure captured
        annotated <- rewriteCheckedAnnotations env annotations parsed
        inventories <- mapM (\(owner,fingerprint) -> hydratePlannedDeclarationInventory owner fingerprint env >>= either fail pure) originals
        mapNativeModule (transformProgramDeclarationImports inventories values env) annotated
  _ -> transformFor purpose target env summary

hostInputSignature :: [(String, CheckedSignature)] -> IO CheckedSignature
hostInputSignature [(_, signature)] = pure signature
hostInputSignature _ = fail "host input recipe has no unique native signature"

withNativeTypecheckRecovery :: PipelineVariant -> ModuleName -> HscEnv -> ModSummary -> ParsedModule -> IO a -> IO a
withNativeTypecheckRecovery variant target environment summary parsed action
  | ms_mod_name summary /= target || ms_hsc_src summary /= HsSrcFile = action
  | otherwise = case pvGeneratedInstanceCheck variant of
      Nothing -> action
      Just recipe -> withGeneratedInstanceRecovery recipe environment summary parsed action

-- | The seam values for one run, derived from the downsweep graph.
data CompilePlan = CompilePlan
  { cpLoadGraph :: ModuleGraph
    -- ^ The graph handed to @load'@ (the skeleton applies @unpoison@ itself).
  , cpLoadTargets :: Maybe [Target]
    -- ^ Authenticated execution targets, scoped to the load barrier only.
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
  , cpTier :: TierPolicy
  , cpBeforeMerge :: Ghc ()
    -- ^ Runs after the compile loop and its phase emits, before the guts are
    -- merged. The session path reports accumulated injection timing here.
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
  , mfReferencedModules :: Set.Set ModuleName
  , mfQuasiQuoteOrigins :: QuasiQuoteOrigins
    -- ^ Classified once from the parsed source in 'compileFront'; see
    -- 'classifyQuasiQuoteOrigins'.
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
  | CompilerProducerScopeMismatch
  | LoadedFinalizationOwnerMismatch
  | MissingLoadedFrontend
  | MissingLoadedFinalization
  | UnfinishedLoadedFrontend
  | CandidateInterfaceBytesMismatch ModuleName
  | CandidateFrontendReplayRefused ModuleName
  | MissingFinalizedFacts ModuleName
  deriving (Show)

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
    -- this request's retained context from an 'IORef' at run time, so an
    -- empty retained set costs nothing on the compiled bytes (see
    -- 'withholdRetainedUnfoldings').
    let context = retainedContext retained
    retainedRef <- liftIO (newIORef context)
    hscForRetained <- getSession
    setSession (installRetainedUnfoldingsPlugin retainedRef hscForRetained)
    -- One cycle, no cache, no memo — 'sessionT0' is captured BEFORE this
    -- DynFlags bootstrap (above) so the default-on per-compile summary's
    -- wall-clock figure covers it too, exactly as it always has. See
    -- 'runCompileCycle''s haddock for what each argument controls.
    runCompileCycle selection Nothing Nothing context Nothing timing requestIdentity sessionT0 setupResources variant path

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

-- | One memoized module's compile artifacts, keyed by 'ModuleName' across a
-- resident worker's requests. Request-local modules are removed after each
-- compile; only stable dependency modules remain reusable.
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
  }

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
  -> (Map.Map HomeDependency HomeDependencyDigest, Int)
homeDependencyDigests graph =
  ( Map.fromList
      [ (dependency, sccDigests Map.! sccId)
      | (dependency, sccId) <- Map.toList nodeSccs
      ]
  , length components
  )
  where
    components = zip [0 :: Int ..] $ map members $ Graph.stronglyConnComp
      [ (dependency, dependency, children)
      | (dependency, (_, children)) <- Map.toList graph
      ]
    members (Graph.AcyclicSCC dependency) = [dependency]
    members (Graph.CyclicSCC dependencies) = sort dependencies
    nodeSccs = Map.fromList
      [ (dependency, sccId)
      | (sccId, dependencies) <- components
      , dependency <- dependencies
      ]
    outgoing sccId dependencies = Set.toAscList $ Set.fromList
      [ childScc
      | dependency <- dependencies
      , (_, children) <- maybeToList (Map.lookup dependency graph)
      , child <- children
      , Just childScc <- [Map.lookup child nodeSccs]
      , childScc /= sccId
      ]
    maybeToList Nothing = []
    maybeToList (Just value) = [value]
    componentMap = Map.fromList components
    compute memo sccId = case Map.lookup sccId memo of
      Just digest -> (digest, memo)
      Nothing ->
        let dependencies = componentMap Map.! sccId
            (childDigests, memo') = foldl'
              (\(digests, known) childScc ->
                let (childDigest, known') = compute known childScc
                in (childDigest : digests, known'))
              ([], memo) (outgoing sccId dependencies)
            componentDigest = digestComponent dependencies (reverse childDigests)
        in (componentDigest, Map.insert sccId componentDigest memo')
    (_, sccDigests) = foldl'
      (\(_, memo) (sccId, _) -> compute memo sccId)
      (HomeDependencyDigest BS.empty, Map.empty) components
    digestComponent dependencies children = HomeDependencyDigest $ SHA256.hash $
      BS.concat (map ownFrame dependencies ++ map childFrame (sort children))
    -- Content-keyed: the selected path is deliberately left out of this
    -- hash. Two checkouts of the same workspace resolve byte-identical
    -- dependencies at different absolute paths (a worktree-per-actor
    -- checkout); that path difference cannot change the dependency's
    -- compiled Core, so it must not cost a memo hit. 'HomeDependency'
    -- (module name + boot/ordinary kind) still identifies *what* was
    -- resolved, and 'Fingerprint' still identifies its content — together
    -- they preserve the home-resolution-shape guarantee on
    -- 'memoHomeDependencies' above (home vs. package, or which of several
    -- home candidates was selected). Only the path *string* naming where
    -- those same bytes live on disk is dropped. The full witness (path
    -- included) remains available on 'HomeDependencyWitness' itself and, when
    -- tracing is enabled, in 'gmeDirectWitnesses'; this
    -- digest is the only place a path was folded into memo validity.
    ownFrame dependency = frame $ BS8.pack $
      moduleNameString name ++ "\0" ++ show kind ++ "\0" ++ show fingerprint
      where
        HomeDependency name kind = dependency
        HomeDependencyWitness _selectedPath fingerprint = fst (graph Map.! dependency)
    childFrame (HomeDependencyDigest digest) = frame digest
    frame bytes = BS8.pack (show (BS.length bytes) ++ ":") <> bytes

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
-- always misses the memo -- unconditionally, regardless of any allowlist.
-- 'QuasiQuotes' is handled separately: see 'pureQuasiQuoters' below.
hasUnconditionallyUntrackedCompileTimeExecution :: DynFlags -> Bool
hasUnconditionallyUntrackedCompileTimeExecution flags =
  not (null (pluginInputIssues flags))
    || gopt Opt_Pp flags || any (`xopt` flags) [LangExt.Cpp, LangExt.TemplateHaskell]

-- | Quasiquoters proven pure by inspection, named by their fully-qualified
-- defining module and identifier ("Module.Path.name"). THE PURITY
-- GUARANTEE THIS RELIES ON, for every entry: the quoter's spliced
-- expression is a pure, total function of the quote body's literal source
-- string alone -- no 'Language.Haskell.TH.addDependentFile', no
-- 'Name'/'reify' lookup, no 'runIO', no read of process environment or
-- anything not already covered by the module's own 'ms_hs_hash'. That is
-- exactly the guarantee 'moduleFactHasDependentFiles' verifies
-- operationally for 'addDependentFile' alone; here it is an unenforced
-- claim about each listed quoter's implementation, re-verified by hand at
-- 'bridge/haskell/lib/Tidepool/QQ/*.hs'.
--
-- THIS IS THE ONE PLACE a new quasiquoter must be audited before a module
-- that uses it can stay memoizable: read its 'QuasiQuoter' definition, and
-- add it here only if the guarantee above holds. An unaudited entry, or an
-- entry whose quoter implementation later grows a violation (e.g. gains an
-- 'addDependentFile' call or an environment read) without a corresponding
-- re-audit of this list, silently reuses stale Core for every module that
-- imports it and every transitive importer. A quoter not listed here keeps
-- every module that uses it conservatively uncached, exactly as before this
-- allowlist existed.
pureQuasiQuoters :: Set.Set String
pureQuasiQuoters = Set.fromList
  [ "Tidepool.QQ.Bash.bash"
  , "Tidepool.QQ.Fmt.fmt"
  , "Tidepool.QQ.Json.j"
  , "Tidepool.QQ.Label.label"
  , "Tidepool.QQ.Patch.patch"
  , "Tidepool.QQ.Validate.uri"
  ]

-- | What a module's quasiquote occurrences (if any) resolved to, as of its
-- last fresh compile ('classifyQuasiQuoteOrigins', run once in
-- 'compileFront' from the parsed source). A later memo-hit decision trusts
-- this recorded fact instead of re-parsing: 'ms_hs_hash' equality already
-- guarantees identical source bytes, which guarantees the identical set of
-- quote occurrences this classification saw.
data QuasiQuoteOrigins
  = NoQuasiQuotes
    -- ^ The module has no quasiquote occurrences at all (including when
    -- 'QuasiQuotes' is off).
  | AllPureQuasiQuotes (Set.Set String)
    -- ^ Every occurrence resolved, via the module's own import list, to an
    -- allowlisted ('pureQuasiQuoters') quoter. The set is diagnostic only
    -- (TIDEPOOL_MEMO_TRACE "quoters=...").
  | HasUntrackedQuasiQuote (Set.Set String)
    -- ^ At least one occurrence did not resolve to an allowlisted quoter:
    -- unlisted, locally defined in this module (so not brought in by any
    -- import), or an import shape this resolver does not attempt (e.g. an
    -- ambiguous unqualified import). The set is whatever names were
    -- recoverable, diagnostic only; an unresolved occurrence renders as
    -- "<unresolved>".
  deriving (Eq, Show)

-- | Walk a parsed module's quasiquote occurrences ('HsQuasiQuote' nodes,
-- present in the parser's own output before renaming ever expands them) and
-- resolve each occurrence's quoter identifier against the allowlist above.
-- Resolution consults 'HscEnv' (the current session's home package table)
-- to find each candidate import's *defining* module for the occurrence --
-- not just the module named in the import declaration -- so a quoter
-- re-exported through a convenience module (e.g. 'Tidepool.Actors.Exomonad'
-- re-exporting 'Tidepool.QQ.Label.label') still resolves to the allowlist
-- key of its true origin. Dependency modules are already compiled and
-- resident in the HPT by the time this runs (batch compile visits imports
-- before importers); a module not yet resident (impossible in practice
-- for this pipeline) simply fails to disambiguate, same as today.
classifyQuasiQuoteOrigins :: HscEnv -> ParsedModule -> IO QuasiQuoteOrigins
classifyQuasiQuoteOrigins hscEnv parsed
  | null occurrences = pure NoQuasiQuotes
  | otherwise = do
      resolved <- mapM (resolveQuoterOrigin hscEnv imports) occurrences
      pure $ if all isAllowlisted resolved
        then AllPureQuasiQuotes (Set.fromList [origin | Just origin <- resolved])
        else HasUntrackedQuasiQuote (Set.fromList (map (fromMaybe "<unresolved>") resolved))
  where
    hsMod = unLoc (pm_parsed_source parsed)
    imports = hsmodImports hsMod
    occurrences = quasiQuoteOccurrences (ms_hspp_opts (pm_mod_summary parsed)) (pm_parsed_source parsed)
    isAllowlisted (Just origin) = Set.member origin pureQuasiQuoters
    isAllowlisted Nothing = False

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

-- | Diagnostic only (TIDEPOOL_MEMO_TRACE): honestly report which
-- quasiquoters, if any, a module's last fresh compile actually saw --
-- distinguishing "none", "every occurrence is an allowlisted pure quoter",
-- and "at least one is not", each naming the qualified quoter identities
-- observed (or "<unresolved>" for one this resolver could not place).
renderQuasiQuoteOrigins :: QuasiQuoteOrigins -> String
renderQuasiQuoteOrigins NoQuasiQuotes = "none"
renderQuasiQuoteOrigins (AllPureQuasiQuotes origins) = "pure:" ++ intercalate "," (Set.toList origins)
renderQuasiQuoteOrigins (HasUntrackedQuasiQuote origins) = "untracked:" ++ intercalate "," (Set.toList origins)

-- | Resolve one quasiquote occurrence's syntactic quoter identifier to a
-- "Module.Path.name" origin string naming its *defining* module -- not the
-- module named in the import that brought it into this file's scope. A
-- candidate import module's defining module for the occurrence comes from
-- 'definingModuleForOcc', which follows the already-loaded interface's
-- export list (so a quoter re-exported through a convenience module, e.g.
-- 'Tidepool.Actors.Exomonad' re-exporting 'Tidepool.QQ.Label.label',
-- resolves to "Tidepool.QQ.Label.label" either way). 'Nothing' whenever
-- that resolution is not unambiguous (including: the identifier is defined
-- locally in this module rather than imported, so no import provides it;
-- or more than one candidate import resolves the occurrence to a different
-- defining module). Callers must treat 'Nothing' as unlisted/unknown, never
-- as allowlisted: failing closed here is what keeps this resolver sound
-- without needing full renamer-grade name resolution.
resolveQuoterOrigin :: HscEnv -> [LImportDecl GhcPs] -> RdrName -> IO (Maybe String)
resolveQuoterOrigin hscEnv imports rdrName = case rdrName of
  Qual qualifier occ -> resolveCandidates occ
      [ unLoc (ideclName decl)
      | L _ decl <- imports
      , unLoc (ideclName decl) == qualifier || fmap unLoc (ideclAs decl) == Just qualifier
      ]
  Unqual occ -> resolveCandidates occ
      [ unLoc (ideclName decl)
      | L _ decl <- imports
      , ideclQualified decl == NotQualified
      , importBringsOccIntoScope decl occ
      ]
  _ -> pure Nothing
  where
    resolveCandidates occ candidateModNames = do
      defining <- mapM (\mn -> definingModuleForOcc hscEnv mn occ) (nub candidateModNames)
      pure $ case nub [ mn | Just mn <- defining ] of
        [definingModName] -> Just (moduleNameString definingModName ++ "." ++ occNameString occ)
        _ -> Nothing
    importBringsOccIntoScope decl occ = case ideclImportList decl of
      Nothing -> True
      Just (Exactly, L _ ies) -> occ `elem` importListOccs ies
      Just (EverythingBut, L _ ies) -> occ `notElem` importListOccs ies
    importListOccs ies = everything (++) (mkQ [] ieRdrNameOcc) ies
    ieRdrNameOcc :: RdrName -> [OccName]
    ieRdrNameOcc (Unqual o) = [o]
    ieRdrNameOcc (Qual _ o) = [o]
    ieRdrNameOcc _ = []

-- | The module that actually defines the exported entity named 'occ', as
-- seen through the already-loaded interface of home-package module 'modName'
-- -- not necessarily 'modName' itself, since an export list can re-export a
-- name whose 'Name' still carries its original defining module. Consults
-- only the resident home package table ('lookupHpt'): by the time a module
-- is classified, this pipeline has already compiled its import dependencies
-- in topological order, so any module actually in scope here is HPT-resident.
-- A module not found there (not yet compiled, or a non-home package) yields
-- 'Nothing', which 'resolveQuoterOrigin' treats as "this candidate does not
-- resolve" -- failing closed, exactly like an unrecognized import shape.
definingModuleForOcc :: HscEnv -> ModuleName -> OccName -> IO (Maybe ModuleName)
definingModuleForOcc hscEnv modName occ = pure $ case lookupHpt (hsc_HPT hscEnv) modName of
  Nothing -> Nothing
  Just hmi -> case
      [ definingModule
      | avail <- mi_exports (hm_iface hmi)
      , nm <- availNames avail
      , nameOccName nm == occ
      , Just definingModule <- [nameModule_maybe nm]
      ] of
    (m : _) -> Just (moduleName m)
    [] -> Nothing

-- Facts needed even when a module contributes no executable body. Keeping
-- these separately lets an unchanged re-export or validation-only module
-- prove its dependents valid without retaining its compiler session graph.
data ModuleFacts = ModuleFacts
  { moduleFactTyCons :: [TyCon]
  , moduleFactReferences :: Set.Set ModuleName
  , moduleFactPackageImports :: PackageImportEvidence
  , moduleFactHasDependentFiles :: Bool
  , moduleFactQuasiQuoteOrigins :: QuasiQuoteOrigins
    -- ^ Diagnostic and gate input: see 'QuasiQuoteOrigins'. 'lookupValidMemo'
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
  | HydratedObservation ModSummary HomeModInfo PackageImportEvidence

observationSummary :: ModuleObservation -> ModSummary
observationSummary (CachedObservation summary _) = summary
observationSummary (LoadedObservation loaded) = loadedSummary loaded
observationSummary (HydratedObservation summary _ _) = summary

observationFacts :: ModuleObservation -> IO ModuleFacts
observationFacts (CachedObservation _ entry) = pure (payloadFacts (gmePayload entry))
observationFacts (LoadedObservation loaded) = pure (loadedFacts loaded)
observationFacts (HydratedObservation _ hmi roots) = pure ModuleFacts
  { moduleFactTyCons = typeEnvTyCons (md_types (hm_details hmi))
  , moduleFactReferences = Set.empty
  , moduleFactPackageImports = roots
  , moduleFactHasDependentFiles = False
  , moduleFactQuasiQuoteOrigins = NoQuasiQuotes
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
    , moduleFactQuasiQuoteOrigins = mfQuasiQuoteOrigins front
    }

type GutsMemo = Map.Map ModuleName GutsMemoEntry

-- | One compile cycle in an already-open 'Ghc' session. Loaded sources
-- finalize in the typed phase hooks; deferred sources finalize in dependency
-- order before their importers. The caller owns session bootstrap and decides
-- whether the immutable module memo survives this cycle.
--
-- Three seams, independent of the 'PipelineVariant' seam above:
--
--   * 'mCache' — 'load''s 'ModIfaceCache' (§7.1 — cycles 2..N skip stdlib
--     recompilation). 'Nothing' matches a lone compile's own
--     @load' Nothing ...@ byte for byte.
--   * 'mMemoRef' — the per-module dep-guts memo (§7.6/§7.3 — a module's
--     guts, once compiled in ANY cycle, are reused verbatim by every LATER
--     cycle that compiles the same module again). 'Nothing' disables it
--     entirely, compiling every module fresh — a lone compile's only cycle
--     always takes this path.
--   * 'summaryT0' — the caller's compile start. Direct callers capture it
--     before session bootstrap; resident callers capture it per request.
--
-- 'retained' is the immutable per-request index used by both the plugin's
-- recompilation fingerprint and the prepared memo's module validity check.
runCompileCycle
  :: PipelineSelection result -> Maybe ModIfaceCache -> Maybe (IORef GutsMemo)
  -> RetainedContext -> Maybe String -> Bool -> Word64 -> Double -> Maybe ResourceTimingStart -> PipelineVariant -> FilePath -> Ghc result
runCompileCycle selection mCacheInput mMemoRefInput retained incarnation timing requestIdentity sessionT0 setupResources variant path = withCycleHooks $ do
    forM_ (compilerProducerFor variant) $ \producer ->
      forM_ (pvExactScope variant) $ \scope ->
        unless (producer == scopeProducerSha256 scope)
          (liftIO (throwIO CompilerProducerScopeMismatch))
    memoTrace <- liftIO readMemoTraceEnabled
    let preparation = selectionKind selection
        captureProducts = capturesProductInterfaces selection
        candidateManifest = candidateManifestFor selection
        exactCycle = exactCompileCycle selection variant
        -- An exact hydration transaction cannot borrow mutable interface or
        -- Core memo state from a preceding lexical environment.
        mCache = if exactCycle then Nothing else mCacheInput
        mMemoRef = if exactCycle then Nothing else mMemoRefInput
    when exactCycle $ do
      current <- getSession
      fresh <- liftIO (freshExactState current)
      setSession fresh
    unless exactCycle $ do
      current <- getSession
      -- Make unloads home executables only for LinkInMemory. Extraction uses
      -- NoLink, so this request boundary owns the same loader transition.
      -- Immutable cached interfaces and bytecode remain available for reuse.
      liftIO $ forM_ (hsc_interp current) $ \interp -> Linker.unload interp current []
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
    pushLogHookM (diagnosticCollectorHook path warnRef errorRef)
    -- EPS unpoisoning (QQ/TH support — see canonicalizeDFlags haddock).
    -- 'depanal' runs downsweep, whose @enableCodeGenForTH@ downgrades the
    -- splice-needed home modules' ms_hspp_opts to -O0 +
    -- Opt_IgnoreInterfacePragmas so 'load' can provision bytecode. That flag
    -- ALSO governs how external interfaces are READ, and the downgraded
    -- modules compile FIRST, so the session-global External Package State
    -- would cache every interface they demand (GHC.Num, GHC.Float,
    -- freer-simple, …) WITHOUT unfoldings — the -O2 extraction loop below
    -- then can never fire class-op rules (@negate $fNumDouble@ never
    -- reduces, chasing Integer machinery → "Unsupported primop: clz#").
    -- Unset JUST that flag on every summary BEFORE compilation: the
    -- backend/-O0 downgrade stays (splices still provision via bytecode),
    -- but interface loading honors pragmas, so the EPS is healthy from the
    -- start. Non-TH graphs carry no downgrade — the unset is a no-op there.
    --
    -- A post-'load' EPS flush cannot work: home-module
    -- TyCons are already realized in the HPT, so re-typechecking lib modules
    -- never re-demands the package interfaces that define their instances —
    -- they never re-enter the fresh EPS, and typechecking fails with e.g.
    -- "No instance for Monad (Eff '[Console, …])".
    -- GHC may reuse a preprocessed summary solely from the source hash.
    -- A changed include must run preprocessing again before memo validation.
    previous <- getSession
    let keepSummary (ModuleNode _ summary) = not (xopt LangExt.Cpp (ms_hspp_opts summary))
        keepSummary _ = True
    setSession previous {hsc_mod_graph = mkModuleGraph
      (filter keepSummary (mgModSummaries' (hsc_mod_graph previous)))}
    modGraphDownsweep <- depanal (pvDownsweepExcludes variant) False
    forM_ (pvExactScope variant) $ \scope -> do
      let exactNames = Set.fromList [mkModuleName (exactModule iface)
            | (iface, _, _) <- scopeInterfaces scope]
      when (any (\node -> case node of
          ModuleNode _ summary -> ms_mod_name summary `Set.member` exactNames
          _ -> False) (mgModSummaries' modGraphDownsweep)) $
        liftIO $ ioError $ userError "fresh source collides with an admitted exact owner"
    modGraphRaw <- elideUnusedQuasiQuoteCodegen timing modGraphDownsweep
    sourceSelection <- case pvExactScope variant of
      Nothing -> pure Nothing
      Just scope -> withSourceSelectionRefusal
        (selectCurrentSourceOriginals scope (pvGeneratedScaffold variant) modGraphRaw)
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
    acceptedCandidates <- case candidateManifest of
      Nothing -> pure Map.empty
      Just manifest -> certifyModuleCandidates (compilerProducerFor variant) selectedExact
        sourceFreeOwners manifest modGraphRaw path
    -- 'ghc_setup' phase (TIDEPOOL_TIMING): 'guessTarget'/'setTargets' + this
    -- 'depanal' call, nothing else, on EVERY caller — a lone compile also
    -- includes its session bootstrap because 'runCompile' captures
    -- 'sessionT0' first. A resident request starts after the shared bootstrap.
    -- This phase is flat and non-overlapping with 'ghc_load'.
    setupT1 <- monotonicTime
    endResourceTiming setupResources "compile" "ghc_setup"
    liftIO (emitPhase timing "ghc_setup" (elapsedMs sessionT0 setupT1))
    originalPlan <- pvPlan variant timing modGraphRaw selectedExact
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
                           installed <- getSession
                           setSession (hscUpdateHPT (\hpt -> addToHpt hpt name hmi) installed)
                 , cpTier = OptimizeEveryModule }
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
          in summary { ms_hspp_opts = (canonicalizeDFlags original)
               { backend = backend original, ghcLink = ghcLink original } }
    targetName <- liftIO (targetModuleNameFor path)
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
    frontendOriginsRef <- liftIO (newIORef Map.empty)
    pendingFinalizationsRef <- liftIO (newIORef Map.empty)
    beforeLoad <- getSession
    let originalPhaseHook = runPhaseHook (hsc_hooks beforeLoad)
        runOriginalPhase :: TPhase a -> IO a
        runOriginalPhase phase = case originalPhaseHook of
          Nothing -> runPhase phase
          Just (PhaseHook hook) -> hook phase
        canonicalSummary summary = summary
          { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts summary) }
        canonicalEnvironment env summary = scopeRetainedHscEnv (ms_mod summary)
          (hscSetFlags (ms_hspp_opts (canonicalSummary summary)) env)
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
          unless (name `Map.member` acceptedCandidates) $ do
            atomicModifyIORef' finalizedModulesRef (\known -> (Map.insert name finalized known, ()))
            when captureProducts $
              atomicModifyIORef' productInterfacesRef (\known -> (Map.insert name skinny known, ()))
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
                origins <- liftIO (classifyQuasiQuoteOrigins current parsed)
                transformed <- liftIO (pvTransformParsed variant current summaryC parsed)
                ((tcg, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName current summaryC parsed
                  (typecheckNativeModuleWithDiagnostics current transformed)
                liftIO (validateCompilationFamilies current tcg)
                liftIO $ do
                  atomicModifyIORef' frontendOriginsRef (\known ->
                    (Map.insert (ms_mod summaryC) (origins, current) known, ()))
                  when (ms_mod_name summaryC == targetName) (writeIORef targetEnvironmentRef (Just tcg))
                  when timing $ hPutStrLn stderr $
                    "tidepool-canonical-frontend module=" ++ moduleNameString (ms_mod_name summaryC)
                pure (FrontendTypecheck tcg, warnings)) session
                `catch` \failure -> do
                  when (ms_mod_name summary == targetName) $
                    writeIORef targetInstanceFailure (Just (failure :: GeneratedInstanceRejection))
                  throwIO failure
        canonicalLoadPhase (T_HscPostTc phaseEnv summary (FrontendTypecheck tcg) tcWarnings oldHash)
          | ms_hsc_src summary == HsSrcFile = do
              (origins, env) <- atomicModifyIORef' frontendOriginsRef (\known ->
                (Map.delete (ms_mod summary) known, Map.lookup (ms_mod summary) known))
                >>= maybe (throwIO MissingLoadedFrontend) pure
              let summaryC = canonicalSummary summary
                  flags = hsc_dflags env
                  resultRoots = [idName identifier | cpKeepPrivateResult plan
                    , ms_mod_name summaryC == targetName
                    , identifier <- typeEnvIds (tcg_type_env tcg)
                    , occNameString (nameOccName (idName identifier)) `elem` cpResultBinders plan
                    , nameModule_maybe (idName identifier) == Just (ms_mod summaryC)]
              modifyIORef' (tcg_keep tcg) (`extendNameSetList` resultRoots)
              (desugared, dsWarnings) <- runHsc' env (hscDesugar' (ms_location summaryC) tcg)
              printOrThrowDiagnostics (hsc_logger env) (initPrintConfig flags) (initDiagOpts flags)
                (unionMessages tcWarnings dsWarnings)
              plugins <- readIORef (tcg_th_coreplugins tcg)
              simplified <- hscSimplify env plugins desugared
              (tidy, details) <- hscTidy env simplified
              roots <- directPackageImports env tcg
              files <- readIORef (tcg_dependent_files tcg)
              let partial = force (mkPartialIface env (cg_binds tidy) details summaryC
                    (tcg_import_decls tcg) simplified)
                  externalized = externalizeInternalTops simplified
                  output = ModuleOutput (mg_module externalized) (mg_binds externalized)
                    (capturedBindingDisplay evalUserBinder tcg) (capturedCellBinderPins env tcg)
                    (foldr (<|>) Nothing [capturedBindingType name tcg | name <- cpResultBinders plan])
                  facts = ModuleFacts (mg_tcs desugared) (moduleRefs desugared) roots
                    (not (null files)) origins
                  pending = PendingFinalization summaryC facts output tidy details env
                  action = HscRecomp tidy (ms_location summaryC) partial oldHash
              when timing $ hPutStrLn stderr $
                "tidepool-canonical-finalization module=" ++ moduleNameString (ms_mod_name summaryC)
              if not (backendGeneratesCode (backend (hsc_dflags phaseEnv)))
                then do
                  iface <- mkFullIface env partial Nothing Nothing NoStubs []
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
        captureCanonicalFailure phase = canonicalLoadPhase phase
          `catch` (\failure -> do
            when (targetPhase phase) (writeIORef targetLoadFailure (Just (failure :: SourceError)))
            throwIO failure)
          `catch` (\failure -> do
            writeIORef canonicalFailureRef (Just (failure :: CanonicalFrontendFailure))
            throwIO failure)
    when (isJust originalPhaseHook) (liftIO (throwIO CustomLoadPhaseHook))
    when (isJust (hscFrontendHook (hsc_hooks beforeLoad))) (liftIO (throwIO CustomLoadFrontendHook))
    loadT0 <- monotonicTime
    loadResources <- beginResourceTiming timing
    loadFlag <- reifyGhc $ \session -> bracket
      (reflectGhc (getSession >>= \env -> setSession env
        { hsc_hooks = (hsc_hooks env)
            { runPhaseHook = Just (PhaseHook captureCanonicalFailure) } }) session)
      (const (reflectGhc (getSession >>= \env -> setSession env
        { hsc_hooks = hsc_hooks beforeLoad }) session))
      (const (reflectGhc (withLoadTargets (cpLoadTargets plan) $ load' mCache loadHowMuch
        dependencyDiagnostic (Just batchMsg)
        (scopeRetainedModuleGraph (mapMG canonicalizeLoadSummary loadGraph))) session))
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
        origins <- liftIO (readIORef frontendOriginsRef)
        unless (Map.null pending && Map.null origins) $
          liftIO $ throwIO UnfinishedLoadedFrontend
      Failed -> do
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
    loadedModules <- liftIO (readIORef loadedModulesRef)
    summaries0 <- cpSummaries plan
    let summaries = [ ms | ms <- summaries0, ms_hsc_src ms == HsSrcFile ]
    when (null summaries) $
      liftIO $ ioError (userError (pvLabel variant ++ ": empty module graph"))
    let compileExecutable = do
          -- These summed phases cover deferred finalization only. Canonical
          -- frontend work performed by the load hooks belongs to 'ghc_load'.
          tcMsRef <- liftIO (newIORef (0 :: Integer))
          loweringMsRef <- liftIO (newIORef (0 :: Integer))
          -- The existing diagnostic row separates deferred desugaring,
          -- simplification and selected STG counts; it is not a speedup metric.
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
                let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
                -- Classification needs the session's HPT as it stands right
                -- before this module's own typecheck (to resolve import
                -- origins -- see 'classifyQuasiQuoteOrigins'); dependency
                -- modules are already resident by this point in the batch.
                classifyEnv <- getSession
                ((tcGblEnv, quasiQuoteOrigins), tcMs) <- timeSection $
                  timeDetailPhase timing "typecheck" (moduleNameString (ms_mod_name modSum)) $ do
                    parsed <- parseModule modSum
                    origins <- liftIO (classifyQuasiQuoteOrigins classifyEnv parsed)
                    transformed <- liftIO (pvTransformParsed variant classifyEnv modSum parsed)
                    ((tcg, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName classifyEnv modSum parsed
                      (typecheckNativeModuleWithDiagnostics classifyEnv transformed)
                    let flags = ms_hspp_opts modSum
                    liftIO (printOrThrowDiagnostics (hsc_logger classifyEnv)
                      (initPrintConfig flags) (initDiagOpts flags) warnings)
                    familyEnvironment <- getSession
                    liftIO (validateCompilationFamilies familyEnvironment tcg)
                    pure (tcg, origins)
                when (ms_mod_name modSum == targetName) $
                  liftIO (writeIORef targetEnvironmentRef (Just tcGblEnv))
                liftIO (modifyIORef' tcMsRef (+ tcMs))
                hscEnv0 <- getSession
                let hscEnv   = scopeRetainedHscEnv (ms_mod modSum)
                                 (hscUpdateFlags canonicalizeDFlags hscEnv0)
                    -- Capture the inferred type of the eval's top expression NOW,
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
                (desugared0, dsMs) <- timeSection $ liftIO (hscDesugar hscEnv modSum tcGblEnv)
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
                                 , mfQuasiQuoteOrigins = quasiQuoteOrigins
                                 , mfHasDependentFiles = not (null dependentFiles) }
              -- The per-module back half: the optimized-Core pass, the
              -- shared interface registration, then stable name externalization.
              -- Interface construction and prepared lowering share the same tidy
              -- result; the memo retains the interface alongside its prepared body.
              compileBack interfaceUse mf = do
                liftIO (modifyIORef' backCountRef (+ 1))
                (simplified, coreMs) <- timeSection $ liftIO $ do
                  plugins <- readIORef (tcg_th_coreplugins (mfTcGblEnv mf))
                  hscSimplify (mfHscEnv mf) plugins (mfDesugared mf)
                liftIO (modifyIORef' loweringMsRef (+ coreMs))
                liftIO (modifyIORef' c2cMsRef (+ coreMs))
                liftIO (modifyIORef' moduleMsRef
                          (Map.insertWith (+) (moduleNameString (ms_mod_name (mfSummary mf))) coreMs))
                let interfaceReuse = if isJust mMemoRef then MemoMiss else MemoDisabled
                (interfaceMs, registration) <- registerPreparedInterface timing requestIdentity interfaceReuse
                  interfaceUse (mfSummary mf) (mfTcGblEnv mf) (mfHscEnv mf) simplified
                liftIO $ do
                  modifyIORef' finalizedModulesRef (Map.insert (ms_mod_name (mfSummary mf)) registration)
                  when captureProducts $ modifyIORef' productInterfacesRef (Map.insert
                    (ms_mod_name (mfSummary mf)) (hm_iface (finalizedHomeModInfo registration)))
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
              prepareFinalized loaded = case preparation of
                CheckOnly -> pure Nothing
                PrepareStg -> do
                  liftIO (modifyIORef' preparedCountRef (+ 1))
                  current <- getSession
                  let summary = loadedSummary loaded
                      env = scopeRetainedHscEnv (ms_mod summary)
                        (hscUpdateFlags canonicalizeDFlags current)
                      cgGuts = finalizedTidyGuts (loadedFinalized loaded)
                      ownedSiblings = resolvePreparedSiblings (cg_binds cgGuts)
                      importedSiblings = resolvePreparedInterfaceSiblings env
                  siblings <- liftIO $ atomicModifyIORef' preparedSiblingsRef $ \known ->
                    let known' = Map.union ownedSiblings (Map.union known importedSiblings)
                    in (known', known')
                  siteAuthority <- timePhase timing "prepared_site_authority" $ liftIO
                    (resolveSiteAuthority env (instEnvElts
                      (md_insts (hm_details (finalizedHomeModInfo (loadedFinalized loaded))))))
                  (elaboratedBindings, yieldSites, preparedSites, typeGraph, rejections) <- timePhase timing "prepared_sites" $ liftIO $
                    elaboratePreparedSites env siteAuthority siblings (cg_binds cgGuts)
                  let elaboration = PreparedElaboration
                        { peGuts = cgGuts
                        , peBindings = elaboratedBindings
                        , peSitedSiblings = ownedSiblings
                        , peYieldSites = yieldSites
                        , pePreparedSites = preparedSites
                        , peTypeGraph = typeGraph
                        , peEffectRequestTypeIds = siteAuthorityEffectRequestTypeIds siteAuthority
                        , peSiteRejections = rejections
                        }
                  Just <$> timePhase timing "prepared_stg" (liftIO (prepareModule env summary elaboration))
              finalizeCurrent interfaceUse summary = do
                captured <- liftIO (Map.lookup (ms_mod summary) <$> readIORef loadedModulesRef)
                case captured of
                  Just loaded -> pure loaded
                  Nothing -> do
                    front <- compileFront summary
                    (_, output, finalized) <- compileBack interfaceUse front
                    facts <- liftIO (frontFacts front)
                    pure (LoadedModule (mfSummary front) facts output finalized)
              rememberFinalized loaded = do
                let name = ms_mod_name (loadedSummary loaded)
                    finalized = loadedFinalized loaded
                liftIO (modifyIORef' finalizedModulesRef (Map.insert name finalized))
                when captureProducts $ liftIO (modifyIORef' productInterfacesRef
                  (Map.insert name (hm_iface (finalizedHomeModInfo finalized))))
              rememberPreparedSiblings prepared = liftIO $
                modifyIORef' preparedSiblingsRef (\known -> Map.union (pmSitedSiblings prepared) known)
          -- Module names do not identify generated content across independent
          -- requests. A memo hit therefore requires the current source hash and
          -- the selected path/fingerprint closure of every home import. The
          -- closure matters for SOURCE imports because boot summaries are not in
          -- the executable memo walk: an import inside a .hs-boot must still
          -- invalidate its ordinary importer. Ordinary dependencies additionally
          -- propagate compile validity in summary order below.
          let summaryDependency summary = HomeDependency (ms_mod_name summary)
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
              (dependencyDigests, digestComputations) =
                homeDependencyDigests dependencyGraph
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
          -- The selected module graph, once per cycle (never per lookup).
          -- Graph capture holds paths and fingerprints only.
          when memoTrace $ liftIO $
            forM_ (Map.toList summaryByDependency) $ \(dependency@(HomeDependency name kind), summary) -> do
              let HomeDependencyWitness selectedPath fingerprint = summaryFingerprints Map.! dependency
                  resolvedPath = normalise <$> ml_hs_file (ms_location summary)
                  directDeps = [ moduleNameString d | d <- Set.toList (directHomeDeps summary) ]
                  digestHex = case Map.lookup dependency dependencyDigests of
                    Just (HomeDependencyDigest bytes) -> hexBytes bytes
                    Nothing -> "<none>"
              emitMemoCycleGraph memoTrace requestIdentity (moduleNameString name) (show kind)
                selectedPath resolvedPath fingerprint directDeps digestHex
          validThisCycleRef <- liftIO (newIORef (Map.empty :: Map.Map ModuleName Bool))
          executableValidRef <- liftIO (newIORef (Map.empty :: Map.Map ModuleName Bool))
          dropMemoInterface <- liftIO (lookupEnv "TIDEPOOL_TEST_DROP_MEMO_INTERFACE")
          -- The withholding pass can change only a module's own retained
          -- definitions; retained identities defined elsewhere reach it through
          -- a dependency, whose invalidity is already covered by 'depsValidSoFar'.
          let retainedFor modSum = retainedDefinedBy (ms_mod modSum) retained
          let depsValidSoFar modSum = liftIO $ do
                validMap <- readIORef validThisCycleRef
                pure (all (\d -> Map.findWithDefault False d validMap)
                  (Set.toList (directHomeDeps modSum)))
              recordValidity modSum isValid =
                liftIO (modifyIORef' validThisCycleRef (Map.insert (ms_mod_name modSum) isValid))
              executableDepsValid modSum = liftIO $ do
                validMap <- readIORef executableValidRef
                pure (all (\d -> Map.findWithDefault False d validMap)
                  (Set.toList (directHomeDeps modSum)))
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
                | ms_mod modSum `Map.member` loadedModules = pure Nothing
                | ms_mod_name modSum == targetName = pure Nothing
                | otherwise = case mMemoRef of
                  Nothing -> pure Nothing
                  Just ref -> lookupMemo ref modSum
              lookupMemo ref modSum = do
                  depsOk <- depsValidSoFar modSum
                  -- Cpp/TemplateHaskell gate here, before any entry lookup:
                  -- unconditional, no allowlist can rescue them. A
                  -- 'QuasiQuotes'-only module still might be memoizable —
                  -- that depends on a *previous* entry's recorded quoter
                  -- origins, so it is decided below, once 'entry' is in scope.
                  if not depsOk || hasUnconditionallyUntrackedCompileTimeExecution (ms_hspp_opts modSum)
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
                              sameRetained = memoRetained validity == retainedFor modSum
                              sameHomeDependencies =
                                memoHomeDependencies validity == homeDependencyWitnesses modSum
                              -- Session generations are incarnation-local,
                              -- even within a multi-operation transaction.
                              -- Reuse requires both sides to name that owner.
                              -- Every
                              -- non-session module is exempt: its identity
                              -- is not incarnation-scoped.
                              sameIncarnation =
                                not (isJust (parseSessionModule (moduleNameString (ms_mod_name modSum))))
                                  || (isJust incarnation && memoIncarnation validity == incarnation)
                              -- Reached only once Cpp/TemplateHaskell are
                              -- both ruled out above, so any remaining
                              -- 'hasUntrackedCompileTimeExecution' is due to
                              -- QuasiQuotes alone. That extension flag gates
                              -- the whole module even when a quote
                              -- occurrence never runs, so it stays
                              -- conservative UNLESS the previous entry's
                              -- recorded classification ('frontFacts',
                              -- 'classifyQuasiQuoteOrigins') already proved
                              -- every occurrence resolved to an allowlisted
                              -- ('pureQuasiQuoters') quoter -- in which case
                              -- 'sameHash' below (identical source bytes)
                              -- guarantees today's occurrences are the exact
                              -- same ones, with no need to re-parse here.
                              quasiQuotesPureOnRecord = case moduleFactQuasiQuoteOrigins (payloadFacts (gmePayload entry)) of
                                AllPureQuasiQuotes _ -> True
                                NoQuasiQuotes -> True
                                _ -> False
                              compileTimeExecutionTracked =
                                not (hasUntrackedCompileTimeExecution (ms_hspp_opts modSum))
                                  || quasiQuotesPureOnRecord
                          -- Source hashes do not cover CPP includes, splices,
                          -- quasiquoters (unless allowlisted above), or
                          -- addDependentFile inputs. These modules therefore
                          -- remain conservatively uncached.
                          if not (moduleFactHasDependentFiles (payloadFacts (gmePayload entry)))
                              && compileTimeExecutionTracked
                              && sameHash
                              && sameRetained
                              && sameHomeDependencies
                              && sameIncarnation
                            then pure (Just entry)
                            else if not compileTimeExecutionTracked
                              -- A QuasiQuotes-only module whose recorded
                              -- occurrences are not (all) allowlisted: same
                              -- short reason Cpp/TemplateHaskell already use
                              -- above, so this stays indistinguishable from
                              -- the always-conservative case on the plain
                              -- TIDEPOOL_TIMING line. TIDEPOOL_MEMO_TRACE
                              -- still names exactly which quoters were seen.
                              then do
                                memoMiss modSum "untracked-compile-time-execution"
                                memoMissTrace modSum
                                  ("no-reuse:untracked-compile-time-execution quasiquotes="
                                    ++ renderQuasiQuoteOrigins (moduleFactQuasiQuoteOrigins (payloadFacts (gmePayload entry))))
                                  (Just entry)
                                pure Nothing
                              else do
                                memoMiss modSum $ unwords
                                  [ "dependent-files=" ++ show (moduleFactHasDependentFiles (payloadFacts (gmePayload entry)))
                                  , "same-hash=" ++ show sameHash
                                  , "same-retained=" ++ show sameRetained
                                  , "same-home-dependencies=" ++ show sameHomeDependencies
                                  , "same-incarnation=" ++ show sameIncarnation
                                  , "quasiquotes=" ++ renderQuasiQuoteOrigins (moduleFactQuasiQuoteOrigins (payloadFacts (gmePayload entry))) ]
                                memoMissTrace modSum (unwords
                                  [ "dependent-files=" ++ show (moduleFactHasDependentFiles (payloadFacts (gmePayload entry)))
                                  , "same-hash=" ++ show sameHash
                                  , "same-retained=" ++ show sameRetained
                                  , "same-home-dependencies=" ++ show sameHomeDependencies
                                  , "same-incarnation=" ++ show sameIncarnation
                                  , "quasiquotes=" ++ renderQuasiQuoteOrigins (moduleFactQuasiQuoteOrigins (payloadFacts (gmePayload entry))) ]) (Just entry)
                                pure Nothing
          let interfaceUses = zipWith homeInterfaceUse summaries (homeInterfaceConsumers summaries)
          (observations, results, preparedModules, mReachable) <- case cpTier plan of
            OptimizeEveryModule -> do
              pairs <- forM (zip summaries interfaceUses) $ \(modSum, interfaceUse) -> do
                cpBeforeModule plan modSum
                let mn = ms_mod_name modSum
                case Map.lookup mn acceptedCandidates of
                  Just candidate -> do
                    hmi <- case lookupHpt (hsc_HPT certifiedEnv) mn of
                      Just value -> pure value
                      Nothing -> liftIO $ ioError $ userError
                        "certified candidate interface absent during module loop"
                    recordValidity modSum True
                    recordExecutableValidity modSum True
                    when captureProducts $ liftIO $
                      modifyIORef' productInterfacesRef (Map.insert mn (hm_iface hmi))
                    pure (HydratedObservation modSum hmi (admittedCandidateRoots candidate), Nothing, Nothing)
                  Nothing -> do
                    cached <- lookupValidMemo modSum
                    case cached of
                      -- A memo hit reuses the prepared body and its exact interface.
                      Just entry
                        | Just moduleProduct <- payloadProduct (gmePayload entry)
                        , interfaceReady interfaceUse modSum entry -> do
                        recordValidity modSum True
                        recordExecutableValidity modSum True
                        rememberPreparedSiblings (productPrepared moduleProduct)
                        let finalized = productFinalized moduleProduct
                        liftIO $ modifyIORef' finalizedModulesRef (Map.insert mn finalized)
                        when captureProducts $ liftIO $
                          modifyIORef' productInterfacesRef (Map.insert mn (hm_iface (finalizedHomeModInfo finalized)))
                        when (needsPreparedInterface interfaceUse) $
                          forM_ (cachedInterface modSum entry) (installPreparedInterface mn)
                        pure (CachedObservation modSum entry, Just (productOutput moduleProduct), Just (productPrepared moduleProduct))
                      _ -> do
                        recordValidity modSum False
                        recordExecutableValidity modSum False
                        forM_ cached $ \entry -> do
                          let reason
                                | isNothing (payloadProduct (gmePayload entry)) =
                                    "executable-body-not-prepared"
                                | otherwise = "required-interface-not-retained"
                          memoMiss modSum reason
                          memoMissTrace modSum reason (Just entry)
                        loaded <- case cached of
                          Just entry | interfaceReady interfaceUse modSum entry ->
                            pure (payloadLoaded modSum (gmePayload entry))
                          _ -> finalizeCurrent interfaceUse modSum
                        rememberFinalized loaded
                        let r = loadedOutput loaded
                            finalized = loadedFinalized loaded
                            facts = loadedFacts loaded
                        prepared <- prepareFinalized loaded
                        moduleProduct <- requireProduct facts r prepared finalized
                        case mMemoRef of
                          Just ref -> liftIO (modifyIORef' ref
                            (Map.insert mn (GutsMemoEntry
                              (MemoValidity
                                (ms_hs_hash modSum)
                                (retainedFor modSum)
                                (homeDependencyWitnesses modSum)
                                incarnation)
                              (ExecutableProduct moduleProduct)
                              requestIdentity
                              (memoDiagnosticWitnesses modSum))))
                          Nothing  -> pure ()
                        pure (LoadedObservation loaded, Just r, prepared)
              pure ([observation | (observation, _, _) <- pairs], [r | (_, Just r, _) <- pairs],
                    [p | (_, _, Just p) <- pairs], Nothing)
            OptimizeCoreReachable -> do
              -- Install every valid finalized owner before checking any later
              -- importer. Reference facts select STG only after this pass;
              -- unprepared owners retain their same interface/Core pair.
              observations' <- forM (zip summaries interfaceUses) $ \(modSum, interfaceUse) -> do
                cpBeforeModule plan modSum
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
              facts <- liftIO (mapM observationFacts observations')
              -- Desugared Core captures resolved dictionaries and therefore
              -- includes instance-only dependencies in the selected closure.
              -- All owners are already finalized; this only selects STG work.
              forceValidationOnly <- liftIO (lookupEnv "TIDEPOOL_TEST_FORCE_VALIDATION_ONLY")
              let referencesByMod = Map.fromList
                    [ (ms_mod_name (observationSummary observation), moduleFactReferences fact)
                    | (observation, fact) <- zip observations' facts ]
                  reachableMods0 = reachableModuleClosure targetModName' referencesByMod
                  -- A focused fault-injection test can omit one real reachable
                  -- owner from STG preparation without changing its finalization.
                  reachableMods = case forceValidationOnly of
                    Just m  -> Set.delete (mkModuleName m) reachableMods0
                    Nothing -> reachableMods0
              let rememberExecutable modSum output prepared finalized moduleFacts = do
                    moduleProduct <- requireProduct moduleFacts output prepared finalized
                    case mMemoRef of
                      Just ref -> liftIO (modifyIORef' ref
                        (Map.insert (ms_mod_name modSum)
                          (GutsMemoEntry
                            (MemoValidity
                              (ms_hs_hash modSum)
                              (retainedFor modSum)
                              (homeDependencyWitnesses modSum)
                              incarnation)
                            (ExecutableProduct moduleProduct)
                            requestIdentity
                            (memoDiagnosticWitnesses modSum))))
                      Nothing -> pure ()
                  compileReachable _interfaceUse observation moduleFacts = do
                    let modSum = observationSummary observation
                    loaded <- case observation of
                      LoadedObservation captured -> pure captured
                      CachedObservation _ entry -> pure (payloadLoaded modSum (gmePayload entry))
                      HydratedObservation _ _ _ -> liftIO (throwIO MissingLoadedFinalization)
                    rememberFinalized loaded
                    let r = loadedOutput loaded
                        finalized = loadedFinalized loaded
                    prepared <- prepareFinalized loaded
                    rememberExecutable modSum r prepared finalized moduleFacts
                    pure [(r, prepared)]
                  validationOnly observation moduleFacts = do
                    let modSum = observationSummary observation
                    loaded <- case observation of
                      LoadedObservation value -> pure value
                      CachedObservation _ entry -> pure (payloadLoaded modSum (gmePayload entry))
                      HydratedObservation _ _ _ -> liftIO (throwIO MissingLoadedFinalization)
                    case mMemoRef of
                      Just ref -> liftIO (modifyIORef' ref
                        (Map.insert (ms_mod_name modSum)
                          (GutsMemoEntry
                            (MemoValidity
                              (ms_hs_hash modSum)
                              (retainedFor modSum)
                              (homeDependencyWitnesses modSum)
                              incarnation)
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
                      , interfaceReady interfaceUse modSum entry -> do
                        recordExecutableValidity modSum True
                        rememberPreparedSiblings (productPrepared moduleProduct)
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
                            | otherwise = "required-interface-not-retained"
                      memoMiss modSum reason
                      memoMissTrace modSum reason (Just entry)
                      compileReachable interfaceUse observation moduleFacts
                    LoadedObservation _ -> compileReachable interfaceUse observation moduleFacts
                    HydratedObservation _ _ _ -> compileReachable interfaceUse observation moduleFacts
                  -- Keep an existing executable only while its dependencies
                  -- and required interface still match. An unreachable source
                  -- needs validation facts, so preparing a new body here would
                  -- run compiler actions without contributing to this request.
                  else case observation of
                    CachedObservation _ entry
                      | depsExecutable
                      , Just moduleProduct <- payloadProduct (gmePayload entry)
                      , interfaceReady interfaceUse modSum entry -> do
                        recordExecutableValidity modSum True
                        rememberPreparedSiblings (productPrepared moduleProduct)
                        when (needsPreparedInterface interfaceUse) $
                          forM_ (cachedInterface modSum entry) (installPreparedInterface (ms_mod_name modSum))
                        pure []
                    _ -> validationOnly observation moduleFacts >> pure []
              pure (observations', map fst rs, [p | (_, Just p) <- rs], Just reachableMods)
          totalTcMs   <- liftIO (readIORef tcMsRef)
          totalLoweringMs <- liftIO (readIORef loweringMsRef)
          liftIO (emitPhase timing "typecheck" totalTcMs)
          liftIO (emitPhase timing "lowering" totalLoweringMs)
          summaryT1 <- monotonicTime
          liftIO $ do
            moduleTimes <- readIORef moduleMsRef
            interfaceTotal <- readIORef interfaceMsRef
            moduleInterfaces <- readIORef moduleInterfaceMsRef
            let topModules = take 3 (sortOn (negate . snd) (Map.toList moduleTimes))
                topInterfaces = take 3 (sortOn (negate . snd) (Map.toList moduleInterfaces))
            emitPhase timing "module_interface" interfaceTotal
            emitCompileSummary (length summaries) (elapsedMs sessionT0 summaryT1)
              totalTcMs totalLoweringMs interfaceTotal topModules topInterfaces
            emitModuleTiming timing (sortOn (negate . snd) (Map.toList moduleTimes))
              (sortOn (negate . snd) (Map.toList moduleInterfaces))
          -- Diagnostic-only (see 'dsMsRef'/'c2cMsRef' haddock above): NOT part of
          -- the tidepool-timing wire grammar, so 'ExtractTiming::parse' never sees
          -- it and there is nothing to keep in sync there. Emitted only under
          -- 'OptimizeCoreReachable' (there is no tier to report otherwise), and
          -- AFTER the phase lines, exactly where it has always been.
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
          timePhase timing "merge_barrier" $ cpBeforeMerge plan
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
          hscFinal <- getSession
          exactTyCons <- liftIO (exactInterfaceTyCons hscFinal (pvExactScope variant))
          let allBinds  = concatMap moduleOutputBinds depOutputs
                ++ moduleOutputBinds targetOutput
              allTyCons = concatMap moduleFactTyCons moduleFacts ++ exactTyCons
          targetEnvironment <- liftIO (readIORef targetEnvironmentRef) >>= maybe
            (liftIO (ioError (userError (pvLabel variant ++ ": target frontend environment is absent")))) pure
          warnings <- liftIO (nub . reverse <$> readIORef warnRef)
          let pipelineResult = PipelineResult
                { prBinds  = allBinds
                , prTyCons = allTyCons
                , prHscEnv = cpFinalEnv plan hscFinal
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
                  "tidepool-checked-loaded-source module=" ++ moduleNameString (ms_mod_name summary)
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
                    facts <- liftIO (observationFacts (HydratedObservation summary hmi (maybe emptyPackageImports admittedCandidateRoots (Map.lookup (ms_mod_name summary) acceptedCandidates))))
                    liftIO (modifyIORef' checkedFactsRef ((ms_mod_name summary, facts) :))
                  pure Nothing
                else do
                  liftIO $ hPutStrLn stderr $
                    "tidepool-checked module=" ++ moduleNameString (ms_mod_name summary)
                      ++ " target=" ++ show isTarget
                  (tcg, origins) <- timeDetailPhase timing "checked_typecheck"
                    (moduleNameString (ms_mod_name summary)) $ do
                      parsed <- parseModule summary
                      origins <- liftIO (classifyQuasiQuoteOrigins current parsed)
                      transformed <- liftIO (pvTransformParsed variant current summary parsed)
                      ((checkedEnvironment, _), warnings) <- liftIO $ withNativeTypecheckRecovery variant targetName current summary parsed
                        (typecheckNativeModuleWithDiagnostics current transformed)
                      let flags = ms_hspp_opts summary
                      liftIO (printOrThrowDiagnostics (hsc_logger current)
                        (initPrintConfig flags) (initDiagOpts flags) warnings)
                      familyEnvironment <- getSession
                      liftIO (validateCompilationFamilies familyEnvironment checkedEnvironment)
                      pure (checkedEnvironment, origins)
                  let inspectionProbes = capturedInspectionProbes tcg
                      retainInterface reason = do
                        -- A later source module's normal home import resolves via
                        -- this HPT entry. A source-less Val interface injected before
                        -- a later module can also mention an earlier generated Lib
                        -- without importing it from source. SOURCE imports keep using
                        -- the boot iface installed by GHC's load phase, and no returned
                        -- metadata consumer reads the target back through HPT.
                        env <- scopeRetainedHscEnv (ms_mod summary) <$> getSession
                        details <- liftIO (mkBootModDetailsTc (hsc_logger env) tcg)
                        (iface, _ifaceMs) <- liftIO $ measureModuleInterface timing requestIdentity
                          (moduleNameString (ms_mod_name summary)) CheckedEnvironmentInterface HptMiss $
                            mkIfaceTc env Sf_None details summary Nothing tcg
                        let linkable = maybe emptyHomeModInfoLinkable hm_linkable
                              (lookupHpt (hsc_HPT env) (ms_mod_name summary))
                            hmi = HomeModInfo (set_mi_extra_decls Nothing iface) details linkable
                        setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name summary) hmi) env)
                        when timing $ liftIO $ hPutStrLn stderr $
                          "tidepool-checked-interface-retained module="
                            ++ moduleNameString (ms_mod_name summary) ++ reason
                  dependentFiles <- liftIO (readIORef (tcg_dependent_files tcg))
                  liftIO (modifyIORef' checkedFactsRef ((ms_mod_name summary, ModuleFacts
                    { moduleFactTyCons = typeEnvTyCons (tcg_type_env tcg)
                    , moduleFactReferences = Set.empty
                    , moduleFactPackageImports = emptyPackageImports
                    , moduleFactHasDependentFiles = not (null dependentFiles)
                    , moduleFactQuasiQuoteOrigins = origins }) :))
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
          cpBeforeMerge plan
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
                verified <- liftIO (revalidateExactScope env (compilationScope compilation))
                either (liftIO . ioError . userError) pure verified
                liftIO (writeExactCompilation compilation evidence)
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
    case selection of
      PreparedStg -> do
        (result, modules, dependencies, productInterfaces, finalizedModules, packageRoots) <- compileExecutable
        pure PreparedPipelineResult
          { pprPipelineResult = result
          , pprModules = modules
          , pprDependencies = dependencies
          , pprProductInterfaces = productInterfaces
          , pprFinalizedModules = finalizedModules
          , pprPackageImports = packageRoots
          , pprAcceptedCandidates = []
          , pprExactCompilation = exactCompilation
          }
      PreparedProducts _ -> do
        (result, modules, dependencies, productInterfaces, finalizedModules, packageRoots) <- compileExecutable
        valid <- liftIO $ revalidateAcceptedCandidates (Map.elems acceptedCandidates)
        when (not valid) $ liftIO $ ioError $ userError
          "accepted module candidate changed before artifact publication"
        pure PreparedPipelineResult
          { pprPipelineResult = result
          , pprModules = modules
          , pprDependencies = dependencies
          , pprProductInterfaces = productInterfaces
          , pprFinalizedModules = finalizedModules
          , pprPackageImports = packageRoots
          , pprAcceptedCandidates = map admittedCandidateOriginal (Map.elems acceptedCandidates)
          , pprExactCompilation = exactCompilation
          }
      CheckedEnvironment -> compileChecked
      CheckedEnvironmentProducts _ -> compileChecked

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

certifyModuleCandidates
  :: Maybe String -> Maybe ExactScope -> Set.Set ModuleName -> FilePath -> ModuleGraph -> FilePath
  -> Ghc (Map.Map ModuleName AdmittedSourceCandidate)
certifyModuleCandidates expectedProducer exactScope sourceFreeOwners manifest graph targetPath = do
  timing <- liftIO readTimingEnabled
  observations <- liftIO (newIORef Map.empty)
  let record owner reason detail = when timing $ liftIO $
        modifyIORef' observations (Map.insert owner (reason, fmap (take 192) detail))
      recordCandidate candidate = record (candidateUnit candidate, candidateModule candidate)
  decoded <- liftIO (readModuleCandidatesWithGraphs
    (maybe [] scopeExecutionGraphs exactScope) manifest)
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
          graphInventoryMatches =
            let producerMatches = case exactScope of
                  Nothing -> True
                  Just scope -> all ((== scopeProducerSha256 scope) . executionGraphProducer) combinedGraphs
            in producerMatches && Map.size graphInventory <= 4096
              && length combinedReferences <= 4096
              && sum (map (BS.length . executionGraphBytes) combinedGraphs) <= 4 * 1024 * 1024
          candidateExecutionMatches candidate = graphInventoryMatches
            && either (const False) (const True) (candidateProof candidate)
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
              reason : _ -> recordCandidate candidate reason Nothing >> pure Nothing
              [] -> do
                source <- liftIO $ traverse canonicalizePath
                  (ml_hs_file (ms_location summary))
                if source /= Just (candidateSource candidate)
                  then recordCandidate candidate CandidateSourcePath Nothing >> pure Nothing
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
                      hydratedResult <- hydrateCandidateHomeProductsWithOriginals env { hsc_mod_graph = nativeGraph }
                        canonicalLoadGraph interfaces (either (const []) id originalInterfaces)
                        installLexical
                        [nativeSummary summary | (_,summary,_,_) <- Map.elems admitted] selectedBoots
                      case hydratedResult of
                        Left reason -> recordAdmitted CandidateHydration (Just reason) >> pure Map.empty
                        Right hydrated -> do
                          setSession hydrated { hsc_mod_graph = hsc_mod_graph env }
                          forM_ (Map.toAscList admitted) $ \(name,(_,summary,_,_)) ->
                            when (backendGeneratesCode (backend (ms_hspp_opts summary))) $ do
                              executableEnvironment <- getSession
                              let originalFlags = ms_hspp_opts summary
                                  executableSummary = summary {ms_hspp_opts =
                                    (canonicalizeDFlags originalFlags)
                                      {backend = backend originalFlags, ghcLink = ghcLink originalFlags}}
                              home <- liftIO $ hydrateCandidateExecutable executableEnvironment (proofs Map.! name) executableSummary
                              setSession (hscUpdateHPT (\hpt -> addToHpt hpt name home) executableEnvironment)
                              liftIO $ emitCount timing "candidate_finalized_core_bytecode" 1
                          recordAdmitted CandidateAccepted Nothing
                          pure (Map.mapWithKey (\name (candidate, summary, _, roots) -> AdmittedSourceCandidate
                            candidate roots (if backendGeneratesCode (backend (ms_hspp_opts summary))
                              then CandidateLoadForExecution else CandidateInterfaceOnly) (proofs Map.! name)) admitted)
  when timing $ liftIO $ do
    observed <- readIORef observations
    let counts = Map.fromListWith (+)
          [(reason, 1 :: Integer) | (reason, _) <- Map.elems observed]
    forM_ (Map.toAscList counts) $ \(reason, count) ->
      emitCount timing ("candidate_admission." ++ show reason) count
    forM_ (take 128 (Map.toAscList observed)) $ \((unit, name), (reason, detail)) ->
      hPutStrLn stderr ("tidepool-candidate-admission owner=" ++ show (take 192 (unit ++ ":" ++ name))
        ++ " reason=" ++ show reason ++ maybe "" ((" detail=" ++) . show) detail)
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
    coreValid <- case canonicalCoreArtifact proof of
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
    bounded path' limit = withBinaryFile path' ReadMode $ \handle -> do
      bytes <- BS.hGet handle (limit + 1)
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
                  || (xopt LangExt.QuasiQuotes flags && case moduleFactQuasiQuoteOrigins facts of
                        NoQuasiQuotes -> False
                        AllPureQuasiQuotes _ -> False
                        HasUntrackedQuasiQuote _ -> True)
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
-- Resident session: one 'runGhc' boot serving compile requests one at a time.
-- Transport-blind: this module
-- knows nothing about sockets or frames (the worker transport owns that) —
-- it hands the caller a plain IO closure shaped exactly like
-- 'runPipelineSessionSelected', so app/Main.hs's existing dispatch can substitute it
-- in with no other change to its own call sites.
-- ---------------------------------------------------------------------------

-- | Resident compiler with an explicit representation selection per request.
-- Prepared outputs share the same validity checks and request-scope cleanup as
-- the optimized guts from which they were produced.
--
-- The compiler closure's shape matches 'runPipelineSessionSelected''s own
-- (a leading 'PipelineSelection' and a retained-generation 'Set.Set') so
-- 'app/Main.hs' can hold either behind one @Compiler@ alias. This entry
-- point boots exactly one 'HscEnv' in 'runGhc' below and reuses it across
-- every subsequent request; the withholding plugin is installed on it
-- exactly ONCE, at boot, via 'installRetainedUnfoldingsPlugin''s 'IORef'
-- seam ('Tidepool.RetainedUnfoldings') rather than once per request, so
-- passes never accumulate. Each request writes its own retained set into
-- that cell before 'residentCompileOne' runs and resets it to 'Set.empty'
-- afterwards (also on an exception), so the one installed pass reflects
-- only the current request.
withResidentPipelineSelected
  :: [FilePath]
  -> (ResidentCompiler -> IO a)
  -> IO a
withResidentPipelineSelected baseIncludes useCompiler =
  withResidentPipelineSelectedRequests baseIncludes $ \runRequest ->
    runRequest (pure ()) useCompiler

-- | Keep transaction-scoped compiler state across every compile needed to
-- prepare one cell, then remove it before admitting the next transaction.
-- A cell check can compile repeatedly while rejecting generated instances;
-- treating each retry as a request boundary discards the memo entries the
-- next retry was meant to reuse. The scoped runner owns cleanup so a caller
-- cannot admit another request without first sanitizing this one's state.
withResidentPipelineSelectedRequests
  :: [FilePath]
  -> (RequestRunner -> IO a)
  -> IO a
withResidentPipelineSelectedRequests baseIncludes useRequests = do
  producer <- captureCompilerProducerIdentity
  timing <- readTimingEnabled
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    let dflags' = extractionDynFlags dflags baseIncludes
    _ <- setSessionDynFlags dflags'
    retainedRef <- liftIO (newIORef emptyRetainedContext)
    hscForRetained <- getSession
    setSession (installRetainedUnfoldingsPlugin retainedRef hscForRetained)
    availability <- liftIO (newIORef ResidentAvailable)
    ownerThread <- liftIO myThreadId
    let baseImportPaths = importPaths dflags'
    reifyGhc $ \session ->
      let resetSession = reflectGhc
            (getSession >>= liftIO . freshExactState >>= setSession) session
          runRequest :: RequestRunner
          runRequest clearRecovery action = bracket acquire release $ \() -> do
            -- These caches contain GHC values tied to this environment, not
            -- portable products. No cache entry survives its owning bracket.
            cacheRef <- newIfaceCache >>= newIORef
            memoRef <- newIORef Map.empty
            stateOriginRef <- newIORef OrdinarySourceState
            phase <- newIORef CompilerReady
            requestIdentity <- newTimingRequestIdentity
            let compile :: ResidentCompiler
                compile selection retained purpose mscope path extraIncludes buildProductsDir = mask $ \restore -> do
                  caller <- myThreadId
                  unless (caller == ownerThread) (throwIO CompilerTransactionWrongThread)
                  previous <- atomicModifyIORef' phase $ \state ->
                    (if state == CompilerReady then CompilerRunning else state, state)
                  case previous of
                    CompilerReady -> pure ()
                    CompilerRunning -> throwIO CompilerTransactionBusy
                    CompilerFailed -> throwIO CompilerTransactionFailed
                    CompilerClosed -> throwIO CompilerTransactionReleased
                  result <- restore (do
                    targetModName' <- targetModuleNameFor path
                    cache <- readIORef cacheRef
                    -- Target source can be transformed differently by each
                    -- purpose, even when its bytes have not changed.
                    evictTargetMemo targetModName' memoRef
                    (writeIORef retainedRef (retainedContext retained) >>
                      reflectGhc
                        (residentCompileOne producer selection cache memoRef retainedRef stateOriginRef dflags' baseImportPaths
                          timing requestIdentity purpose mscope path extraIncludes buildProductsDir)
                        session)
                      `finally` writeIORef retainedRef emptyRetainedContext)
                    `catch` \(failure :: SomeException) -> do
                      writeIORef phase CompilerFailed
                      case fromException failure :: Maybe SomeAsyncException of
                        Just _ -> throwIO failure
                        Nothing -> do
                          -- A caller may retry a rejected generated instance.
                          -- No partial compiler or recovery graph is reusable.
                          clearRecovery
                          writeIORef memoRef Map.empty
                          newIfaceCache >>= writeIORef cacheRef
                          resetSession
                          writeIORef stateOriginRef OrdinarySourceState
                          writeIORef phase CompilerReady
                          throwIO failure
                  writeIORef phase CompilerReady
                  pure result
                finish = do
                  writeIORef phase CompilerClosed
                  writeIORef memoRef Map.empty
                  clearRecovery
            action compile `finally` finish
          acquire = do
            caller <- myThreadId
            unless (caller == ownerThread) (throwIO CompilerTransactionWrongThread)
            previous <- atomicModifyIORef' availability $ \state ->
              (if state == ResidentAvailable then ResidentBusy else state, state)
            case previous of
              ResidentAvailable -> pure ()
              ResidentBusy -> throwIO CompilerTransactionBusy
              ResidentPoisoned -> throwIO CompilerTransactionPoisoned
              ResidentClosed -> throwIO CompilerTransactionReleased
          release () = do
            writeIORef availability ResidentPoisoned
            writeIORef retainedRef emptyRetainedContext
            resetSession
            writeIORef availability ResidentAvailable
      in useRequests runRequest `finally` writeIORef availability ResidentClosed

type ResidentCompiler = forall result.
  PipelineSelection result
  -> Set.Set SymbolIdentity
  -> CompilePurpose
  -> Maybe SessionScope
  -> FilePath
  -> [FilePath]
  -> Maybe FilePath
  -> IO result

-- The caller's recovery graphs share the compiler transaction's lifetime.
-- Invalidation also runs before any synchronous failed-attempt retry.
type RequestRunner = forall requestResult.
  IO () -> (ResidentCompiler -> IO requestResult) -> IO requestResult

-- | One resident-session compile cycle, against the ALREADY-OPEN session
-- 'withResidentPipelineSelected' booted. Patches @importPaths@ for THIS cycle only
-- (see 'withResidentPipelineSelected'), compiles with the shared 'ModIfaceCache' +
-- 'GutsMemo'. The transaction boundary established by
-- 'withResidentPipelineSelectedRequests' releases its GHC-valued memo after
-- both successful and exceptional transactions.
-- Captures a fresh start time so every request gets its own compile summary.
--
-- The resident and direct paths select the same pipeline variant. This is an
-- output contract: optimization tier affects validation-only dependency Core
-- and therefore can affect merged metadata.
--
-- Reads the context that the request runner wrote into the plugin's cell
-- once at cycle start, then shares that value with memo validation.
residentCompileOne
  :: Maybe CompilerProducerIdentity -> PipelineSelection result -> ModIfaceCache -> IORef GutsMemo -> IORef RetainedContext -> IORef ResidentStateOrigin -> DynFlags -> [FilePath]
  -> Bool -> Word64 -> CompilePurpose -> Maybe SessionScope -> FilePath -> [FilePath] -> Maybe FilePath
  -> Ghc result
residentCompileOne producer selection cache memoRef retainedRef stateOriginRef baseDFlags baseImportPaths timing requestIdentity purpose mscope path extraIncludes buildProductsDir = do
  sessionT0 <- monotonicTime
  setupResources <- beginResourceTiming timing
  retained <- liftIO (readIORef retainedRef)
  selectedVariant <- liftIO $ case mscope of
    Just scope | isSessionScopeActive scope -> sessionVariant purpose scope path
    _                                        -> normalVariant purpose path
  let variant = selectedVariant {pvCompilerProducer = producer}
  previousOrigin <- liftIO (readIORef stateOriginRef)
  let currentOrigin = residentStateOrigin selection variant
      reset = case (previousOrigin,currentOrigin) of
        (ExactState,ExactState) -> False
        (ExactState,_) -> True
        (LegacySourceFreeState,OrdinarySourceState) -> True
        _ -> False
  when reset $ do
      -- A source-free scope leaves virtual summaries, exact interfaces and
      -- splice executables. Ordinary source admission needs a fresh GHC
      -- environment, while its independently validated memo can stay warm.
      -- Keep reset-required provenance until the replacement is installed;
      -- cancellation during unload must require another reset next time.
      getSession >>= liftIO . freshExactState >>= setSession
  liftIO (writeIORef stateOriginRef currentOrigin)
  requestImportPaths <- liftIO (compileSearchPaths variant extraIncludes baseImportPaths)
  hsc0 <- getSession
  let sourceState
        | importPaths (hsc_dflags hsc0) == requestImportPaths = hsc0
        | otherwise = hsc0 { hsc_mod_graph = mkModuleGraph [] }
      -- Downsweep can reuse a byte-identical source summary with the previous
      -- request's import paths. Refresh summaries when the search path changes;
      -- compiled interfaces and dependency-validated module products stay warm.
  setSession (hscUpdateFlags
    (configureBuildProducts baseDFlags buildProductsDir .
      (\df -> df { importPaths = requestImportPaths }))
    sourceState)
  let incarnation = mscope >>= ssIncarnation
  runCompileCycle selection (Just cache) (Just memoRef) retained incarnation timing requestIdentity sessionT0 setupResources variant path

-- Protected requests use their complete admitted search order. GHC's boot
-- defaults (including the worker CWD) are not additional source authority.
compileSearchPaths :: PipelineVariant -> [FilePath] -> [FilePath] -> IO [FilePath]
compileSearchPaths variant requested ordinaryBase = case pvExactScope variant >>= scopeIncludePaths of
  Nothing -> pure (nub (ordinaryBase ++ requested))
  Just admitted -> do
    unless (requested == admitted) (throwIO SearchInputsChanged)
    pure admitted

evictTargetMemo :: ModuleName -> IORef GutsMemo -> IO ()
evictTargetMemo targetModName' memoRef =
  modifyIORef' memoRef (Map.delete targetModName')

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
  -> ModSummary -> TcGblEnv -> HscEnv -> ModGuts
  -> Ghc (Integer, FinalizedModule)
registerPreparedInterface timing requestId interfaceReuse interfaceUse modSum tcGblEnv hscEnv simplified = do
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
  case originalPurpose purpose of
    HostActivationInputCompile {} -> fail "host activation input requires its sealed session admission"
    HostActivationCheck {} -> fail "host activation check requires its sealed session admission"
    _ -> pure ()
  targetModName' <- targetModuleNameFor path
  pure PipelineVariant
   { pvLabel = "runPipeline"
   , pvExactScope = Nothing
   , pvCompilerProducer = Nothing
   , pvGeneratedScaffold = generatedRecipe purpose
   , pvGeneratedInstanceCheck = generatedInstanceRecipe purpose
   , pvDownsweepExcludes = []
   , pvTransformParsed = transformFor purpose targetModName'
   , pvPlan = \_timing modGraphRaw _selectedExact -> pure CompilePlan
      { cpLoadGraph = modGraphRaw
      , cpLoadTargets = Nothing
      , cpAfterLoad = pure ()
        -- Consume load captures and finalize any deferred source in dependency
        -- order before selecting prepared STG.
      , cpSummaries = pure
          [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing) ]
        -- 'runPipeline' (single-shot eval) always compiles a target named
        -- @result@; a resident turn without prior bindings can also land on
        -- this variant while its template names @__result@. Try both, in that
        -- order.
      , cpKeepPrivateResult = purpose == OriginalDeclarationCompile
      , cpResultBinders = [scaffoldOutputBase, scaffoldTargetName]
      , cpBeforeModule = \_ -> pure ()
      , cpTier = if purpose == CertifyHomeProductsCompile
          then OptimizeEveryModule else OptimizeCoreReachable
      , cpBeforeMerge = pure ()
      , cpFinalEnv = id
      }
  }

-- | The SESSION extraction variant (active 'SessionScope' only). The same
-- 'runCompile' skeleton as 'normalVariant' — @depanal@/@load'@ then the
-- dependency-ordered finalization and preparation of each home module, with
-- the session-scope injection seam filled in:
--
--   1. The source-less @Val.G<g>@ modules are EXCLUDED from @depanal@ (no
--      source to summarise) and their thin ifaces are INJECTED into the HPT +
--      finder immediately before each source module that imports them. This
--      preserves the chronological Lib/Val dependency DAG instead of eagerly
--      manufacturing a cycle.
--   2. Every module that (transitively) imports one of those — the turn target
--      included — is excluded from the @load'@ graph (it cannot be compiled
--      before the Val ifaces exist) and compiled instead in the
--      dependency-directed loop, which also registers it back into the HPT
--      ('registerPreparedInterface').
--
-- Its tier is 'OptimizeEveryModule'. Compiling every home module to full -O2
-- guts (rather than extracting only the target and resolving its library
-- calls from HPT ifaces) keeps executable dependencies paired with their
-- registered interfaces. A
-- reference turn imports @Tidepool.Prelude@ via the eval preamble; the
-- @load'@ also keeps those source deps "loaded" (GHC-58427).
-- Recipes grant execution only. The ordinary graph and its lexical admission
-- are checked before discovering any source needed by an original quoter.
data ExactExecutionPlan = ExactExecutionPlan
  { executionLoadGraph :: ModuleGraph
  , executionLoadTargets :: Maybe [Target]
  , executionOriginalNodes :: [ExecutionSourceNode]
  , executionFreshProviders :: [ModSummary]
  , executionLinkGraph :: ModuleGraph
  , executionNegativePaths :: [FilePath]
  , executionPackageRoots :: [PackageImportRoot]
  }

-- Linker hooks and diagnostic collectors belong to one cycle. Restore both
-- even when refusal or cancellation prevents the final environment handoff.
withCycleHooks :: Ghc a -> Ghc a
withCycleHooks action = reifyGhc $ \session -> bracket
  (reflectGhc ((\env -> (hsc_hooks env, hsc_logger env)) <$> getSession) session)
  (\(hooks, logger) -> reflectGhc
    (getSession >>= \env -> setSession env {hsc_hooks=hooks, hsc_logger=logger}) session)
  (const (reflectGhc action session))

-- GHC's whole-module pipeline consults targets to choose bytecode generation.
-- Restore only this field in the current environment, retaining the load's HPT.
withLoadTargets :: Maybe [Target] -> Ghc a -> Ghc a
withLoadTargets Nothing action = action
withLoadTargets (Just targets) action = reifyGhc $ \session -> bracket
  (reflectGhc (hsc_targets <$> getSession) session)
  (\original -> reflectGhc (getSession >>= \env -> setSession env {hsc_targets=original}) session)
  (const (reflectGhc (setTargets targets >> action) session))

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

-- Only actual current source imports can select an original library. The
-- retained interface inventory itself grants no source or lexical visibility.
selectCurrentSourceOriginals
  :: ExactScope -> Maybe GeneratedScaffoldRecipe -> ModuleGraph
  -> Ghc (Maybe SourceSelectedOriginals)
selectCurrentSourceOriginals admitted recipe sourceGraph = do
  initial <- getSession
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
      hiddenImports = [(summary,imported,key) | summary <- sourceSummaries
        , imported <- ms_textual_imps summary ++ ms_srcimps summary
        , Just key <- [localOwner imported], Map.member key exactOwners
        , (key `Set.notMember` lexicalOwners || key `Set.member` scopeSourceSelectedOwners admitted)
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
            liftIO (readGeneratedScaffoldImportAuthority closure'
              (scopeExecutionNativeOwners admitted) protected parsed sourceGraph hydrated)
              >>= either (liftIO . fail) pure
          _ -> liftIO (fail "generated scaffold target summary is missing or duplicated")
    let roots = Set.toAscList (Set.fromList [key | (summary,imported,key) <- hiddenImports
          , not (permitsGeneratedScaffoldImport scaffold summary key imported)])
    case roots of
      [] -> pure Nothing
      root : _ -> do
        includes <- maybe (liftIO (throwIO (ExecutionSourceUnavailable root))) pure
          (scopeIncludePaths admitted)
        unless (all ((== includes) . importPaths . ms_hspp_opts) sourceSummaries) $
          liftIO (throwIO (ExecutionSourceUnsupported root))
        nodes <- either (liftIO . throwIO) pure (executionSourceClosure
          (scopeExecutionGraphs admitted) (scopeExecutionOwners admitted) (scopeExecutionNativeOwners admitted) roots)
        validation <- validateExactOriginalSources admitted interfaces sourceGraph nodes
        let selectedKeys = Set.fromList (map (executionIdentityKey . executionNodeIdentity) nodes)
            selectedGraph = mkModuleGraph [node | node@(ModuleNode _ summary) <-
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
                , not (permitsGeneratedScaffoldImport scaffold summary key imported)]
            relevant resolution = (dependencyResolutionQualifier resolution,dependencyResolutionModule resolution,
              dependencyResolutionBoot resolution) `Set.member` relevantImports
        unless (all ((== includes) . importPaths . ms_hspp_opts)
            [summary | ModuleNode _ summary <- mgModSummaries' selectedGraph]) $
          liftIO (throwIO (ExecutionSourceUnsupported root))
        (sources,complete) <- liftIO (captureDependencySources selectedGraph)
        unless complete $ liftIO (throwIO (ExecutionSourceChanged root))
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
              [(executionNodeIdentity node,executionGraphSha256 (executionNodeGraph node)) | node <- nodes] evidence
        setSession initial
        pure (Just selected)

data ValidatedOriginalSources = ValidatedOriginalSources
  { validatedOriginalGraph :: ModuleGraph
  , validatedOriginalTargets :: [Target]
  , validatedOriginalEvidence :: DependencyEvidence
  , validatedOriginalNegatives :: [FilePath]
  , validatedOriginalPackages :: [PackageImportRoot]
  }

-- Current source selection and compile-time bytecode loading share the same
-- original recipe checks. Selection alone never loads or replaces a product.
validateExactOriginalSources
  :: ExactScope -> [(ExactIfaceArtifact, ModIface)] -> ModuleGraph
  -> [ExecutionSourceNode] -> Ghc ValidatedOriginalSources
validateExactOriginalSources admitted interfaces sourceGraph nodes = do
  initial <- getSession
  let ownerKey owner = (unitString (moduleUnit owner), moduleNameString (moduleName owner))
      originalByOwner = Map.fromList [((exactUnit artifact,exactModule artifact),iface)
        | (artifact,iface) <- interfaces]
  let selected = Map.fromList [(executionIdentityKey (executionNodeIdentity node),node) | node <- nodes]
      selectedNames = Set.fromList [mkModuleName name | (_,name) <- Map.keys selected]
      excluded = [mkModuleName (exactModule artifact) | (artifact,_,_) <- scopeInterfaces admitted
        , mkModuleName (exactModule artifact) `Set.notMember` selectedNames]
        ++ [mkModuleName (exactModule artifact) | artifact <- scopeValueInterfaces admitted]
  extraTargets <- forM nodes $ \node -> do
    let source = dependencyModuleSource (executionNodeModule node)
        key = executionIdentityKey (executionNodeIdentity node)
    bytes <- liftIO (BS.readFile source)
    unless (hexBytes (SHA256.hash bytes) == executionNodeSourceSha256 node) $
      liftIO (throwIO (ExecutionSourceChanged key))
    contents <- either (const (liftIO (throwIO (ExecutionSourceUnsupported key)))) pure
      (TextEncoding.decodeUtf8' bytes)
    now <- liftIO (getModificationTime source)
    target <- guessTarget source Nothing Nothing
    pure target {targetContents=Just (stringToStringBuffer (Text.unpack contents),now)
      ,targetAllowObjCode=False}
  -- The baseline preflight installed virtual Finder locations. Execution
  -- selection must use real current source resolution, never those locations.
  finder <- liftIO initFinderCache
  setSession initial {hsc_FC=finder}
  setTargets (hsc_targets initial ++ extraTargets)
  executionGraph <- depanal excluded False
  env <- getSession
  current <- liftIO (dependencyEvidenceFor env ([],True) executionGraph [])
  let summaries = Map.fromList [(ownerKey (ms_mod summary),summary)
        | ModuleNode _ summary <- mgModSummaries' executionGraph, ms_hsc_src summary == HsSrcFile]
      currentNodes = Map.fromList [((dependencyModuleUnit node,dependencyModuleName node),node)
        | node <- dependencyModules current, not (dependencyModuleBoot node)]
      originalNames = Map.keysSet originalByOwner
      freshSummaries = Map.fromList [((ownerKey (ms_mod summary),ms_hsc_src summary == HsBootFile),summary)
        | ModuleNode _ summary <- mgModSummaries' sourceGraph]
  forM_ (mgModSummaries' executionGraph) $ \graphNode -> case graphNode of
    ModuleNode _ summary -> do
      let key = ownerKey (ms_mod summary)
          sourceKey = (key,ms_hsc_src summary == HsBootFile)
      unless (Map.member sourceKey freshSummaries || Map.member key selected) $
        liftIO (throwIO (ExecutionSourceIncomplete key))
      forM_ (Map.lookup sourceKey freshSummaries) $ \originalFresh -> do
        originalPath <- liftIO (traverse canonicalizePath (ml_hs_file (ms_location originalFresh)))
        executionPath <- liftIO (traverse canonicalizePath (ml_hs_file (ms_location summary)))
        unless (ms_mod summary == ms_mod originalFresh && executionPath == originalPath
            && ms_hs_hash summary == ms_hs_hash originalFresh) $
          liftIO (throwIO (ExecutionSourceChanged key))
    _ -> pure ()
  observations <- forM nodes $ \node -> do
    let key = executionIdentityKey (executionNodeIdentity node)
        original = executionNodeModule node
        source = dependencyModuleSource original
    summary <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key summaries)
    actual <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key currentNodes)
    canonical <- liftIO (traverse canonicalizePath (ml_hs_file (ms_location summary)))
    (sourceProof,fingerprint) <- liftIO (sourceEvidenceWithFingerprint source)
    unless (canonical == Just source && dependencySourceSha256 sourceProof == executionNodeSourceSha256 node
        && fingerprint == ms_hs_hash summary) $ liftIO (throwIO (ExecutionSourceChanged key))
    originalQuotes <- if xopt LangExt.QuasiQuotes (ms_hspp_opts summary)
      then do parsedOriginal <- parseModule summary
              pure (quasiQuoteOccurrences (ms_hspp_opts summary) (pm_parsed_source parsedOriginal))
      else pure []
    unless (not (hasUnconditionallyUntrackedCompileTimeExecution (ms_hspp_opts summary))
        && not (gopt Opt_Pp (ms_hspp_opts summary))
        && not (xopt LangExt.StaticPointers (ms_hspp_opts summary))
        && null originalQuotes) $
      liftIO (throwIO (ExecutionSourceUnsupported key))
    let expectedPath imported = case dependencyImportSelected imported of
          Just path' -> Just path'
          Nothing | (fst key,dependencyImportName imported) `elem` executionNodeRequirements node ->
            dependencyModuleSource . executionNodeModule <$> Map.lookup
              (fst key,dependencyImportName imported) selected
          Nothing -> Nothing
        tuple imported path' = (dependencyImportQualifier imported,dependencyImportName imported,
          dependencyImportBoot imported,path')
    let ordinaryNames = Set.fromList (map dependencyImportName (dependencyModuleImports original))
        originalExact = Set.fromList [owner | (sourceOwner,imports') <-
          executionGraphExactImports (executionNodeGraph node), sourceOwner == key, owner <- imports'
          , snd owner `Set.notMember` ordinaryNames]
        exactRows = [imported | imported <- dependencyModuleImports actual
          , (fst key,dependencyImportName imported) `Set.member` originalExact]
    unless (length exactRows == Set.size originalExact
        && Set.fromList [(fst key,dependencyImportName imported) | imported <- exactRows] == originalExact) $
      liftIO (throwIO (ExecutionSourceIncomplete key))
    exactImports <- forM exactRows $ \imported -> do
      let importedKey = (fst key,dependencyImportName imported)
      child <- maybe (liftIO (throwIO (ExecutionSourceIncomplete importedKey))) pure
        (Map.lookup importedKey selected)
      let path' = dependencyModuleSource (executionNodeModule child)
      unless (not (dependencyImportBoot imported)
          && (case dependencyImportQualifier imported of
                DependencyUnqualified -> True
                DependencyThisUnit unit -> unit == fst key
                DependencyOtherUnit _ -> False)
          && dependencyImportSelected imported == Just path') $
        liftIO (throwIO (ExecutionSourceResolutionChanged key))
      pure (tuple imported (Just path'))
    let expectedImports = sort (exactImports ++
          [tuple imported (expectedPath imported) | imported <- dependencyModuleImports original])
        actualImports = sort [tuple imported (dependencyImportSelected imported) | imported <- dependencyModuleImports actual]
    unless (expectedImports == actualImports) $
      liftIO (throwIO (ExecutionSourceImportResolutionChanged key expectedImports actualImports))
    -- Explicit source roots must not override an earlier current search-path
    -- candidate. Original negative-resolution witnesses remain live as well.
    let imports = Set.fromList [(dependencyImportQualifier imported,dependencyImportName imported,
          dependencyImportBoot imported) | imported <- dependencyModuleImports original]
        applicable resolution = (dependencyResolutionQualifier resolution,dependencyResolutionModule resolution,
          dependencyResolutionBoot resolution) `Set.member` imports
        currentResolutions = filter applicable (dependencyResolutions current)
        originalResolutions = filter applicable (dependencyResolutions
          (executionGraphEvidence (executionNodeGraph node)))
        negative resolution = case dependencyResolutionSelected resolution of
          Nothing -> dependencyResolutionCandidates resolution
          Just path' -> takeWhile (/= path') (dependencyResolutionCandidates resolution)
        originalNegative resolution
          | (fst key,dependencyResolutionModule resolution) `Set.member` originalNames = []
          | otherwise = negative resolution
    let negativePaths = nubOrd (concatMap negative currentResolutions ++ concatMap originalNegative originalResolutions)
    present <- liftIO (filterM doesFileExist negativePaths)
    unless (null present) $ liftIO (throwIO (ExecutionSourceSearchChanged key present))
    let proof = [(artifact,path',sha) | (artifact,path',sha) <- scopeInterfaces admitted
          , (exactUnit artifact,exactModule artifact) == key]
    packageProof <- case proof of
      [(artifact,path',sha)] -> liftIO (readPackageImports path' sha artifact)
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
    pure (concatMap negative currentResolutions ++ concatMap originalNegative originalResolutions,
      packageInterfaces packageProof)
  -- Native interface validity and executable loading use their respective
  -- profiles. No equality with a newly emitted bytecode interface is assumed.
  native <- liftIO (hydrateExactScope env interfaces)
  let nativeEnv = native {hsc_mod_graph=mapMG (\summary -> summary
        {ms_hspp_opts=canonicalizeDFlags (ms_hspp_opts summary)}) executionGraph}
  forM_ nodes $ \node -> do
    let key = executionIdentityKey (executionNodeIdentity node)
    summary <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key summaries)
    iface <- maybe (liftIO (throwIO (ExecutionSourceIncomplete key))) pure (Map.lookup key originalByOwner)
    decision <- liftIO (checkOldIface (scopeRetainedHscEnv (ms_mod summary) nativeEnv)
      summary {ms_hspp_opts=canonicalizeDFlags (ms_hspp_opts summary)} (Just iface))
    case decision of
      UpToDateItem _ -> pure ()
      OutOfDateItem _ _ -> liftIO (throwIO (ExecutionSourceChanged key))
  setSession env {hsc_targets=hsc_targets initial}
  pure (ValidatedOriginalSources executionGraph extraTargets current
    (nubOrd (concatMap fst observations)) (Set.toAscList (Set.fromList (concatMap snd observations))))

planExactExecutionLoad
  :: ExactScope -> [(ExactIfaceArtifact, ModIface)] -> [(ExactIfaceArtifact, ModIface)] -> ModuleName
  -> ModuleGraph -> ModuleGraph -> Ghc ExactExecutionPlan
planExactExecutionLoad admitted interfaces checkedInterfaces targetName sourceGraph ordinaryLoad = do
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
  if null roots then pure (ExactExecutionPlan ordinaryLoad Nothing [] [] sourceGraph [] []) else withLoadTargets (Just (hsc_targets initial)) $ do
    forM_ roots $ \key -> unless (isNothing (hscCompileCoreExprHook (hsc_hooks initial))) $
      liftIO (throwIO (ExecutionSourceUnsupported key))
    nodes <- either (liftIO . throwIO) pure (executionSourceClosure
      (scopeExecutionGraphs admitted) (scopeExecutionOwners admitted) (scopeExecutionNativeOwners admitted) roots)
    validation <- validateExactOriginalSources admitted interfaces sourceGraph nodes
    let selected = Map.fromList [(executionIdentityKey (executionNodeIdentity node),node) | node <- nodes]
        executionGraph = validatedOriginalGraph validation
        extraTargets = validatedOriginalTargets validation
        selectedNames = Set.fromList [mkModuleName name | (_,name) <- Map.keys selected]
        excluded = [mkModuleName (exactModule artifact) | (artifact,_,_) <- scopeInterfaces admitted
          , mkModuleName (exactModule artifact) `Set.notMember` selectedNames]
          ++ [mkModuleName (exactModule artifact) | artifact <- scopeValueInterfaces admitted]
    let shadowNames = Set.fromList [mkModuleName name | (_,name) <- Map.keys selected]
        blockedSeed = Set.fromList (targetName : excluded)
        deferred names =
          let grown = names `Set.union` Set.fromList [ms_mod_name summary
                | ModuleNode _ summary <- mgModSummaries' executionGraph
                , any ((`Set.member` names) . unLoc . snd) (ms_textual_imps summary)]
          in if grown == names then names else deferred grown
        blocked = deferred blockedSeed
        ordinaryNames = Set.fromList [ms_mod_name summary | ModuleNode _ summary <- mgModSummaries' ordinaryLoad]
        loadGraph = mkModuleGraph [graphNode | graphNode <- mgModSummaries' executionGraph
          , case graphNode of ModuleNode _ summary -> ms_mod_name summary `Set.member` Set.union shadowNames ordinaryNames
                              _ -> True]
        freshProviders = [summary | ModuleNode _ summary <- flattenSCCs (topSortModuleGraph True sourceGraph Nothing)
          , ms_mod_name summary `Set.notMember` ordinaryNames
          , ms_mod_name summary /= targetName
          , backendGeneratesCode (backend (ms_hspp_opts summary))]
    forM_ (Set.toList (Set.intersection shadowNames blocked)) $ \name ->
      liftIO (throwIO (ExecutionSourceUnsupported (unitString home,moduleNameString name)))
    forM_ freshProviders $ \summary -> unless
        (ms_mod_name summary `Set.notMember` blocked
          && null (ms_srcimps summary)
          && not (any (\case ModuleNode _ boot -> ms_hsc_src boot == HsBootFile && ms_mod boot == ms_mod summary
                             _ -> False) (mgModSummaries' sourceGraph))
          && not (xopt LangExt.StaticPointers (ms_hspp_opts summary))) $
          liftIO (throwIO (ExecutionSourceUnsupported (ownerKey (ms_mod summary))))
    pure (ExactExecutionPlan loadGraph (Just (hsc_targets initial ++ extraTargets)) nodes freshProviders executionGraph
      (validatedOriginalNegatives validation) (validatedOriginalPackages validation))

sessionVariant :: CompilePurpose -> SessionScope -> FilePath -> IO PipelineVariant
sessionVariant purpose scope path = do
  targetModName' <- targetModuleNameFor path
  completedValuesRef <- newIORef Nothing
  let effectivePurpose = originalPurpose purpose
      completedValues = case effectivePurpose of
        CheckedItemCompile _ _ values -> values
        HostActivationInputCompile _ _ values -> values
        ProgramItemCompile _ _ _ values -> values
        _ -> []
  capturedExact <- traverse (\manifest -> readExactScope manifest >>= either (ioError . userError) pure)
    (ssExactScope scope)
  let exact = case withoutGeneratedInstanceCheck purpose of
        PlannedDeclarationCheck _ admitted -> Just admitted
        CellProgramCompile _ admitted -> Just admitted
        _ -> capturedExact
  let hostPurpose = case effectivePurpose of
        HostActivationInputCompile {} -> True
        _ -> False
      hostAdmission = maybe False ((== Just HostActivationInput) . fmap itemPurpose . scopeCheckedItem) exact
  unless (hostPurpose == hostAdmission)
    (fail "host activation input has another compiler purpose")
  let checkPurpose = case effectivePurpose of
        HostActivationCheck signature -> Just signature
        _ -> Nothing
      checkAdmission = exact >>= scopeCheckedCell >>= \cell -> case checkedCellPurpose cell of
        HostInputCellCheck signature -> Just signature
        AuthoredCellCheck -> Nothing
  unless (checkPurpose == checkAdmission)
    (fail "host activation check has another compiler purpose")
  forM_ exact $ \admitted -> case capturedExact of
    Just original | scopeRequestSha256 original == scopeRequestSha256 admitted -> pure ()
    _ -> ioError (userError "planned declaration leaves its original compiler offer")
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
   , pvExactScope = exact
   , pvCompilerProducer = Nothing
   , pvGeneratedScaffold = generatedRecipe purpose
   , pvGeneratedInstanceCheck = generatedInstanceRecipe purpose
   , pvDownsweepExcludes = excludedOwners
   , pvTransformParsed = \env summary parsed -> do
       captured <- readIORef completedValuesRef
       transformed <- transformWithCompletedValues captured purpose targetModName' env summary parsed
       case scopeRequestTypes =<< exact of
         Just (recipe, signatures) | ms_mod_name summary == targetModName' ->
           thenNativeModule transformed (rewriteRequestTypes env recipe signatures)
         _ -> pure transformed
   , pvPlan = \timing modGraphRaw selectedExact -> do
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
      originalHooks <- hsc_hooks <$> getSession
      verifiedClosureRef <- liftIO (newIORef Nothing)
      scaffoldRef <- liftIO (newIORef noGeneratedScaffoldImports)
      injectMsRef <- liftIO (newIORef (0 :: Integer))
      executionPlan <- case selectedExact of
        Nothing -> pure (ExactExecutionPlan depGraph Nothing [] [] modGraphRaw [] [])
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
              liftIO (readGeneratedScaffoldImportAuthority closure'
                (scopeExecutionNativeOwners admitted) recipe parsedScaffold modGraphRaw hydrated)
                >>= either (liftIO . fail) pure
          liftIO (writeIORef scaffoldRef scaffold)
          preflight <- liftIO (installExactLexicalGraphWithScaffold modGraphRaw lexical checkedValues scaffold hydrated)
          _ <- either (liftIO . ioError . userError) pure preflight
          liftIO (writeIORef verifiedClosureRef (Just closure'))
          planExactExecutionLoad admitted interfaces checkedInterfaces targetModName' modGraphRaw depGraph
      let executionNodes = executionOriginalNodes executionPlan
      liftIO $ do
        emitCount timing "exact_execution_original_load_owners" (toInteger (length executionNodes))
        emitCount timing "exact_execution_fresh_provider_compiles" (toInteger (length (executionFreshProviders executionPlan)))
      pure CompilePlan
        -- Compile the turn's home-package SOURCE dependencies
        -- (@Tidepool.Prelude@, @Tidepool.Effects@, @Lib.G<g>@) into the HPT,
        -- but NOT the turn target itself. LoadAllTargets on depGraph (target
        -- filtered out above) — equivalent to the old @LoadDependenciesOf@
        -- but without compiling the target prematurely.
        { cpLoadGraph = executionLoadGraph executionPlan
        , cpLoadTargets = executionLoadTargets executionPlan
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
                   when timing $ forM_ (take 128 executionNodes) $ \node -> do
                     let key@(_,owner) = executionIdentityKey (executionNodeIdentity node)
                         loaded = lookupHpt (hsc_HPT hscMG) (mkModuleName owner)
                     liftIO $ hPutStrLn stderr $ "tidepool-exact-execution-load owner=" ++ show key
                       ++ " allow_object=False bytecode=" ++ show (maybe False (isJust . homeMod_bytecode . hm_linkable) loaded)
                       ++ " object=" ++ show (maybe False (isJust . homeMod_object . hm_linkable) loaded)
                   executable <- forM executionNodes $ \node -> do
                     let original = executionNodeIdentity node
                         key = executionIdentityKey original
                         name = mkModuleName (executionModule original)
                     case lookupHpt (hsc_HPT hscMG) name of
                       Just hmi | (unitString (moduleUnit (mi_module (hm_iface hmi))),
                           moduleNameString (moduleName (mi_module (hm_iface hmi)))) == key
                         , let links = hm_linkable hmi
                         , isJust (homeMod_bytecode links) || isJust (homeMod_object links)
                         , not (any usageFile (mi_usages (hm_iface hmi))) -> pure (name,links)
                       _ -> liftIO (throwIO (ExecutionSourceLinkableMissing key))
                   hydrated <- liftIO (hydrateExactScope hscMG interfaces)
                   let withExecutables = hscUpdateHPT (\table -> foldr
                         (\(name,links) result -> case lookupHpt result name of
                           Just hmi -> addToHpt result name hmi {hm_linkable=links}
                           Nothing -> result) table executable) hydrated
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
                   linked <- if null executionNodes then pure baseline else do
                     forM_ executionNodes $ \node ->
                       unless (isNothing (hscCompileCoreExprHook (hsc_hooks baseline))) $
                         liftIO (throwIO (ExecutionSourceUnsupported (executionIdentityKey (executionNodeIdentity node))))
                     let executionKeys = Set.fromList (map (executionIdentityKey . executionNodeIdentity) executionNodes)
                         shadows = [node | node@(ModuleNode _ summary) <- mgModSummaries' (executionLinkGraph executionPlan)
                           , (unitString (moduleUnit (ms_mod summary)),moduleNameString (ms_mod_name summary))
                             `Set.member` executionKeys]
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
                     hmi <- liftIO (compileOne' (Just batchMsg) current
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
                    Just admitted | isJust (scopeCheckedCell admitted) || isJust (scopeCheckedItem admitted)
                        || isJust (scopeCheckedDisplay admitted) || isJust (scopeCheckedInspection admitted) -> do
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
                          hydrated <- injectSessionScope (scope { ssValIfaces = needed }) hsc0
                          pure (hydrated, Nothing)
                setSession hscInjected
                liftIO $ do
                  forM_ completedCap $ \captured -> writeIORef completedValuesRef (Just captured)
                  modifyIORef' injectedRef
                    (`Set.union` Set.fromList (map renderSessionModule needed
                      ++ [mkModuleName (completedValueModule value) | captureCompleted, value <- completedValues]))
                  modifyIORef' injectMsRef (+ injectMs)
        , cpTier = OptimizeEveryModule
        , cpBeforeMerge =
            do liftIO (readIORef injectMsRef >>= emitPhase timing "inject")
               forM_ executionNodes $ \node -> do
                 actual <- liftIO (sourceEvidenceWithFingerprint (dependencyModuleSource (executionNodeModule node)))
                 unless (dependencySourceSha256 (fst actual) == executionNodeSourceSha256 node) $
                   liftIO (throwIO (ExecutionSourceChanged (executionIdentityKey (executionNodeIdentity node))))
               forM_ (executionNegativePaths executionPlan) $ \path' -> do
                 exists <- liftIO (doesFileExist path')
                 when exists $ liftIO (throwIO (ExecutionSourceResolutionChanged ("",path')))
               env <- getSession
               forM_ (executionPackageRoots executionPlan) $ \root -> do
                 unchanged <- liftIO (validatePackageImportRoot env root)
                 either (const (liftIO (throwIO (ExecutionSourceResolutionChanged
                   (packageUnit root,packageModule root))))) pure unchanged
        , cpFinalEnv = \env -> hscUpdateFlags canonicalizeDFlags env {hsc_hooks=originalHooks}
        }
   }
  where
    usageFile UsageFile{} = True
    usageFile _ = False
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
-- field declaration @f Int@ (which 'dataConOrigArgTys' can never resolve to
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
isClosureType ty0 =
  let (_, _, body) = tcSplitSigmaTy ty0
  in goT emptyUniqSet body
  where
    goT :: UniqSet TyCon -> Type -> Bool
    goT visited ty
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
              fieldHit = case tyConDataCons_maybe tc of
                Just dcs -> any (\dc -> any (\(Scaled _ ft) -> goT visited' ft)
                                             (dataConOrigArgTys dc)) dcs
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

-- | Every external module referenced by a module's top-level binding RHSs.
-- This graph-independent fact stays valid when a later request changes the
-- set of home modules; reachability intersects it with that request's graph.
moduleRefs :: ModGuts -> Set.Set ModuleName
moduleRefs guts = Set.unions (map rhsModules (mg_binds guts))
  where
    rhsModules (NonRec _ rhs) = externalVarModules rhs
    rhsModules (Rec ps)       = Set.unions [ externalVarModules rhs | (_, rhs) <- ps ]

-- | Every home module (restricted to @known@) referenced by a real 'Var'
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
externalVarModules :: CoreExpr -> Set.Set ModuleName
externalVarModules = go
  where
    go expr = case expr of
      Var v -> case nameModule_maybe (idName v) of
        Just m -> Set.singleton (moduleName m)
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
-- Why re-applied: GHC 9.12's @enableCodeGenForTH@ downgrades the DynFlags of
-- home modules whose code is needed for splices (QuasiQuotes/TH) so 'load'
-- can provision bytecode — interpreter backend, -O0. That downgrade is
-- correct for the load phase (splices run against dep bytecode), but it
-- persists in each ModSummary's @ms_hspp_opts@, which the extraction loop
-- re-uses. Without re-canonicalizing, extraction of any module in a
-- quasi-quote dependency graph emits UNOPTIMIZED Core — e.g.
-- @negate \@Double $fNumDouble (D# 2.5##)@ instead of a folded @D# -2.5##@,
-- which then requires runtime machinery the optimized program did not.
--
-- Surgical: backend/opt-level/gopt only — exactly the fields the TH
-- downgrade touches. Per-module LANGUAGE pragmas already merged into
-- @ms_hspp_opts@ are preserved. Platform spoofing and @importPaths@ are
-- session-setup-only (see runPipeline): re-pinning bare genericPlatform
-- here would strip the platform constants populated at session init.
--
-- Flag contracts:
--   * FullLaziness conflicts with eager eval.
--   * Opt_CprAnal is ineffective for this GHC version, but keeping it unset
--     records the intended extraction profile. Worker-wrapper remains enabled.
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
  gopt_set (gopt_set (gopt_unset (gopt_unset (updOptLevel 2 $ dflags
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
