module CandidateExecutionSourcesTest (candidateExecutionSourcesTest) where

import ExecutionSourceDecodeTest (executionSourceDecodeChecks, executionSourceDecodeBenchmark, executionSourceDecodeSnapshots, executionSourceResolutionBudgetChecks)
import ExactScopeV9Test (exactScopeV9Checks, nativeOriginChecks, candidateCanonicalChecks)
import CandidateGraphDescriptorTest (candidateGraphDescriptorChecks)
import GenuineCandidateFixture
  ( writeGenuineCandidateManifestFor, writeGenuineMetadataScope, writeGenuineEmptyMetadataScope
  , writeGenuineCandidateNativeScope, writeGenuineCandidateLexicalScope, writeGenuineAuthoredDeclarationScope
  , writeGenuineOriginalExecutionScope, writeGenuineExecutionScope )

import Codec.CBOR.Encoding (encodeBool, encodeListLen, encodeString)
import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Data.ByteString.Lazy qualified as BSL
import Control.Exception (SomeException, IOException, AsyncException(ThreadKilled), bracket, evaluate, finally, try, fromException)
import Control.Concurrent (MVar, forkIO, killThread, myThreadId, throwTo, threadDelay, newEmptyMVar, putMVar, takeMVar)
import Control.Monad (foldM, forM, forM_, unless, void, when)
import GHC.Clock (getMonotonicTimeNSec)
import Data.Word (Word32, Word64)
import Data.IORef (newIORef, readIORef, writeIORef, modifyIORef')
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Char8 qualified as BSC
import Data.List (isInfixOf, isPrefixOf, sort, sortOn, stripPrefix)
import Data.Maybe (catMaybes, isJust, isNothing, maybeToList)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import GHC (runGhc, getSession, setSession, SafeHaskellMode(Sf_None), ms_mod_name, ms_hsc_src, ms_hspp_buf, ms_hspp_file, ms_hspp_opts, parseModule, typecheckModule, TypecheckedModule(..), ParsedModule(..), Target(..))
import GHC.Core qualified as Core
import GHC.Builtin.Types (boolTy, intTy, charTy, stringTy, intDataCon)
import GHC.Core.Type (mkVisFunTyMany, mkTyVarTy, mkForAllTy)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Builtin.Types (liftedTypeKind)
import GHC.Types.Var (mkTyVar, VarBndr(..))
import GHC.Types.Name.Occurrence (mkTyVarOcc, mkVarOcc)
import GHC.Types.SrcLoc (noSrcSpan)
import Language.Haskell.Syntax.Specificity (ForAllTyFlag(..), Specificity(..))
import GHC.Types.Id (idName, idType, setIdName)
import GHC.Types.Literal (Literal(..), LitNumType(..))
import GHC.Types.Name (getOccString, nameOccName, nameSrcSpan, mkExternalName, mkInternalName)
import GHC.Types.Avail (availNames)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Types.Unique.Supply (mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import GHC.Driver.Env (HscEnv(..), hsc_HPT, hscUpdateHPT)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), HomeModLinkable(..), lookupHpt, addToHpt)
import GHC.Utils.Logger (Logger, popLogHook)
import GHC.Unit.Finder (initFinderCache, addModuleToFinder)
import GHC.Unit.Finder.Types (FinderCache(..))
import GHC.Unit.Module.Location (ml_hi_file)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Tc.Types (tcg_imports, tcg_type_env, tcg_mod)
import GHC.Unit.Module.Deps (imp_mods, Usage(..))
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries', mkModuleGraph)
import GHC.Types.SourceFile (HscSource(..))
import Control.Monad.IO.Class (liftIO)
import GHC.Driver.Session (importPaths, targetProfile, wopt_set, xopt, WarningFlag(Opt_WarnMissingSignatures))
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Types.Error (isEmptyMessages)
import GHC.Types.Name.Set (nameSetElemsStable)
import GHC.Driver.Hooks (hscCompileCoreExprHook, runMetaHook)
import GHC.Iface.Make (mkIfaceTc)
import GHC.Iface.Recomp (MaybeValidated(..), checkOldIface)
import GHC.Iface.Tidy (mkBootModDetailsTc)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Unit.Module.ModIface (set_mi_module, mi_module, mi_exports, mi_usages, mi_decls)
import GHC.Iface.Syntax (ifaceDeclImplicitBndrs)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModGuts (cg_binds)
import GHC.Unit.Module (Module, mkModule, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString, unitIdString, stringToUnit, GenWithIsBoot(..))
import Numeric (showHex)
import System.Directory
  ( copyFile, createDirectory, createDirectoryIfMissing, getTemporaryDirectory, removeDirectoryRecursive
  , removeFile, renameFile, listDirectory, doesFileExist, getPermissions, setPermissions, executable
  , getModificationTime, setModificationTime, withCurrentDirectory, getCurrentDirectory, canonicalizePath )
import System.Environment (getArgs, getExecutablePath, setEnv, lookupEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>), takeDirectory, normalise, replaceExtension, addTrailingPathSeparator)
import System.IO (hClose, hFlush, hPutStrLn, hSeek, hSetFileSize, withBinaryFile, IOMode(WriteMode), SeekMode(AbsoluteSeek), openTempFile, stderr)
import System.IO.Error (isDoesNotExistError, ioeGetFileName)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.Process (readProcessWithExitCode)
import System.Timeout (timeout)
import Tidepool.CertifiedProducts (encodeCertifiedProducts, resolvePackageGlobal)
import Tidepool.FinalizedModuleArtifacts (captureFinalizedModuleArtifacts, emptyFinalizedModuleArtifacts)
import Tidepool.FinalizedModule (finalizedHomeModInfo, homeInterfaceUsageOwners)
import Tidepool.ExecutionEncode (encodeModuleProducts)

import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), projectPreparedModuleGroups
  , projectPreparedModuleProducts, projectOriginalHomeModuleProducts, preparedModuleProductOutcomes, closeUnavailableOriginalGroups, topBinders
  , ReferenceFact(..), preparedModuleReferenceFacts, preparedRootIdentity, projectPrepared )
import Tidepool.ExecutionProjection (resolveTextPackageUnit)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.PreparedJson (resolveJsonAuthority)
import Tidepool.PreparedSites (SiteRejection(..), resolvePreparedInterfaceSiblings, lookupPreparedVerb)
import Tidepool.SiteClassifier (SiteFailure(..), classifySiteOccurrence)
import Tidepool.EffectSchema (YieldSite(..), SiteType(..))
import GHC.Driver.Env (hsc_home_unit)
import GHC.Unit.Home (isHomeUnit)
import GHC.Core.DataCon (dataConWorkId, dataConTyCon)
import GHC.Core.TyCon (tyConDataCons)
import Tidepool.PreparedFacts (PreparedFacts(..), extractPreparedFacts)
import GHC.Types.Name (nameModule_maybe)
import GHC.Types.Var (varName)
import Tidepool.CompileInput (writeCompileInputProof)
import Tidepool.DiagJson (InputRejection(..))
import Tidepool.ExecutionSchema
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencyImport(..)
  , DependencyResolution(..), ProductAvailability(..), DependencySource(..), sourceEvidence
  , DependencyQualifier(..), renderDependencyQualifier
  , selectedHomeRequirements, renderDependencyEvidence )
import Tidepool.ExactHydration
  ( CheckedTemplateInterface(..), newOriginalInterfaceArtifacts, originalInterfaceBytes, ExactIfaceArtifact(..), freshExactState, noCheckedValueImports, installExactLexicalGraph
  , readCheckedValueImportAuthority, readExactIfaceArtifacts, hydrateExactScope
  , readVerifiedExactIfaceClosure, readVerifiedExactIfaceClosureWithCheckedValues
  , selectVerifiedExactInterfaces, selectVerifiedValueInterfaces, checkedValueImportAuthorityFromVerified )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.HomeProducts (hydrateCandidateHomeProducts, CandidateCoreFailure(..))
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), PipelineResult(..), CheckedEnvironmentResult(..)
  , finalizedTidyGuts
  , renderType, generatedScaffoldRecipe, activationPreviewInputType, withSourceImportIntents
  , CompilePurpose(..), runPipelineSelected, runPipelineSessionSelected, withResidentPipelineSelected
  , withResidentPipelineSelectedRequests )
import Tidepool.ModuleCandidates (ModuleCandidate(..), CandidateGroup(..), CandidateGlobal(..)
  , readModuleCandidates, readModuleCandidatesWithGraphs, candidateExecutionSources, candidateOriginalIdentity
  , candidateCoreDescriptor)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..), encodePackageImports, emptyPackageImports, readPackageImports, revalidatePackageImports)
import Tidepool.PreparedStg (PreparedModule(..), PreparedCoverage(..))
import Tidepool.FatIface (readExactInterface)
import Tidepool.Session (SessionScope(..), emptySessionScope)
import Tidepool.RetainedUnfoldings (scopeRetainedSummaryHscEnv)
import Tidepool.SessionArtifacts (mkBoundBinders, parseValModule)
import Tidepool.Session (sessionHiPath, Generation(..), SessionModule(..), SessionModuleKind(..))
import Tidepool.ExactScope
  ( ExactScope(..), ExactScopePurpose(..), ExactProduct(..), ExactOriginalGroup(..)
  , CheckedCellAdmission(..), CheckedCellPurpose(..), CheckedItemAdmission(..), CheckedItemPurpose(..)
  , scopeCheckedCell, scopeCheckedItem, scopeIncludePaths, readExactScope
  , scopeDurableInterfaces, canonicalCoreArtifact, canonicalCorePath
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalRequirements
  , originalGroupFromCandidate
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget, scopeExecutionNativeOwners )
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import ProgressBoundaryTest (progressBoundaryChecks, watchReplyEvidenceChecks, watchReplyWarmAuthorityChecks)
import FinalizedCoreTest (finalizedCoreChecks)
import Tidepool.CheckedCell (CheckedSignature(..), RequestTypeSignatures(..), RequestHelperRecipe(..), captureCheckedSignature, encodeCheckedSignature, encodeRequestTypeSignatures
  , captureCheckedTypeWitness, sealCheckedTypeWitness, encodeCheckedTypeWitness, rewriteCheckedAnnotations, rewriteHostInputType, rewriteRequestTypes, NativeParsedModule(..), thenNativeModule, typecheckNativeModule, typecheckNativeModuleWithDiagnostics)
import Tidepool.TurnSource (replaceTemplateMarker, spliceTemplate)
import Tidepool.Binders (BoundBinder(..), analyzeCellWithFlags, CellSourcePlan(..))
import Tidepool.ExecutionSource
  ( WorkerExecutionSource(OrdinaryExecutionSource)
  , ExecutionSourceIdentity(..), ExecutionSourceOwner(..), ExecutionSourceRef(..), ExecutionSourceGraph(..), ExecutionSourceNode(..)
  , executionSourceClosure, executionSourceOriginalNode, executionSourceOriginalClosure, executionIdentityKey
  , ExecutionSourceFailure(..), ExecutionSourceRecipe(..), issueExecutionSourceRecipe, executionSourceProspectiveReferences
  , executionNodeOriginalResolutions
  , decodeExecutionSourceGraph
  , executionSourceGraphBytesLimit )

import SourceBootFixtureSupport

candidateExecutionSourcesTest :: IO ()
candidateExecutionSourcesTest = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs","MetadataQuoter.hs","ExecutionReexportFacade.hs","ExecutionReexportTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionReexportFacade.hs") [work] Nothing
  let sourceScopePath = work </> "original-scope.cbor"
      candidatePath = manifest work
  writeExecutionScope sourceScopePath work original ["ExecutionReexportFacade"]
  originalScope <- readExactScope sourceScopePath >>= either fail pure
  originalTerm <- readTerm sourceScopePath
  parcel <- case originalTerm of
    TList [_,_,_,_,_,_,_,TList [_,references],_] -> pure (TList
      [TList [TList [TString (T.pack (executionGraphSha256 graph)),TBytes (executionGraphBytes graph)]
        | graph <- scopeExecutionGraphs originalScope],references])
    _ -> fail "candidate fixture lacks original execution parcel"
  let owners = ["MetadataQuoteSupport","MetadataQuoter"]
  writeManifestFor owners work original
  descriptors <- readTerm candidatePath >>= \case
    TList [_,_,_,_,TList rows,_] -> pure rows
    _ -> fail "candidate fixture lacks source descriptors"
  rows <- forM descriptors $ \case
    TList fields@(TString _:TString name:_) -> do
      product' <- case [value | value <- scopeProducts originalScope, originalModule value == T.unpack name] of
        [value] -> pure value
        _ -> fail "candidate fixture lacks actual original product"
      prepared <- case [value | value <- pprModules original, moduleName (pmModule value) == mkModuleName (T.unpack name)] of
        [value] -> pure value
        _ -> fail "candidate fixture lacks paired prepared body"
      let context = ProjectionContext "test" "matched"
            (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
            (SymbolIdentity "main" name "value" "answerValue" Nothing) [] Nothing Nothing Nothing Nothing
      groups <- either (fail . show) pure (projectPreparedModuleGroups context prepared)
      pure (TList [case index of
        6 -> TString (T.pack (originalVersion product'))
        7 -> TString (T.pack (originalProductSha256 product'))
        10 -> TList (map candidateGroupTerm groups)
        13 -> TString (T.pack (originalProductPath product'))
        _ -> field | (index,field) <- zip [0::Int ..] fields])
    _ -> fail "candidate fixture has malformed descriptor"
  (symbols,globals,compactRows) <- either fail pure (compactInventoryRows rows)
  let filteredParcel = case parcel of
        TList [graphs,TList refs] -> TList [graphs,TList [reference | reference@(TList (_:TString name:_)) <- refs
          , T.unpack name `elem` owners]]
        _ -> parcel
      envelope value = TList [TString "TPMCAN",TString "8",symbols,globals,TList compactRows,value]
      writeTerm path value = BS.writeFile path (toStrictByteString (encodeTerm value))
  writeTerm candidatePath (envelope filteredParcel)
  offered <- readModuleCandidates candidatePath >>= either fail pure
  unless (length offered == 2 && all (isJust . candidateExecutionSources) offered) $
    fail "candidate reader lost original execution provenance"
  accepted <- runPipelineSessionSelected (PreparedProducts (Just candidatePath)) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionReexportFacade.hs") [work] Nothing
  unless (Set.fromList (map candidateModule (pprAcceptedCandidates accepted)) == Set.fromList owners) $
    fail "real GHC admission did not accept the proven source-selected originals"
  -- Keep the dependency as an exact original while offering its importer.
  -- Root replacement remains forbidden even though its tuple is identical.
  let crossoverScopePath = work </> "candidate-original-scope.cbor"
      crossoverScope = emptySessionScope {ssRoot=work,ssExactScope=Just crossoverScopePath}
      helperName = "MetadataQuoteSupport"
      helperRows rows' = [row | row@(TList (_:TString name:_)) <- rows', name == helperName]
      crossoverTerm lexical = case originalTerm of
        TList fields -> TList [case (index,field) of
          (4,TList rows') -> TList (helperRows rows')
          (5,_) -> TList [TList [TList [TString "main",TString helperName],TList []] | lexical]
          (6,TList rows') -> TList (helperRows rows')
          (7,TList [graphs,TList refs]) -> TList [graphs,TList (helperRows refs)]
          (8,_) -> TList [TString "cell-check2",TString (T.replicate 64 "0")
            ,TString (T.replicate 64 "0"),TString (T.replicate 64 "0")
            ,TList [],TList [],TList [],TList [],TList [TString (T.pack work)]]
          _ -> field | (index,field) <- zip [0::Int ..] fields]
        _ -> originalTerm
      crossover = runPipelineSessionSelected (PreparedProducts (Just candidatePath)) Set.empty
        CertifyHomeProductsCompile (Just crossoverScope)
        (work </> "ExecutionReexportFacade.hs") [work] Nothing
      requireImporter result = unless
        (map candidateModule (pprAcceptedCandidates result) == ["MetadataQuoter"]
          && "MetadataQuoteSupport" `notElem` preparedNames result) $
        fail "exact dependency reuse rejected its importer or replaced the protected original"
  writeTerm crossoverScopePath (crossoverTerm True)
  crossoverAccepted <- crossover
  requireImporter crossoverAccepted
  let helperSource = work </> "MetadataQuoteSupport.hs"
  bracket (BS.readFile helperSource <* removeFile helperSource) (BS.writeFile helperSource) $ \_ -> do
    sourceFreeOriginal <- crossover
    requireImporter sourceFreeOriginal
  let sharedGraphParcel = case filteredParcel of
        TList [_,references] -> TList [TList [],references]
        other -> other
  writeTerm candidatePath (envelope sharedGraphParcel)
  readModuleCandidates candidatePath >>= \case
    Left _ -> pure ()
    Right _ -> fail "candidate manifest borrowed an unavailable original graph"
  sharedCandidates <- readModuleCandidatesWithGraphs (scopeExecutionGraphs originalScope) candidatePath
    >>= either fail pure
  unless (length sharedCandidates == 2) $ fail "combined exact graph inventory lost candidate recipes"
  sharedGraphAccepted <- crossover
  requireImporter sharedGraphAccepted
  writeTerm candidatePath (envelope filteredParcel)
  -- Exact-produced evidence keeps its authenticated home edge in the graph,
  -- outside the ordinary import rows. Preserve the real GHC source/body pair.
  let exactRows = [case row of
        TList fields@(_:TString "MetadataQuoter":_) -> TList [case (index,field) of
          (9,TList imports) -> TList [edge | edge@(TList (_:TString name:_)) <- imports, name /= helperName]
          _ -> field | (index,field) <- zip [0::Int ..] fields]
        other -> other | row <- compactRows]
  exactParcel <- case filteredParcel of
    TList [TList [TList [_,TBytes graphBytes]],TList refs] -> do
      graph <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict graphBytes))
      let removeOriginal node = case node of
            TList fields@(_:TString "MetadataQuoter":_) -> TList [case (index,field) of
              (4,TList imports) -> TList [edge | edge@(TList (_:TString name:_)) <- imports, name /= helperName]
              _ -> field | (index,field) <- zip [0::Int ..] fields]
            other -> other
          exactGraph = case graph of
            TList fields -> TList [case (index,field) of
              (7,TList evidence) -> TList [if column == 4 then case value of
                TList modules -> TList (map removeOriginal modules)
                other -> other else value | (column,value) <- zip [0::Int ..] evidence]
              (9,_) -> TList [TList [TString "main",TString "MetadataQuoter"
                ,TList [TList [TString "main",TString helperName]]]]
              _ -> field | (index,field) <- zip [0::Int ..] fields]
            other -> other
          exactBytes = toStrictByteString (encodeTerm exactGraph)
          exactSha = TString (T.pack (digest exactBytes))
          references = [TList [if index == 5 then exactSha else field
            | (index,field) <- zip [0::Int ..] fields] | TList fields <- refs]
      pure (TList [TList [TList [exactSha,TBytes exactBytes]],TList references])
    _ -> fail "candidate fixture lacks one authenticated graph"
  writeTerm candidatePath (TList [TString "TPMCAN",TString "8",symbols,globals,TList exactRows,exactParcel])
  graphOriginalAccepted <- crossover
  requireImporter graphOriginalAccepted
  writeTerm candidatePath (envelope filteredParcel)
  let reservedScope = case crossoverTerm True of
        TList fields -> TList [case (index,field) of
          (8,TList auth) -> TList [if column == 6 then TList [TString "MetadataQuoter"] else entry
            | (column,entry) <- zip [0::Int ..] auth]
          _ -> field | (index,field) <- zip [0::Int ..] fields]
        other -> other
  writeTerm crossoverScopePath reservedScope
  reserved <- crossover
  unless (null (pprAcceptedCandidates reserved) && "MetadataQuoter" `elem` preparedNames reserved) $
    fail "reserved source owner was admitted as a cached replacement root"
  -- Actual source selection, rather than inventory membership, authorizes
  -- the same dependency when it was initially hidden from lexical imports.
  writeTerm crossoverScopePath (crossoverTerm False)
  crossoverSelected <- crossover
  requireImporter crossoverSelected
  let unsealedScope = case crossoverTerm False of
        TList fields -> TList [if index == 8 then TNull else field
          | (index,field) <- zip [0::Int ..] fields]
        other -> other
  writeTerm crossoverScopePath unsealedScope
  hidden <- try crossover :: IO (Either SomeException PreparedPipelineResult)
  unless (case hidden of
      Left reason -> "OriginalSourceSelectionRejected" `isInfixOf` show reason
      Right _ -> False) $
    fail "hidden inventory original became visible without current source-selection authority"
  writeTerm crossoverScopePath (crossoverTerm False)
  -- Without the importer's authenticated recipe, raw path normalization is
  -- forbidden and source compilation remains the safe fallback.
  writeTerm candidatePath (envelope (TList [TList [],TList []]))
  unproven <- crossover
  unless (null (pprAcceptedCandidates unproven) && "MetadataQuoter" `elem` preparedNames unproven) $
    fail "exact dependency without a recipe bypassed current compilation"
  writeTerm candidatePath (envelope filteredParcel)
  copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
  differentOriginal <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "MetadataQuoteSupport.hs") [work] Nothing
  writeExecutionScope crossoverScopePath work differentOriginal [T.unpack helperName]
  mismatched <- crossover
  unless (null (pprAcceptedCandidates mismatched) && "MetadataQuoter" `elem` preparedNames mismatched) $
    fail "cached importer admitted a different current exact dependency tuple"
  changed <- runPipelineSessionSelected (PreparedProducts (Just candidatePath)) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionReexportFacade.hs") [work] Nothing
  unless (null (pprAcceptedCandidates changed)) $
    fail "candidate execution provenance bypassed current source validation"
  copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
  let emptyExecution = originalScope {scopeExecutionGraphs=[],scopeExecutionOwners=[]}
      parcels = [value | candidate <- pprAcceptedCandidates accepted, Just value <- [candidateExecutionSources candidate]]
  promoted <- either (fail . show) pure
    (extendExactExecutionSources (concatMap fst parcels) (map snd parcels) emptyExecution)
  unless (length (scopeExecutionOwners promoted) == 2
      && scopeLexical promoted == scopeLexical emptyExecution
      && scopeInterfaces promoted == scopeInterfaces emptyExecution) $
    fail "candidate execution promotion changed lexical/interface authority"
  unless (extendExactExecutionSources (concatMap fst parcels) (map snd parcels) promoted == Right promoted) $
    fail "identical original candidate execution promotion conflicts"
  shared <- either (fail . show) pure (executionSourceClosure (scopeExecutionGraphs promoted)
    (scopeExecutionOwners promoted) (scopeExecutionNativeOwners promoted)
    [("main","MetadataQuoter"),("main","MetadataQuoteSupport")])
  unless (length shared == 2) (fail "two roots from one original cycle lost their shared helper")
  helperOriginal <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "MetadataQuoteSupport.hs") [work] Nothing
  let helperScopePath = work </> "helper-only-original.cbor"
  writeExecutionScope helperScopePath work helperOriginal ["MetadataQuoteSupport"]
  helperScope <- readExactScope helperScopePath >>= either fail pure
  -- The helper's independent receipt has a different graph digest but the
  -- same exact tuple; fresh parent edges retain their own graph provenance.
  copyFile helperScopePath crossoverScopePath
  independentlyAuthenticated <- crossover
  requireImporter independentlyAuthenticated
  helperReference <- case scopeExecutionOwners helperScope of
    [value] -> pure value
    _ -> fail "helper-only source cycle has another native owner"
  unless (executionRefIdentity helperReference `elem` scopeExecutionNativeOwners promoted) $
    fail "separate GHC helper source cycle did not preserve its actual native/interface pairing"
  let mixedGraphs = scopeExecutionGraphs promoted ++ scopeExecutionGraphs helperScope
      mixedReferences = [if executionIdentityKey (executionRefIdentity reference) == ("main","MetadataQuoteSupport")
        then helperReference else reference | reference <- scopeExecutionOwners promoted]
  differentLocal <- either (fail . show) pure (executionSourceClosure mixedGraphs mixedReferences
    (scopeExecutionNativeOwners promoted) [("main","MetadataQuoter")])
  unless (length differentLocal == 2) (fail "fresh local helper borrowed or required its separately authenticated graph")
  differentShared <- either (fail . show) pure (executionSourceClosure mixedGraphs mixedReferences
    (scopeExecutionNativeOwners promoted) [("main","MetadataQuoter"),("main","MetadataQuoteSupport")])
  unless (length differentShared == 2) (fail "equivalent local recipes from different original cycles did not share their helper")
  let conflictingGraphs = [if executionGraphSha256 graph == executionRefGraph helperReference
        then graph {executionGraphEvidence=(executionGraphEvidence graph) {
          dependencySources=[source {dependencySourceSha256=replicate 64 'f'}
            | source <- dependencySources (executionGraphEvidence graph)]}}
        else graph | graph <- mixedGraphs]
  case executionSourceClosure conflictingGraphs mixedReferences (scopeExecutionNativeOwners promoted)
      [("main","MetadataQuoter"),("main","MetadataQuoteSupport")] of
    Left _ -> pure ()
    Right _ -> fail "two roots silently selected conflicting same-owner original source recipes"
  quoterRef <- case [reference | reference <- scopeExecutionOwners promoted
      , executionIdentityKey (executionRefIdentity reference) == ("main","MetadataQuoter")] of
    [reference] -> pure reference
    _ -> fail "shared-recipe fixture lacks one quoter reference"
  quoterNode <- either (fail . show) pure (executionSourceOriginalNode mixedGraphs
    (executionRefIdentity quoterRef) (executionRefGraph quoterRef))
  let originalGraph = executionNodeGraph quoterNode
      applies row = any (\edge -> dependencyImportQualifier edge == dependencyResolutionQualifier row
        && dependencyImportName edge == dependencyResolutionModule row
        && dependencyImportBoot edge == dependencyResolutionBoot row)
        (dependencyModuleImports (executionNodeModule quoterNode))
      originalEvidence = executionGraphEvidence originalGraph
      alternateGraph = originalGraph {executionGraphSha256=replicate 64 'd',
        executionGraphEvidence=originalEvidence {dependencyResolutions=
          [if applies row then row {dependencyResolutionCandidates=
              (work </> "unproven-shadow.hs") : dependencyResolutionCandidates row}
            else row | row <- dependencyResolutions originalEvidence]}}
  unless (any applies (dependencyResolutions originalEvidence)) $
    fail "shared-recipe fixture lacks applicable negative-resolution witnesses"
  contextualNodes <- either (fail . show) pure (executionSourceOriginalClosure (alternateGraph:mixedGraphs)
    [quoterRef,quoterRef {executionRefGraph=executionGraphSha256 alternateGraph}])
  contextualQuoter <- case [node | node <- contextualNodes
      , executionNodeIdentity node == executionRefIdentity quoterRef] of
    [node] -> pure node
    _ -> fail "shared source recipe lost its exact quoter owner"
  contextualResolutions <- either (fail . show) pure
    (executionNodeOriginalResolutions (alternateGraph:mixedGraphs) contextualQuoter)
  unless (Set.fromList [executionGraphSha256 originalGraph,executionGraphSha256 alternateGraph]
        `Set.isSubsetOf` executionNodeOriginalGraphs contextualQuoter
      && work </> "unproven-shadow.hs" `elem` concatMap dependencyResolutionCandidates contextualResolutions) $
    fail "shared source dedup discarded another recipe's negative-resolution constraints"
  -- Each level shares both later levels. Revalidating settled recipes per
  -- incoming path expands this bounded source inventory exponentially.
  let dagNames = ["SharedRecipe" ++ show index | index <- [0::Int ..35]]
      dagIdentity name = (executionRefIdentity helperReference) {executionModule=name}
      dagPath name = work </> name ++ ".hs"
      dagModules = [DependencyModule "main" name False (dagPath name)
          [DependencyImport DependencyUnqualified child False (Just (dagPath child))
            | child <- take 2 (drop (index+1) dagNames)] ProductReady
        | (index,name) <- zip [0::Int ..] dagNames]
      dagGraph = originalGraph {executionGraphSha256=replicate 64 'c',
        executionGraphOwners=[ExecutionSourceOwner (dagIdentity name) True Nothing | name <- dagNames],
        executionGraphExactImports=[],executionGraphEvidence=originalEvidence {
          dependencySources=[DependencySource (dagPath name) (replicate 64 'a') | name <- dagNames],
          dependencyModules=dagModules,dependencyResolutions=[]}}
      dagRefs=[ExecutionSourceRef (dagIdentity "SharedRecipe0") (executionGraphSha256 dagGraph)]
  dagResult <- timeout 2000000 $ evaluate $ case executionSourceClosure [dagGraph] dagRefs
      (map dagIdentity dagNames) [("main","SharedRecipe0")] of
    Left refusal -> Left refusal
    Right nodes -> Right (length nodes)
  unless (dagResult == Just (Right 36)) $
    fail "shared source recipe DAG did not finish with exactly 36 owners inside its bounded traversal"
  let providerRefs = [reference | (_,reference) <- parcels
        , executionIdentityKey (executionRefIdentity reference) == ("main","MetadataQuoter")]
  local <- either (fail . show) pure
    (extendExactExecutionSources (concatMap fst parcels) providerRefs emptyExecution)
  localNodes <- either (fail . show) pure (executionSourceClosure (scopeExecutionGraphs local)
    (scopeExecutionOwners local) (scopeExecutionNativeOwners local) [("main","MetadataQuoter")])
  unless (length (scopeExecutionOwners local) == 1 && length localNodes == 2) $
    fail "fresh local source recipe incorrectly required a separately published helper capability"
  let missingHelper = emptyExecution {scopeProducts=
        filter ((/= "MetadataQuoteSupport") . originalModule) (scopeProducts emptyExecution)}
  unavailable <- either (fail . show) pure
    (extendExactExecutionSources (concatMap fst parcels) providerRefs missingHelper)
  unless (null (scopeExecutionOwners unavailable) && scopeProducts unavailable == scopeProducts missingHelper
      && case executionSourceClosure (scopeExecutionGraphs unavailable) (scopeExecutionOwners unavailable)
          (scopeExecutionNativeOwners unavailable) [("main","MetadataQuoter")] of Left _ -> True; _ -> False) $
    fail "missing dependency capability either rejected native inventory or authorized an unavailable execution root"
  let noProducts = emptyExecution {scopeProducts=[]}
  unless (case extendExactExecutionSources (concatMap fst parcels) (map snd parcels) noProducts of Left _ -> True; _ -> False) $
    fail "prospective candidate recipe entered a scope before native promotion"
  unless (case extendExactExecutionSources (concatMap fst parcels) (map snd parcels)
      emptyExecution {scopeProducerSha256=replicate 64 'f'} of Left _ -> True; _ -> False) $
    fail "candidate recipe promoted another compiler producer"
  -- Re-emitting the admitted parcel exercises the actual source-free load:
  -- target sees only the thin reexport facade, not the hidden defining owner.
  let admittedPath = work </> "promoted-scope.cbor"
      admittedTerm = case originalTerm of
        TList fields -> TList [case (index,field,filteredParcel) of
          (7,TList [graphDescriptors,_],TList [_,references]) -> TList [graphDescriptors,references]
          _ -> field | (index,field) <- zip [0::Int ..] fields]
        _ -> originalTerm
  writeTerm admittedPath admittedTerm
  result <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile
    (Just emptySessionScope {ssRoot=work,ssExactScope=Just admittedPath})
    (work </> "ExecutionReexportTarget.hs") [work] Nothing
  unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult result))) $
    fail "promoted cached original recipes did not execute through the thin facade"
  let corrupt = case filteredParcel of
        TList [graphs,TList (TList fields:refs)] ->
          TList [graphs,TList (TList [if index == 2 then TString (T.replicate 64 "f") else field
            | (index,field) <- zip [0::Int ..] fields]:refs)]
        _ -> filteredParcel
  let wrongDigest = case filteredParcel of
        TList [TList (TList [_,bytes]:graphs),refs] ->
          TList [TList (TList [TString (T.replicate 64 "f"),bytes]:graphs),refs]
        _ -> filteredParcel
      duplicateRef = case filteredParcel of
        TList [graphs,TList (first:refs)] -> TList [graphs,TList (first:first:refs)]
        _ -> filteredParcel
  forM_ [("wrong original version",corrupt),("graph digest",wrongDigest)
      ,("duplicate owner",duplicateRef),("unoffered original owner",parcel)] $ \(label,invalid) -> do
    writeTerm candidatePath (envelope invalid)
    readModuleCandidates candidatePath >>= \case
      Left _ -> pure ()
      Right _ -> fail ("candidate execution manifest accepted " ++ label)
  putStrLn "candidate execution sources: actual native/source and matching exact dependency admission, protected roots, selected-source authority, missing/mismatched proof and source drift refusals, accepted-only promotion, no lexical widening, thin reexport execution and identity/digest/duplicate/producer refusals passed"
  where
    readTerm path = do
      bytes <- BS.readFile path
      either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
    candidateGroupTerm group = TList [TInt (fromIntegral (projectedOriginalOrdinal group))
      ,TList (map symbolTerm (projectedBinders group)),TList [TList
        [symbolTerm (globalIdentity global),repTerm (globalRep global)
        ,maybe TNull signatureTerm (globalEntrySignature global >>= \(SignatureId index) ->
          atIndex (projectedSignatures (projectedBody group)) (fromIntegral index))
        ,TBool (globalRequiredEvaluated global),maybe TNull (TInt . fromIntegral) (globalRequiredGeneration global)]
        | global <- projectedGlobals (projectedBody group)]]
    symbolTerm value = TList [TString (symbolUnit value),TString (symbolModule value),TString (symbolNamespace value)
      ,TString (symbolOccurrence value),maybe TNull TString (symbolRecordParent value)]
    repTerm value = TList $ case value of
      VoidRep -> [TString "void",TInt 0]
      LiftedRefRep -> [TString "lifted",TInt 0]
      UnliftedRefRep -> [TString "unlifted",TInt 0]
      AddressRep -> [TString "address",TInt 0]
      IntRep width -> [TString "int",TInt (fromIntegral width)]
      WordRep width -> [TString "word",TInt (fromIntegral width)]
      FloatRep width -> [TString "float",TInt (fromIntegral width)]
    signatureTerm value = TList [TList (map repTerm (signatureArguments value)),case signatureResults value of
      Returns reps -> TList [TString "returns",TList (map repTerm reps)]
      NoSuccess -> TList [TString "no_success",TList []]
      CallerResult -> TList [TString "caller_result",TList []]]
    atIndex values index = case drop index values of value:_ -> Just value; [] -> Nothing

