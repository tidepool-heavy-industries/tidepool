module SourceBootFixtureSupport where

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

writeExecutionScope :: FilePath -> FilePath -> PreparedPipelineResult -> [String] -> IO ()
writeExecutionScope path work original lexicalNames = do
  (source, roots) <- originalCompilerInput original
  let nativeOwners = map (moduleNameString . moduleName . pmModule) (pprModules original)
  writeGenuineExecutionScope nativeOwners lexicalNames work source roots path original


data FixtureInventory = FixtureInventory
  { fixtureSymbols :: Map.Map BS.ByteString Int
  , fixtureSymbolRows :: [Term]
  , fixtureGlobals :: Map.Map BS.ByteString Int
  , fixtureGlobalRows :: [Term]
  }

compactInventoryRows :: [Term] -> Either String (Term,Term,[Term])
compactInventoryRows rows = do
  (inventory,compact) <- mapFixtureInventory compactRow empty rows
  pure (TList (reverse (fixtureSymbolRows inventory)),TList (reverse (fixtureGlobalRows inventory)),compact)
  where
    empty = FixtureInventory Map.empty [] Map.empty []
    compactRow inventory (TList fields) | length fields == 16 = case drop 10 fields of
      TList groups:_ -> do
        (next,compact) <- mapFixtureInventory compactGroup inventory groups
        pure (next,TList (take 10 fields ++ [TList compact] ++ drop 11 fields))
      _ -> Left "fixture candidate lacks groups"
    compactRow _ _ = Left "fixture candidate must have sixteen fields"
    compactGroup inventory (TList [ordinal,TList binders,TList globals]) = do
      (withBinders,binderRefs) <- mapFixtureInventory internFixtureSymbol inventory binders
      (withGlobals,globalRefs) <- mapFixtureInventory internFixtureGlobal withBinders globals
      pure (withGlobals,TList [ordinal,TList binderRefs,TList globalRefs])
    compactGroup _ _ = Left "fixture original group must have three fields"

mapFixtureInventory :: (FixtureInventory -> a -> Either String (FixtureInventory,b))
  -> FixtureInventory -> [a] -> Either String (FixtureInventory,[b])
mapFixtureInventory step initial values = do
  (final,reversed) <- foldM (\(inventory,acc) value -> do
    (next,result) <- step inventory value
    pure (next,result:acc)) (initial,[]) values
  pure (final,reverse reversed)

internFixtureSymbol :: FixtureInventory -> Term -> Either String (FixtureInventory,Term)
internFixtureSymbol inventory value@(TList [_,_,_,_,_]) =
  let key = toStrictByteString (encodeTerm value)
  in case Map.lookup key (fixtureSymbols inventory) of
    Just index -> Right (inventory,TInt index)
    Nothing ->
      let index = Map.size (fixtureSymbols inventory)
      in Right (inventory
        { fixtureSymbols = Map.insert key index (fixtureSymbols inventory)
        , fixtureSymbolRows = value:fixtureSymbolRows inventory },TInt index)
internFixtureSymbol _ _ = Left "fixture symbol must have five fields"

internFixtureGlobal :: FixtureInventory -> Term -> Either String (FixtureInventory,Term)
internFixtureGlobal inventory value@(TList [identity,rep,signature,evaluated,generation]) =
  let key = toStrictByteString (encodeTerm value)
  in case Map.lookup key (fixtureGlobals inventory) of
    Just index -> Right (inventory,TInt index)
    Nothing -> do
      (withSymbol,symbolRef) <- internFixtureSymbol inventory identity
      let index = Map.size (fixtureGlobals withSymbol)
      pure (withSymbol
        { fixtureGlobals = Map.insert key index (fixtureGlobals withSymbol)
        , fixtureGlobalRows = TList [symbolRef,rep,signature,evaluated,generation]:fixtureGlobalRows withSymbol },TInt index)
internFixtureGlobal _ _ = Left "fixture global must have five fields"


hasIntResultLiteral :: Integer -> [Core.CoreBind] -> Bool
hasIntResultLiteral expected = any (\case
      Core.NonRec binder rhs -> getOccString binder == "__result" && contains rhs
      Core.Rec bindings -> any (\(binder, rhs) -> getOccString binder == "__result" && contains rhs) bindings)
  where
    contains = \case
      Core.Lit (LitNumber LitNumInt value) -> value == expected
      Core.App function argument -> contains function || contains argument
      Core.Lam _ body -> contains body
      Core.Let binding body -> any (contains . snd) (Core.flattenBinds [binding]) || contains body
      Core.Case scrutinee _ _ alternatives -> contains scrutinee
        || any (\(Core.Alt _ _ rhs) -> contains rhs) alternatives
      Core.Cast body _ -> contains body
      Core.Tick _ body -> contains body
      _ -> False


withTiming :: IO a -> IO a
withTiming action = bracket (lookupEnv "TIDEPOOL_TIMING") restore $ \_ ->
  setEnv "TIDEPOOL_TIMING" "1" >> action
  where restore = maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING")


manifest :: FilePath -> FilePath
manifest work = work </> "module-candidates.cbor"


writeManifestFor :: [String] -> FilePath -> PreparedPipelineResult -> IO ()
writeManifestFor names work prepared = do
  (source, roots) <- originalCompilerInput prepared
  writeGenuineCandidateManifestFor names work source roots prepared

originalCompilerInput :: PreparedPipelineResult -> IO (FilePath, [FilePath])
originalCompilerInput prepared = do
  let result = pprPipelineResult prepared
      target = tcg_mod (prTargetTcGblEnv result)
      name = moduleNameString (moduleName target)
      unit = unitString (moduleUnit target)
  source <- case [dependencyModuleSource node | node <- dependencyModules (pprDependencies prepared)
      , dependencyModuleUnit node == unit, dependencyModuleName node == name
      , not (dependencyModuleBoot node)] of
    [path] -> pure path
    _ -> fail "fixture compiler input has no unique captured target source"
  pure (source, importPaths (hsc_dflags (prHscEnv result)))


digest :: BS.ByteString -> String
digest = concatMap (\byte -> let text = showHex byte ""
  in replicate (2 - length text) '0' ++ text) . BS.unpack . SHA.hash


withScratch :: (FilePath -> IO a) -> IO a
withScratch action = bracket
  (do root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-source-boot-test"
      hClose handle
      removeFile path
      createDirectory path
      pure path)
  removeDirectoryRecursive action

preparedNames :: PreparedPipelineResult -> [String]
preparedNames = map (moduleNameString . moduleName . pmModule) . pprModules
