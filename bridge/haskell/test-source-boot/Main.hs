module Main (main) where

import ExecutionSourceDecodeTest (executionSourceDecodeChecks, executionSourceDecodeBenchmark, executionSourceDecodeSnapshots, executionSourceResolutionBudgetChecks)
import ExactScopeV9Test (exactScopeV9Checks, nativeOriginChecks, candidateCanonicalChecks)
import CandidateGraphDescriptorTest (candidateGraphDescriptorChecks)
import GenuineCandidateFixture
  ( writeGenuineCandidateManifestFor, writeGenuineMetadataScope, writeGenuineEmptyMetadataScope
  , writeGenuineCandidateNativeScope, writeGenuineCandidateLexicalScope, writeGenuineAuthoredDeclarationScope
  , writeGenuineOriginalExecutionScope )

import Codec.CBOR.Encoding (encodeBool, encodeListLen, encodeString)
import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Data.ByteString.Lazy qualified as BSL
import Control.Exception (SomeException, AsyncException(ThreadKilled), bracket, evaluate, finally, try, fromException)
import Control.Concurrent (MVar, forkIO, killThread, newEmptyMVar, putMVar, takeMVar)
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
import GHC.Tc.Types (tcg_imports, tcg_type_env)
import GHC.Unit.Module.Deps (imp_mods, Usage(..))
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries', mkModuleGraph)
import GHC.Types.SourceFile (HscSource(..))
import Control.Monad.IO.Class (liftIO)
import GHC.Driver.Session (targetProfile, wopt_set, xopt, WarningFlag(Opt_WarnMissingSignatures))
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Types.Error (isEmptyMessages)
import GHC.Types.Name.Set (nameSetElemsStable)
import GHC.Driver.Hooks (hscCompileCoreExprHook)
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
  , getModificationTime, setModificationTime, withCurrentDirectory, getCurrentDirectory )
import System.Environment (getArgs, getExecutablePath, setEnv, lookupEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>), takeDirectory, normalise, replaceExtension)
import System.IO (hClose, hFlush, hPutStrLn, hSeek, hSetFileSize, withBinaryFile, IOMode(WriteMode), SeekMode(AbsoluteSeek), openTempFile, stderr)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.Process (readProcessWithExitCode)
import System.Timeout (timeout)
import Tidepool.CertifiedProducts (encodeCertifiedProducts, resolvePackageGlobal)
import Tidepool.FinalizedModuleArtifacts (captureFinalizedModuleArtifacts, emptyFinalizedModuleArtifacts)
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
import Tidepool.HomeProducts (hydrateCandidateHomeProducts)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), PipelineResult(..), CheckedEnvironmentResult(..)
  , finalizedTidyGuts
  , renderType, generatedScaffoldRecipe, activationPreviewInputType
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
  , originalGroupFromCandidate
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget, scopeExecutionNativeOwners )
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import ProgressBoundaryTest (progressBoundaryChecks)
import FinalizedCoreTest (finalizedCoreChecks)
import Tidepool.CheckedCell (CheckedSignature(..), RequestTypeSignatures(..), RequestHelperRecipe(..), captureCheckedSignature, encodeCheckedSignature, encodeRequestTypeSignatures
  , captureCheckedTypeWitness, sealCheckedTypeWitness, encodeCheckedTypeWitness, rewriteCheckedAnnotations, rewriteHostInputType, rewriteRequestTypes, NativeParsedModule(..), thenNativeModule, typecheckNativeModule, typecheckNativeModuleWithDiagnostics)
import Tidepool.TurnSource (replaceTemplateMarker, spliceTemplate)
import Tidepool.Binders (BoundBinder(..))
import Tidepool.ExecutionSource
  ( WorkerExecutionSource(OrdinaryExecutionSource)
  , ExecutionSourceIdentity(..), ExecutionSourceOwner(..), ExecutionSourceRef(..), ExecutionSourceGraph(..), ExecutionSourceNode(..)
  , executionSourceClosure, executionSourceOriginalNode, executionSourceOriginalClosure, executionIdentityKey
  , ExecutionSourceFailure(..), ExecutionSourceRecipe(..), issueExecutionSourceRecipe, executionSourceProspectiveReferences
  , executionNodeOriginalResolutions
  , decodeExecutionSourceGraph
  , executionSourceGraphBytesLimit )

counterValues :: String -> String -> [Integer]
counterValues name diagnostics = map parseCount matching
  where
    prefix = "tidepool-count name=" ++ name ++ " "
    matching = [line | line <- lines diagnostics, prefix `isPrefixOf` line]
    parseCount line = case
      [raw | field <- words line, Just raw <- [stripPrefix "count=" field]] of
        [raw] -> case reads raw of
          [(value, "")] -> value
          _ -> error ("invalid count field for " ++ name ++ " in diagnostic: " ++ line)
        _ -> error ("missing or duplicate count field for " ++ name
          ++ " in diagnostic: " ++ line)

counterTotal :: String -> String -> Integer
counterTotal name = sum . counterValues name

exactCompilationCacheSafety
  :: FilePath -> BS.ByteString -> Either String (Maybe Bool)
exactCompilationCacheSafety expectedSource bytes = do
  (remaining, term) <- case deserialiseFromBytes decodeTerm (BSL.fromStrict bytes) of
    Left failure -> Left ("invalid exact compilation receipt CBOR: " ++ show failure)
    Right decoded -> Right decoded
  unless (BSL.null remaining) (Left "exact compilation receipt has trailing CBOR bytes")
  case term of
    TList (TString "TPEXACTCOMPILE" : TString version : fields)
      | version /= "2" -> Left ("unsupported exact compilation receipt schema " ++ T.unpack version)
      | length fields /= 8 -> Left ("exact compilation receipt v2 has "
          ++ show (length fields + 2) ++ " fields; expected 10")
      | otherwise -> case fields of
          [_, _, TString source, _, _, TString facts, _, _]
            | source /= T.pack expectedSource -> Right Nothing
            | otherwise -> Just <$> dependencyCacheSafe (T.unpack facts)
          _ -> Left "exact compilation receipt v2 has invalid source or evidence fields"
    _ -> Left "exact compilation receipt has an invalid tag or outer record"
  where
    dependencyCacheSafe facts = case stripPrefix "{\"version\":4,\"cache_safe\":" facts of
      Nothing -> Left "exact compilation receipt has invalid dependency evidence v4 JSON"
      Just value
        | Just rest <- stripPrefix "false," value, not (null rest), last rest == '}' -> Right False
        | Just rest <- stripPrefix "true," value, not (null rest), last rest == '}' -> Right True
        | otherwise -> Left "dependency evidence v4 has no canonical cache_safe boolean"

main :: IO ()
main = getArgs >>= \case
  ["--finalized-core"] -> finalizedCoreChecks
  ["--exact-scope-v9", manifest] -> exactScopeV9Checks manifest
  ["--native-origin-scope", manifest] -> nativeOriginChecks manifest
  ["--canonical-candidates", scope, candidates] -> candidateCanonicalChecks scope candidates
  ["--finalized-frontend-once"] -> finalizedFrontendOnce
  ["--execution-source-decode"] -> executionSourceDecodeChecks
  ["--execution-source-resolution-budget"] -> executionSourceResolutionBudgetChecks
  "--execution-source-decode-benchmark" : iterations : files -> executionSourceDecodeBenchmark iterations files
  "--execution-source-decode-snapshots" : output : files -> executionSourceDecodeSnapshots output files
  ["--exact-scope-binders"] -> exactScopeBinders
  ["--original-package-projection"] -> originalPackageProjection
  ["--original-package-cohort", coreRoot, output] -> originalPackageCohort coreRoot output
  ["--original-projection-products"] -> originalProjectionProducts
  ["--candidate-manifest-products", path] -> candidateManifestProducts path
  ["--candidate-compact-inventory"] -> candidateCompactInventory
  ["--candidate-ghc-load"] -> candidateGhcLoad
  ["--source-boot-reuse"] -> withTiming (withScratch sourceBootReuseAt)
  ["--source-boot-reuse", work] -> withTiming $ do
    createDirectory work
    sourceBootReuseAt work
  ["--mixed-default"] -> withTiming $ do
    mixedGraph False 1
    mixedGraph True 10
  ["--candidate-sited-siblings"] -> candidateSitedSiblings
  ["--candidate-sited-siblings", work] -> candidateSitedSiblingsAt work
  ["--generated-scaffold-imports"] -> generatedScaffoldImports
  ["--generated-scaffold-retained",scope,seal] -> generatedScaffoldRetained scope seal
  ["--hydrated-site-siblings"] -> hydratedSiteSiblings
  ["--fresh-execution-recipe"] -> freshExecutionRecipeTest
  ["--candidate-graph-descriptors"] -> withTiming (withScratch candidateGraphDescriptorsAt)
  ["--native-checked-signatures"] -> nativeCheckedSignaturesTest
  ["--host-activation-purpose"] -> hostActivationPurposeTest Nothing
  ["--host-activation-purpose", destination] -> hostActivationPurposeTest (Just destination)
  ["--candidate-execution-sources"] -> candidateExecutionSourcesTest
  ["--candidate-execution-wire", path] -> candidateExecutionWire path
  ["--checked-value-type-closure", effects] -> checkedValueTypeClosure effects
  ["--progress-boundary", effects] -> progressBoundaryChecks effects
  ["--execution-source-wire", path] -> executionSourceWire path
  ["--execution-source-closure-wire", path] -> executionSourceClosureWire path
  ["--exact-retained-quoter"] -> exactRetainedQuoter
  ["--retained-execution-publication"] -> retainedExecutionPublication
  ["--exact-reexport-quoter"] -> exactReexportQuoter
  ["--exact-execution-hidden-instance"] -> exactExecutionHiddenInstance
  ["--exact-execution-values"] -> exactExecutionValues
  ["--exact-to-ordinary"] -> exactToOrdinary
  ["--checked-value-imports"] -> checkedValueImports
  ["--exact-loaded-metadata"] -> exactLoadedMetadata
  ["--quasiquote-codegen-transition"] -> quasiQuoteCodegenTransition
  ["--exact-bash-metadata", effects] -> exactBashMetadata effects
  ["--package-inputs"] -> withTiming packageInputs
  ["--home-instance-edges"] -> selectedHomeInstanceEdges
  ["--fresh", work] -> reuseFresh work >>= requireReused "fresh worker"
  ["--mixed-fresh", work, count] -> reuseFresh work >>= requireMixed (read count)
  ["--resolution-paths"] -> resolutionPaths
  ["--mixed"] -> do
    setEnv "TIDEPOOL_TIMING" "1"
    forM_ [1, 10, 100] (mixedGraph False)
    mixedGraph True 10
  [] -> do
    exactScopeBinders
    selectedHomeInstanceEdges
    withTiming (withScratch sourceBootReuseAt)
    candidateSitedSiblings
    setEnv "TIDEPOOL_TIMING" "1"
    mixedGraph False 1
    mixedGraph True 10
  _ -> fail "unexpected SOURCE boot test arguments"

-- One immutable capture supplies SOURCE SCC candidates and a separate ordinary
-- native dependency pair. SOURCE imports cannot issue execution source recipes.
sourceBootReuseAt :: FilePath -> IO ()
sourceBootReuseAt work = do
  forM_ ["CacheEven.hs", "CacheEven.hs-boot", "CacheOdd.hs", "CacheEntry.hs"
        , "NativeScopeBase.hs", "NativeScopeOwner.hs", "SourceBootCapture.hs"] $ \file ->
    copyFile ("test-source-boot/fixtures" </> file) (work </> file)
  cold <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
    (work </> "SourceBootCapture.hs") [work] (Just (work </> "build-products"))
  unless (all (`elem` preparedNames cold) ["CacheEven", "CacheOdd", "CacheEntry"
      , "NativeScopeBase", "NativeScopeOwner", "SourceBootCapture"]
      && null (pprAcceptedCandidates cold)) $
    fail "cold SOURCE graph omitted original defining products"
  unless (dependencyCacheSafe (pprDependencies cold)
      && dependencySelectionComplete (pprDependencies cold)) $
    fail "cold SOURCE graph lacks final source/package evidence"
  let nativeScope = work </> "ordinary-native-scope.cbor"
  writeGenuineCandidateNativeScope ["CacheEven", "CacheOdd"]
    ["NativeScopeBase", "NativeScopeOwner"] work
    (work </> "SourceBootCapture.hs") [work] nativeScope cold
  deliveredScope <- readExactScope nativeScope >>= either fail pure
  unless (Set.fromList (map originalModule (scopeProducts deliveredScope))
      == Set.fromList ["NativeScopeBase", "NativeScopeOwner"]
      && not (null (scopeExecutionGraphs deliveredScope))
      && not (null (scopeExecutionNativeOwners deliveredScope))) $
    fail "genuine ordinary scope omitted original native rows or authenticated execution custody"
  candidates <- readModuleCandidates (manifest work) >>= either fail pure
  unless (Set.fromList (map candidateModule candidates) == Set.fromList ["CacheEven", "CacheOdd"]
      && all (\candidate -> case candidateExecutionSource candidate of
        Nothing -> True
        Just _ -> False) candidates) $
    fail "SOURCE SCC candidate issued unsupported execution source custody"
  putStrLn "SOURCE execution custody: CacheEven/CacheOdd recipes absent; ordinary NativeScopeBase/NativeScopeOwner recipes admitted"
  exactScopeV9Checks nativeScope
  let nativeOwner = SessionModule LibMod (Generation 1)
      authoredSource = replaceExtension (sessionHiPath work nativeOwner) "hs"
      authoredScope = work </> "authored-native-origin-scope.cbor"
  createDirectoryIfMissing True (takeDirectory authoredSource)
  copyFile "test-source-boot/fixtures/AuthoredScopeG1.hs" authoredSource
  writeGenuineAuthoredDeclarationScope nativeOwner [work] authoredSource authoredScope
  nativeOriginChecks authoredScope
  candidateCanonicalChecks nativeScope (manifest work)
  verifyHydration work cold
  partial <- runPipelineSelected (PreparedProducts (Just (manifest work)))
    (work </> "CacheEven.hs") [work]
  requireRefused "SCC containing the fresh target" partial
  withResidentPipelineSelected [work] $ \compile -> do
    first <- compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
      Nothing (work </> "CacheEntry.hs") [] Nothing
    requireReused "first resident request" first
    second <- compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
      Nothing (work </> "CacheEntry.hs") [] Nothing
    requireReused "warm resident request" second
    candidates <- readModuleCandidates (manifest work) >>= either fail pure
    originalProduct <- case [candidateProductPath candidate | candidate <- candidates
      , candidateModule candidate == "CacheEven"] of
      [path] -> pure path
      _ -> fail "genuine SOURCE fixture lost its native product"
    let reuse = compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
          Nothing (work </> "CacheEntry.hs") [] Nothing
    originalBytes <- BS.readFile originalProduct
    (BS.writeFile originalProduct "changed original native bytes" >>
      reuse >>= requireRefused "changed original native product")
      `finally` BS.writeFile originalProduct originalBytes
    reuse >>= requireReused "restored original native product"
    exerciseRefusals work (compile (PreparedProducts (Just (manifest work))) Set.empty
      GeneralCompile Nothing (work </> "CacheEntry.hs") [] Nothing)
  executable <- getExecutablePath
  (exit, _, errors) <- readProcessWithExitCode executable ["--fresh", work] ""
  unless (exit == ExitSuccess) $ fail ("fresh worker reuse failed: " ++ errors)
  _ <- reuseFresh work >>= requireReused "reuse after refusal"
  putStrLn "SOURCE boot cache: cold, resident, warm, fresh-worker, ABI and CPP refusal passed"

-- One GHC capture supplies a genuine graph larger than the metadata envelope.
-- Candidate and scope delivery use their existing canonical Rust owners.
candidateGraphDescriptorsAt :: FilePath -> IO ()
candidateGraphDescriptorsAt work = do
  forM_ ["NativeScopeBase.hs", "NativeScopeOwner.hs", "NativeScopeCapture.hs"] $ \file ->
    copyFile ("test-source-boot/fixtures" </> file) (work </> file)
  let source = work </> "NativeScopeCapture.hs"
      scopeRoot = work </> "native-scope-request"
      scopePath = scopeRoot </> "native-scope.cbor"
      owners = Set.fromList ["NativeScopeBase", "NativeScopeOwner"]
  BS.appendFile source (BSC.pack ("\n--" ++ replicate (4*1024*1024) ' ' ++ "\n"))
  createDirectory scopeRoot
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile
    Nothing source [work] (Just (work </> "build-products"))
  writeGenuineCandidateNativeScope (Set.toAscList owners) (Set.toAscList owners)
    work source [work] scopePath original
  candidateGraphDescriptorChecks scopePath (manifest work)
  candidateCanonicalChecks scopePath (manifest work)
  accepted <- runPipelineSelected (PreparedProducts (Just (manifest work))) source [work]
  unless (Set.fromList (map candidateModule (pprAcceptedCandidates accepted)) == owners
      && Set.null (owners `Set.intersection` Set.fromList (preparedNames accepted))) $
    fail "genuine graph above four MiB lost actual candidate reuse"
  putStrLn "candidate graph descriptors: genuine bounded offer and actual dependency reuse passed"

-- Load provenance is local to a request; retained owners and source changes
-- must still govern the next exact frontend.
checkedValueImports :: IO ()
checkedValueImports = withScratch $ \work -> do
  let valueName = mkModuleName "Tidepool.Session.Val.G2"
      valueDirectory = work </> "Tidepool/Session/Val"
      hi = work </> "checked-value.hi"
  createDirectoryIfMissing True valueDirectory
  copyFile "test-source-boot/fixtures/CheckedValueG2.hs" (valueDirectory </> "G2.hs")
  copyFile "test-source-boot/fixtures/CheckedValueConsumer.hs" (work </> "CheckedValueConsumer.hs")
  compiled <- runPipelineSelected (PreparedProducts Nothing) (work </> "CheckedValueConsumer.hs") [work]
  let producer = prHscEnv (pprPipelineResult compiled)
      consumers = [ModuleNode [] summary
        | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph producer)
        , ms_mod_name summary == mkModuleName "CheckedValueConsumer"]
      sourceGraph = mkModuleGraph consumers
  iface <- maybe (fail "checked value fixture omitted its interface") pure
    (Map.lookup valueName (pprProductInterfaces compiled))
  writeBinIface (targetProfile (hsc_dflags producer)) QuietBinIFace NormalCompression hi iface
  bytes <- BS.readFile hi
  let artifact = ExactIfaceArtifact "main" "Tidepool.Session.Val.G2" hi (digest bytes) []
  fresh <- freshExactState producer
  verified <- readCheckedValueImportAuthority fresh [artifact] >>= either fail pure
  present <- installExactLexicalGraph sourceGraph [] verified producer >>= either fail pure
  absent <- installExactLexicalGraph sourceGraph [] verified fresh >>= either fail pure
  unless ([ms_mod_name summary | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph present)]
      == [mkModuleName "CheckedValueConsumer"]
      && case lookupHpt (hsc_HPT absent) valueName of Nothing -> True; Just _ -> False) $
    fail "checked value import authority installed a value or added a lexical implementation"
  unverified <- installExactLexicalGraph sourceGraph [] noCheckedValueImports producer
  unless (case unverified of Left _ -> True; Right _ -> False) $
    fail "an HPT value without checked import authority became importable"
  collision <- installExactLexicalGraph (hsc_mod_graph producer) [] verified producer
  unless (case collision of Left _ -> True; Right _ -> False) $
    fail "checked value authority admitted a fresh source owner"
  forM_ [artifact { exactModule = "Tidepool.Session.Val.G3" }
    , artifact { exactSha256 = replicate 64 '0' }
    , artifact { exactPath = hi ++ ".missing" }
    , artifact { exactModule = "CheckedValueConsumer" }] $ \changed -> do
      refused <- readCheckedValueImportAuthority fresh [changed]
      unless (case refused of Left _ -> True; Right _ -> False) $
        fail "checked value import accepted a missing, changed or wrong-owner proof"
  captured <- readExactIfaceArtifacts absent [artifact] >>= either fail pure
  injected <- hydrateExactScope absent captured
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    setSession injected
    case consumers of
      [ModuleNode _ summary] -> void (parseModule summary >>= typecheckModule)
      _ -> liftIO (fail "checked value import fixture lacks its consumer")
  putStrLn "checked value imports: HPT parity, delayed injection and wrong-input refusal passed"

writeGenuineEmptyScopeFields :: FilePath -> IO [Term]
writeGenuineEmptyScopeFields path = do
  writeGenuineEmptyMetadataScope path
  _ <- readExactScope path >>= either fail pure
  bytes <- BS.readFile path
  either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes)) >>= \case
      TList values | length values == 9 -> pure values
      _ -> fail "genuine exact scope has another envelope layout"

exactScopeBinders :: IO ()
exactScopeBinders = withScratch $ \work -> do
  fields <- writeGenuineEmptyScopeFields (work </> "issued-binder-scope.cbor")
  let path = work </> "binder-scope.cbor"
      text = TString . T.pack
      sha = text (replicate 64 '0')
      binder namespace occurrence parent = SymbolIdentity "main" "BinderFixture" namespace occurrence parent
      identity value = TList [TString (symbolUnit value), TString (symbolModule value)
        , TString (symbolNamespace value), TString (symbolOccurrence value)
        , maybe TNull TString (symbolRecordParent value)]
      -- Malformed native rows belong only to refusal tests. The binder
      -- decoder rejects their duplicate inventory before owner promotion.
      term groups = TList [if index == 6 then
          TList [TList [text "main", text "BinderFixture", sha, sha, sha, text (work </> "original.tpmod")
            , TList [TList [TInt ordinal, TList (map identity values), TList []]
              | (ordinal, values) <- zip [0..] groups]]]
        else field | (index,field) <- zip [0::Int ..] fields]
      readGroups groups = do
        BS.writeFile path (toStrictByteString (encodeTerm (term groups)))
        readExactScope path
      accepted groups = do
        -- Identity/group conversion is a typed stage, not wire authority.
        let converted = [originalGroupFromCandidate (CandidateGroup ordinal values [])
              | (ordinal,values) <- zip [0..] groups]
        unless (map originalBinders converted == groups
            && map originalOrdinal converted == take (length groups) [0..]
            && all (null . originalGlobals) converted) $
          fail "exact original group conversion changed binder identities or group order"
      refused groups = do
        result <- readGroups groups
        unless (case result of Left reason -> "duplicate exact original binders" `isInfixOf` reason; Right _ -> False) $
          fail "exact scope accepted duplicate original binder identity"
      answer = binder "value" "answer" Nothing
      sameSpelling = [answer, binder "type" "answer" Nothing
        , binder "value" "answer" (Just "RecordA"), binder "value" "answer" (Just "RecordB")]
      allFields = sameSpelling ++ [answer { symbolUnit = "another-unit" }
        , answer { symbolModule = "AnotherModule" }
        , binder "value" "anotherOccurrence" Nothing]
      large = [binder "value" (T.pack ("binder" ++ show index)) Nothing | index <- [1..6037 :: Int]]
  unless (and [(left == right) == (compare left right == EQ) | left <- allFields, right <- allFields]) $
    fail "SymbolIdentity Eq and Ord disagree"
  accepted [[]]
  accepted [[answer]]
  accepted [take 2 sameSpelling, drop 2 sameSpelling]
  accepted [large]
  refused [[answer, answer]]
  refused [take 2 sameSpelling, drop 2 sameSpelling ++ [answer]]
  refused [large, [binder "value" "binder1" Nothing]]
  putStrLn "exact scope binders: 8 checks passed (identity equality, empty/singleton/distinct/6037 inventory, same-group/cross-group/late duplicate)"

executionSourceWire :: FilePath -> IO ()
executionSourceWire path = do
  scope <- readExactScope path >>= either fail pure
  unless (length (scopeExecutionGraphs scope) == 1 && length (scopeExecutionOwners scope) == 1
      && scopePurpose scope == NoCheckedPurpose && scopeIncludePaths scope == Nothing) $
    fail "Rust ordinary exact scope lost its execution payload or NULL purpose"
  nodes <- either (fail . show) pure (executionSourceClosure (scopeExecutionGraphs scope)
    (scopeExecutionOwners scope) (scopeExecutionNativeOwners scope)
    (map (executionIdentityKey . executionRefIdentity) (scopeExecutionOwners scope)))
  unless (length nodes == 1) (fail "Rust execution source graph lost its original source root")
  bytes <- BS.readFile path
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
  changed <- case term of
    TList [magic,version,semantic,producer,interfaces,lexical,products,
        TList [graphs,TList [TList [unit,name,original,iface,_,graph]]],purpose] ->
      pure (TList [magic,version,semantic,producer,interfaces,lexical,products,
        TList [graphs,TList [TList [unit,name,original,iface,TString (T.replicate 64 "0"),graph]]],purpose])
    _ -> fail "Rust exact scope has another frozen execution layout"
  let changedPath = takeDirectory path </> "changed-native-reference.cbor"
  BS.writeFile changedPath (toStrictByteString (encodeTerm changed))
  refused <- readExactScope changedPath
  unless (case refused of Left _ -> True; Right _ -> False) $
    fail "execution graph accepted a reference to another native product"
  let budgetPath = takeDirectory path </> "budget-scope.cbor"
  budgetScope <- readExactScope budgetPath >>= either fail pure
  budgetBytes <- BS.readFile budgetPath
  unless (scopeExecutionGraphs budgetScope == scopeExecutionGraphs scope
      && scopeExecutionOwners budgetScope == scopeExecutionOwners scope
      && BS.length budgetBytes + sum (map (BS.length . executionGraphBytes) (scopeExecutionGraphs scope)) > 4*1024*1024) $
    fail "independent valid metadata/graph budgets lost original execution custody"
  (graphSha, graphPath) <- case term of
    TList [_,_,_,_,_,_,_,TList [TList [TList [TString sha,TString file]],_],_] -> pure (T.unpack sha,T.unpack file)
    _ -> fail "Rust scope6 descriptor layout differs"
  originalBytes <- BS.readFile graphPath
  let refuse label expected action = do
        action
        result <- readExactScope path
        BS.writeFile graphPath originalBytes
        unless (case result of Left reason -> expected `isInfixOf` reason; Right _ -> False) $
          fail ("scope6 failed to enforce " ++ label ++ " before execution")
  refuse "missing graph" "does not exist" (removeFile graphPath)
  refuse "truncated graph" "digest differs" (BS.writeFile graphPath (BS.take (BS.length originalBytes - 1) originalBytes))
  refuse "tampered graph" "digest differs" (BS.writeFile graphPath (BS.cons 0 (BS.drop 1 originalBytes)))
  withBinaryFile graphPath WriteMode $ \handle ->
    hSetFileSize handle (fromIntegral executionSourceGraphBytesLimit + 1)
  oversizedResult <- readExactScope path
  BS.writeFile graphPath originalBytes
  unless (case oversizedResult of Left reason -> "exceed 64 MiB" `isInfixOf` reason; Right _ -> False) $
    fail "scope6 graph aggregate was not refused by its byte bound"
  let swapped = takeDirectory path </> "swapped-graph.cbor"
      swappedManifest = takeDirectory path </> "swapped-scope.cbor"
      wrongGraph = BSC.pack "another immutable graph"
  BS.writeFile swapped wrongGraph
  swappedTerm <- case term of
    TList [magic,version,semantic,producer,interfaces,lexical,products,TList [_,refs],purpose] ->
      pure (TList [magic,version,semantic,producer,interfaces,lexical,products,
        TList [TList [TList [TString (T.pack graphSha),TString (T.pack swapped)]],refs],purpose])
    _ -> fail "scope6 fixture changed layout"
  BS.writeFile swappedManifest (toStrictByteString (encodeTerm swappedTerm))
  swappedResult <- readExactScope swappedManifest
  unless (case swappedResult of Left _ -> True; Right _ -> False) $
    fail "scope6 accepted swapped graph before execution"
  let rejectTerm label expected changedTerm = do
        let changedManifest = takeDirectory path </> (label ++ "-scope.cbor")
        BS.writeFile changedManifest (toStrictByteString (encodeTerm changedTerm))
        result <- readExactScope changedManifest
        unless (case result of Left reason -> expected `isInfixOf` reason; Right _ -> False) $
          fail ("scope6 failed to enforce " ++ label ++ " before execution")
  case term of
    TList [magic,_,semantic,producer,interfaces,lexical,products,TList [_,refs],purpose] ->
      rejectTerm "legacy-v5" "unsupported exact scope" (TList [magic,TString "5",semantic,producer,interfaces,lexical,products,
        TList [TList [TList [TString (T.pack graphSha),TBytes originalBytes]],refs],purpose])
    _ -> fail "scope6 fixture changed version layout"
  let outsidePath = takeDirectory (takeDirectory path) </> "outside.cbor"
  BS.writeFile outsidePath originalBytes
  case term of
    TList [magic,version,semantic,producer,interfaces,lexical,products,TList [_,refs],purpose] ->
      rejectTerm "outside-request" "outside its request directory" (TList [magic,version,semantic,producer,interfaces,lexical,products,
        TList [TList [TList [TString (T.pack graphSha),TString (T.pack outsidePath)]],refs],purpose])
    _ -> fail "scope6 fixture changed path layout"
  putStrLn "Rust execution wire: scope6 closure, independent budgets, wrong native/missing/truncated/tampered/swapped/oversized/outside-request/v5 refusals passed (10 checks)"

executionSourceClosureWire :: FilePath -> IO ()
executionSourceClosureWire path = do
  scope <- readExactScope path >>= either fail pure
  let graphs = scopeExecutionGraphs scope
      references = scopeExecutionOwners scope
      native = scopeExecutionNativeOwners scope
      closure inputs owners = executionSourceClosure inputs references owners [("main","A")]
  unless (length graphs == 2 && length references == 2
      && sum (map (BS.length . executionGraphBytes) graphs) > 4*1024*1024) $
    fail "retained closure fixture lost its independently bounded graphs"
  nodes <- either (fail . show) pure (closure graphs native)
  unless (map (executionIdentityKey . executionNodeIdentity) nodes == [("main","A"),("main","B")]) $
    fail "large retained original closure was truncated"
  forM_ graphs $ \graph -> unless
      (case closure (filter (/= graph) graphs) native of Left _ -> True; _ -> False) $
    fail "large closure accepted a missing required original graph"
  unless (case closure graphs (filter ((/= "B") . executionModule) native) of
      Left _ -> True; _ -> False) $
    fail "large closure accepted a missing native owner"
  let candidates = takeDirectory path </> "empty-candidates.cbor"
  BS.writeFile candidates (toStrictByteString (encodeTerm (TList
    [TString "TPMCAN", TString "10", TList [], TList [], TList [], TList [TList [],TList []]
      ,TString (T.pack (scopeProducerSha256 scope))])))
  decoded <- readModuleCandidatesWithGraphs graphs candidates >>= either fail pure
  unless (null decoded) $ fail "empty candidate manifest invented source candidates"
  bytes <- BS.readFile path
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
  graphPath <- case term of
    TList [_,_,_,_,_,_,_,TList [TList (TList [_,TString file] : _),_],_] -> pure (T.unpack file)
    _ -> fail "large closure descriptor layout differs"
  original <- BS.readFile graphPath
  withBinaryFile graphPath WriteMode $ \handle ->
    hSetFileSize handle (fromIntegral executionSourceGraphBytesLimit + 1)
  oversized <- readExactScope path
  BS.writeFile graphPath original
  unless (case oversized of Left reason -> "exceed 64 MiB" `isInfixOf` reason; Right _ -> False) $
    fail "large closure lost its aggregate graph bound"
  putStrLn "Rust execution closure wire: complete two-graph closure above four MiB, missing graphs/native owner, candidate join and above-64-MiB refusal passed (6 checks)"

checkedValueTypeClosure :: FilePath -> IO ()
checkedValueTypeClosure effects = withScratch $ \work -> do
  let producerPath = work </> "MetadataBashTarget.hs"
      consumerPath = work </> "CheckedCommandConsumer.hs"
      scopePath = work </> "exact-scope.cbor"
  copyFile "test-source-boot/fixtures/MetadataBashTarget.hs" producerPath
  copyFile "test-source-boot/fixtures/CheckedCommandConsumer.hs" consumerPath
  prepared <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing producerPath [work,"lib",effects] Nothing
  let result = pprPipelineResult prepared
      environment = prHscEnv result
  binders <- mkBoundBinders ["cmd"] 7 work result
  valueOwner <- maybe (fail "checked command has no canonical value owner") pure
    (parseValModule "Tidepool.Session.Val.G7")
  let valuePath = sessionHiPath work valueOwner
  valueBytes <- BS.readFile valuePath
  requirementBytes <- BS.readFile (valuePath ++ ".requirements")
  requirements <- case deserialiseFromBytes decodeTerm (BSL.fromStrict requirementBytes) of
    Right (remaining,TList rows) | BSL.null remaining -> forM rows $ \case
      TList [TString unit,TString name] -> pure (T.unpack unit,T.unpack name)
      _ -> fail "checked command type requirement has another format"
    _ -> fail "checked command type requirements cannot be decoded"
  unless (("main","Tidepool.Command.Types") `elem` requirements) $
    fail "checked Command fixture lacks its real home type dependency"
  originals <- forM (Map.toAscList (pprProductInterfaces prepared)) $ \(name,iface) -> do
    let path = work </> (moduleNameString name ++ ".original.hi")
    writeBinIface (targetProfile (hsc_dflags environment)) QuietBinIFace NormalCompression path iface
    bytes <- BS.readFile path
    dependencies <- either fail pure (selectedHomeRequirements (pprDependencies prepared) "main" (moduleNameString name))
    let artifact = ExactIfaceArtifact "main" (moduleNameString name) path (digest bytes) dependencies
        packagesPath = path ++ ".packages"
        packages = encodePackageImports artifact
          (Map.findWithDefault emptyPackageImports name (pprPackageImports prepared))
    BS.writeFile packagesPath packages
    pure (artifact,packagesPath,digest packages)
  let value = ExactIfaceArtifact "main" "Tidepool.Session.Val.G7" valuePath (digest valueBytes) requirements
      lexical = [(artifact,exactRequirements artifact) | (artifact,_,_) <- originals]
  writeExactMetadataScopeWithLexical scopePath originals lexical
  base <- readExactScope scopePath >>= either fail pure
  valuePackages <- BS.readFile (valuePath ++ ".packages")
  let admitted = base
        { scopeInterfaces = scopeInterfaces base ++ [(value,valuePath ++ ".packages",digest valuePackages)]
        , scopeLexical = scopeLexical base ++ [(("main","Tidepool.Session.Val.G7"),requirements)]
        , scopePurpose = ExactCellPurpose (CheckedCellAdmission (replicate 64 '0') (replicate 64 '0')
            (replicate 64 '0') [] ["Tidepool.Session.Val.G7"] [] [value] Nothing AuthoredCellCheck) [work,"lib",effects] }
      scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath, ssValIfaces = [valueOwner] }
  isolated <- readCheckedValueImportAuthority environment [value]
  unless (case isolated of Left "incomplete exact interface dependency closure" -> True; _ -> False) $
    fail "a checked value authorized its absent type owner"
  verified <- readVerifiedExactIfaceClosure environment (value : [iface | (iface,_,_) <- originals])
    >>= either fail pure
  _ <- either fail pure (checkedValueImportAuthorityFromVerified verified [value])
  forM_ [value {exactRequirements=[]},value {exactSha256=replicate 64 '0'}] $ \changed ->
    unless (case selectVerifiedExactInterfaces verified [changed] of Left _ -> True; Right _ -> False) $
      fail "late checked value weakened or changed its captured type proof"
  let missing = value : [iface | (iface,_,_) <- originals, exactModule iface /= "Tidepool.Command.Types"]
  refused <- readVerifiedExactIfaceClosure environment missing
  unless (case refused of Left _ -> True; Right _ -> False) $
    fail "a checked command accepted an absent captured type owner"
  let aliasPath = work </> "captured-command-copy.hi"
      alias = value {exactPath=aliasPath,exactRequirements=[]}
  copyFile valuePath aliasPath
  aliasClosure <- readVerifiedExactIfaceClosureWithCheckedValues environment
    (value : [iface | (iface,_,_) <- originals]) [alias] >>= either fail pure
  selected <- either fail pure (selectVerifiedValueInterfaces aliasClosure [alias])
  unless (map (exactRequirements . fst) selected == [requirements]) $
    fail "captured value alias lost its complete original type requirements"
  forM_ [alias {exactSha256=replicate 64 '0'},alias {exactPath=aliasPath ++ ".missing"}] $ \wrong -> do
    rejected <- readVerifiedExactIfaceClosureWithCheckedValues environment
      (value : [iface | (iface,_,_) <- originals]) [wrong]
    unless (case rejected of Left _ -> True; Right _ -> False) $
      fail "captured command alias authorized changed or missing bytes"
  identifier <- case binders of
    [binder] -> pure (bbVarId binder)
    _ -> fail "checked command has another binder inventory"
  withResidentPipelineSelected [work,"lib",effects] $ \compile ->
    forM_ [(value,GeneralCompile),(alias,CheckedItemCompile [] Nothing
        [CompletedValueImport "main" "Tidepool.Session.Val.G7" aliasPath (digest valueBytes) [("cmd",identifier)]])]
      $ \(input,purpose) -> do
        let capture = admitted {scopePurpose = case scopePurpose admitted of
              ExactCellPurpose admission paths -> ExactCellPurpose (admission {checkedValueInterfaces=[input]}) paths
              purpose' -> purpose'}
        checked <- compile CheckedEnvironment Set.empty (CellProgramCompile purpose capture)
          (Just scope) consumerPath [work,"lib",effects] Nothing
        unless (fmap renderType (crResultType checked) == Just "Command") $
          fail "dependency-ordered command value injection changed its captured type"
  putStrLn "checked command value: complete type closure, delayed injection and missing/changed-owner refusal passed"

generatedScaffoldImports :: IO ()
generatedScaffoldImports = withTiming $ withScratch $ \work -> do
  let supportDirectory = work </> "Tidepool/Internal"
      supportPath = supportDirectory </> "Resume.hs"
      capturePath = work </> "GeneratedScaffoldCapture.hs"
      target = work </> "Expr.hs"
      hiddenPath = work </> "hidden-scaffold.cbor"
      hidden = emptySessionScope {ssRoot=work,ssExactScope=Just hiddenPath}
      includes = [work]
      originalOwners = filter (/= "GeneratedScaffoldCapture") . preparedNames
  createDirectoryIfMissing True supportDirectory
  copyFile "lib/Tidepool/Internal/Resume.hs" supportPath
  copyFile "test-source-boot/fixtures/GeneratedScaffoldCapture.hs" capturePath
  copyFile "test-source-boot/fixtures/GeneratedScaffoldExpr.hs" target
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing capturePath includes Nothing
  writeGenuineCandidateNativeScope [] (originalOwners original)
    work capturePath includes hiddenPath original
  protected <- readFile target
  recipe <- generatedScaffoldRecipe [] protected protected target "Expr" >>= either fail pure
  let purpose = GeneratedScaffoldCompile recipe (CheckedItemCompile [] Nothing [])
      requireRejected label action = do
        result <- try (void action) :: IO (Either SomeException ())
        case result of
          Left reason -> do
            let detail = show reason
            putStrLn ("scaffold refused " ++ label ++ ": " ++ take 512 detail)
          Right _ -> fail ("scaffold authority accepted " ++ label)
  withResidentPipelineSelected includes $ \compile -> do
    admitted <- compile (PreparedProducts Nothing) Set.empty purpose (Just hidden) target [] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult admitted))) $
      fail "generated scaffold lost its actual settled result"
    -- Native custody alone does not admit this authored import. Only the
    -- protected scaffold occurrence above may use the hidden Resume owner.
    general <- try (void (compile (PreparedProducts Nothing) Set.empty GeneralCompile
      (Just hidden) target [] Nothing)) :: IO (Either InputRejection ())
    case general of
      Left (OriginalSourceSelectionRejected
          (ExecutionSourceUnavailable ("main", "Tidepool.Internal.Resume"))) ->
        putStrLn "scaffold general-purpose import refused missing current-source authority"
      Left reason -> fail ("scaffold general-purpose import had another input refusal: " ++ show reason)
      Right () -> fail "scaffold general-purpose import acquired hidden source authority"
    let extra = unlines (take 5 (lines protected) ++ ["import Tidepool.Internal.Resume"] ++ drop 5 (lines protected))
    writeFile target extra
    duplicate <- generatedScaffoldRecipe [] protected extra target "Expr" >>= either fail pure
    requireRejected "additional authored hidden import" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile duplicate GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target (protected ++ "\ntampered = 0 :: Int\n")
    requireRejected "changed rendered target" $
      compile (PreparedProducts Nothing) Set.empty purpose (Just hidden) target [] Nothing
    writeFile target protected
    copyFile "test-source-boot/fixtures/GeneratedScaffoldHelper.hs" (work </> "GeneratedScaffoldHelper.hs")
    let helperTarget = unlines (take 5 (lines protected) ++ ["import GeneratedScaffoldHelper"] ++ drop 5 (lines protected))
    writeFile target helperTarget
    helperRecipe <- generatedScaffoldRecipe [] protected helperTarget target "Expr" >>= either fail pure
    requireRejected "fresh helper importing hidden support" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile helperRecipe GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target ("{-# LINE 100 \"authored.hs\" #-}\n" ++ protected)
    lineRecipe <- generatedScaffoldRecipe [] protected ("{-# LINE 100 \"authored.hs\" #-}\n" ++ protected) target "Expr" >>= either fail pure
    requireRejected "logical LINE import location differs from protected occurrence" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile lineRecipe GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target protected
    admittedScope <- readExactScope hiddenPath >>= either fail pure
    unless (null (scopeLexical admittedScope)
        && Set.fromList (map originalModule (scopeProducts admittedScope))
          == Set.fromList (originalOwners original)) $
      fail "scaffold capture changed lexical authority or original native inventory"
    supportInterface <- case [artifact | (artifact,_,_) <- scopeInterfaces admittedScope
        , exactUnit artifact == "main", exactModule artifact == "Tidepool.Internal.Resume"] of
      [artifact] -> pure artifact
      _ -> fail "template interface fixture lacks its exact captured owner"
    let templateInterface = CheckedTemplateInterface (exactUnit supportInterface)
          (exactModule supportInterface) (exactSha256 supportInterface) []
        withTemplate = unlines (take 5 (lines protected)
          ++ ["import Tidepool.Internal.Resume"] ++ drop 5 (lines protected))
    writeFile target withTemplate
    capturedTemplate <- generatedScaffoldRecipe [templateInterface] withTemplate withTemplate target "Expr"
      >>= either fail pure
    let capturedPurpose = GeneratedScaffoldCompile capturedTemplate (CheckedItemCompile [] Nothing [])
    interfaceOnly <- compile (PreparedProducts Nothing) Set.empty capturedPurpose (Just hidden) target [] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult interfaceOnly))) $
      fail "initial template interface changed the native result"
    requireRejected "template interface cannot replace paired native owner" $
      compile (PreparedProducts Nothing) Set.empty
        (CellProgramCompile capturedPurpose admittedScope {scopeProducts=[]}) (Just hidden) target [] Nothing
    wrongSeal <- generatedScaffoldRecipe [templateInterface {templateInterfaceSha256=replicate 64 'f'}]
      withTemplate withTemplate target "Expr" >>= either fail pure
    requireRejected "changed initial template interface seal" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile wrongSeal GeneralCompile)
        (Just hidden) target [] Nothing
    let secondImport = unlines (take 5 (lines withTemplate)
          ++ ["import qualified Tidepool.Internal.Resume as AuthoredSecond"] ++ drop 5 (lines withTemplate))
    writeFile target secondImport
    secondRecipe <- generatedScaffoldRecipe [templateInterface] withTemplate secondImport target "Expr"
      >>= either fail pure
    requireRejected "authored second import beside permitted initial template import" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile secondRecipe GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target (withTemplate ++ "\ntamperedTemplateTarget = 0 :: Int\n")
    requireRejected "same owner with changed template target bytes" $
      compile (PreparedProducts Nothing) Set.empty capturedPurpose (Just hidden) target [] Nothing
    writeFile target protected
    forM_ ["Bind","Display"] $ \name -> do
      let rendered = T.unpack (T.replace "module Expr where" (T.pack ("module " ++ name ++ " where")) (T.pack protected))
          generatedPath = work </> (name ++ ".hs")
      writeFile generatedPath rendered
      generated <- generatedScaffoldRecipe [] protected rendered generatedPath name >>= either fail pure
      let wrapped = CellProgramCompile (GeneratedScaffoldCompile generated (CheckedItemCompile [] Nothing [])) admittedScope
      checked <- compile (PreparedProducts Nothing) Set.empty wrapped (Just hidden) generatedPath [] Nothing
      unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult checked))) $
        fail ("generated " ++ name ++ " lost its settled result or CellProgram wrapper")
    requireRejected "missing paired original native owner" $
      compile (PreparedProducts Nothing) Set.empty
        (CellProgramCompile purpose admittedScope {scopeProducts=[]}) (Just hidden) target [] Nothing
    let alteredOwner product' = product' {originalIfaceSha256=replicate 64 'f'}
    -- A product with another paired interface cannot grant scaffold authority.
    let differentScope = admittedScope {scopeProducts=map alteredOwner (scopeProducts admittedScope)}
    requireRejected "wrong paired original interface identity" $
      compile (PreparedProducts Nothing) Set.empty (CellProgramCompile purpose differentScope)
        (Just hidden) target [] Nothing
    supportText <- BSC.unpack <$> BS.readFile supportPath
    let incompleteExports = unlines [if line == "  , resumeLifted" then "" else line | line <- lines supportText]
    writeFile supportPath incompleteExports
    missingExport <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing capturePath [] Nothing
    let missingExportPath = work </> "missing-export.cbor"
    writeGenuineCandidateNativeScope [] (originalOwners missingExport)
      work capturePath includes missingExportPath missingExport
    requireRejected "missing actual resumeLifted export" $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just missingExportPath}) target [] Nothing
    forM_ ["ExecutionClass.hs","ExecutionHiddenOrphan.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
    let withOrphan = unlines [if line == "import Prelude" then
          "import Prelude\nimport ExecutionHiddenOrphan ()" else line | line <- lines supportText]
    writeFile supportPath withOrphan
    hiddenNeighbor <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing capturePath [] Nothing
    let neighborPath = work </> "hidden-neighbor.cbor"
    writeGenuineCandidateNativeScope [] (originalOwners hiddenNeighbor)
      work capturePath includes neighborPath hiddenNeighbor
    requireRejected "hidden orphan neighbor through scaffold support" $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just neighborPath}) target [] Nothing
    copyFile "test-source-boot/fixtures/MetadataHiddenFamily.hs" (work </> "MetadataHiddenFamily.hs")
    let withFamily = unlines [if line == "import Prelude" then
          "import Prelude\nimport MetadataHiddenFamily ()" else line | line <- lines supportText]
    writeFile supportPath withFamily
    hiddenFamily <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing capturePath [] Nothing
    let familyPath = work </> "hidden-family.cbor"
    writeGenuineCandidateNativeScope [] (originalOwners hiddenFamily)
      work capturePath includes familyPath hiddenFamily
    requireRejected "hidden family neighbor through scaffold support" $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just familyPath}) target [] Nothing
    writeFile supportPath supportText
    let metadataPath = work </> "CellCheck.hs"
    copyFile "test-source-boot/fixtures/GeneratedScaffoldMetadata.hs" metadataPath
    requireRejected "generated alias in authored metadata" $
      compile CheckedEnvironment Set.empty GeneralCompile (Just hidden) metadataPath [] Nothing
    -- A current source implementation uses ordinary source admission, not the
    -- generated edge exception. It must not require a retained exact product.
    cold <- compile (PreparedProducts Nothing) Set.empty purpose Nothing target [] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult cold))) $
      fail "generated cold source scaffold failed ordinary support admission"
  putStrLn "generated scaffold: exact hidden support, settled result, ordinary/cold scope, bind/display CellProgram; duplicate/helper/source-drift/native/export/hidden-orphan/family/metadata refusals passed"

-- The producer fixture retains real published native/interface/Home-seal
-- bytes. Only the transport envelope is composed here; this test does not
-- substitute for Rust's original certificate issuer or the lost G3 scope.
generatedScaffoldRetained :: FilePath -> FilePath -> IO ()
generatedScaffoldRetained manifestPath sealPath = withTiming $ withScratch $ \work -> do
  scope <- readExactScope manifestPath >>= either fail pure
  (artifact,original) <- case (scopeInterfaces scope,scopeProducts scope) of
    ([(artifact,_,_)],[original]) -> pure (artifact,original)
    _ -> fail "retained scaffold fixture lacks one original pair"
  let owner = (originalUnit original,originalModule original)
      fullOwner = TList (map (TString . T.pack) [originalUnit original,originalModule original,
        originalVersion original,originalIfaceSha256 original,originalProductSha256 original])
      term path = BS.readFile path >>= either (fail . show) (pure . snd)
        . deserialiseFromBytes decodeTerm . BSL.fromStrict
      scopeSession = emptySessionScope {ssRoot=work,ssExactScope=Just manifestPath}
  seal <- term sealPath
  case seal of
    TList (TString "TPHOMEOWNERS":_:sealedOwner:_:TList [required]:_)
      | sealedOwner == fullOwner && required == fullOwner -> pure ()
    _ -> fail "production Home seal does not retain the exact native self owner"
  unless (owner == ("main","Tidepool.Internal.Resume") && exactRequirements artifact == [owner]) $
    fail "retained scaffold fixture lost its native self requirement"
  native <- term (originalProductPath original)
  interfaceBytes <- BS.readFile (exactPath artifact)
  case native of
    TList [TString "TPMOD",TInt 1,TList [TList [unit,name,TBytes paired,TList groups]]]
      | [unit,name] == map (TString . T.pack) [fst owner,snd owner]
      , paired == interfaceBytes, length groups == length (originalGroups original) -> pure ()
    _ -> fail "production scaffold native bytes are not paired with the exact GHC interface"
  let target = work </> "Expr.hs"
  copyFile "test-source-boot/fixtures/GeneratedScaffoldExpr.hs" target
  source <- readFile target
  recipe <- generatedScaffoldRecipe [] source source target "Expr" >>= either fail pure
  let purpose = GeneratedScaffoldCompile recipe (CheckedItemCompile [] Nothing [])
      reject label expected action = do
        refused <- try (void action) :: IO (Either SomeException ())
        case refused of
          Left reason | expected `isInfixOf` show reason ->
            putStrLn ("retained scaffold refused " ++ label ++ ": " ++ take 512 (show reason))
          Left reason -> fail ("retained scaffold unexpected refusal for " ++ label ++ ": " ++ show reason)
          Right _ -> fail ("retained scaffold accepted " ++ label)
  withResidentPipelineSelected [work] $ \compile -> do
    reject "general compile" "source graph imports unadmitted home implementation" $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile
      (Just scopeSession) target [] Nothing
    result <- compile (PreparedProducts Nothing) Set.empty purpose (Just scopeSession) target [] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult result))) $
      fail "real retained Resume self-custody lost settled result"
    originalTerm <- term manifestPath
    copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
    helper <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing (work </> "MetadataQuoteSupport.hs") [] Nothing
    let helperScope = work </> "helper.cbor"
    writeExecutionScope helperScope work helper []
    helperTerm <- term helperScope
    let encode = toStrictByteString . encodeTerm
        replacement requirement includeHelper = case (originalTerm,helperTerm) of
          (TList fields,TList helperFields) -> TList [case index of
            4 -> case field of
              TList [TList ownerFields] -> TList ([TList [if column == 4 then TList requirement else value
                | (column,value) <- zip [0::Int ..] ownerFields]] ++ if includeHelper then
                  case helperFields !! 4 of TList rows -> rows; _ -> [] else [])
              _ -> field
            6 | includeHelper -> case (field,helperFields !! 6) of
              (TList rows,TList extra) -> TList (rows ++ extra)
              _ -> field
            _ -> field | (index,field) <- zip [0::Int ..] fields]
          _ -> error "retained fixture scope framing changed"
        key unit name = TList (map (TString . T.pack) [unit,name])
    forM_ [("foreign home requirement",[key "main" "Tidepool.Internal.Resume",key "main" "MetadataQuoteSupport"],True)
      ,("wrong unit self requirement",[key "foreign" "Tidepool.Internal.Resume"],False)] $ \(label,requirements,includeHelper) -> do
        let changedPath = work </> (if includeHelper then "foreign.cbor" else "wrong-unit.cbor")
        BS.writeFile changedPath (encode (replacement requirements includeHelper))
        reject label (if includeHelper then "generated scaffold support requires another home implementation owner"
          else "incomplete or conflicting exact owner closure") $ compile (PreparedProducts Nothing) Set.empty purpose
          (Just scopeSession {ssExactScope=Just changedPath}) target [] Nothing
  after <- term sealPath
  unless (after == seal) (fail "scaffold consumer changed the original Home seal")
  putStrLn "retained scaffold: real production full owner/native/interface/seal self-custody admitted, foreign and wrong-unit requirements refused; original proof unchanged"

retainedExecutionPublication :: IO ()
retainedExecutionPublication = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoter.hs", "MetadataQuotedTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  copyFile "test-source-boot/fixtures/MetadataQuoteSupportModuleFlags.hs" (work </> "MetadataQuoteSupport.hs")
  let helper = mkModuleName "MetadataQuoteSupport"
      provider = mkModuleName "MetadataQuoter"
      target = mkModuleName "MetadataQuotedTarget"
      helperPath = work </> "MetadataQuoteSupport.hs"
      providerPath = work </> "MetadataQuoter.hs"
      targetPath = work </> "MetadataQuotedTarget.hs"
      scopePath = work </> "original-execution.cbor"
      session = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
  -- The request target is generated-source evidence, not an original recipe.
  -- Capture the helper as real source support of the quoter producer.
  original <- runPipelineSelected (PreparedProducts Nothing) providerPath [work]
  let originalEnvironment = prHscEnv (pprPipelineResult original)
      helperFlags = [ms_hspp_opts summary | ModuleNode _ summary <-
        mgModSummaries' (hsc_mod_graph originalEnvironment), ms_mod_name summary == helper]
  unless (case helperFlags of
      [flags] -> xopt LangExt.TypeFamilies flags
        && not (xopt LangExt.TypeFamilies (hsc_dflags originalEnvironment))
      _ -> False) $
    fail "retained support must require module flags absent from the session defaults"
  writeGenuineOriginalExecutionScope ["MetadataQuoteSupport"] work providerPath [work] scopePath original
  exact <- readExactScope scopePath >>= either fail pure
  originalBytes <- BS.readFile scopePath
  deferredOriginalModuleFlags work session
  withResidentPipelineSelected [work] $ \compile -> do
    (prepared, diagnostics) <- captureDiagnostics $ compile (PreparedProducts Nothing) Set.empty GeneralCompile
      (Just session) targetPath [work] Nothing
    let result = pprPipelineResult prepared
        environment = prHscEnv result
        freshOwners = Map.keysSet (pprFinalizedModules prepared)
    unless (hasIntResultLiteral 42 (prBinds result)
        && counterValues "exact_execution_original_load_owners" diagnostics == [1]
        && counterValues "exact_execution_fresh_provider_compiles" diagnostics == [1]
        && freshOwners == Set.fromList [provider,target]
        && Map.notMember helper (pprProductInterfaces prepared)) $
      fail "retained executable acquired fresh finalization ownership or fresh provider lost it"
    case lookupHpt (hsc_HPT environment) helper of
      Just hmi | isJust (homeMod_bytecode (hm_linkable hmi)) -> pure ()
      _ -> fail "retained original lost its authorized GHC bytecode"
    let captured = work </> "fresh-publication"
    createDirectory captured
    originals <- newOriginalInterfaceArtifacts environment (pprFinalizedModules prepared)
      [artifact | (artifact,_,_) <- scopeInterfaces exact] captured
    _ <- captureFinalizedModuleArtifacts originals environment (pprFinalizedModules prepared)
      (pprPackageImports prepared) (pprDependencies prepared) captured
    after <- BS.readFile scopePath
    unless (after == originalBytes) $ fail "execution mutated its original immutable admission"
    copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" helperPath
    fresh <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing helperPath [work] Nothing
    unless (Map.member helper (pprFinalizedModules fresh)
        && Map.member helper (pprProductInterfaces fresh)) $
      fail "ordinary source refresh inherited retained execution ownership"
    changed <- try (compile (PreparedProducts Nothing) Set.empty GeneralCompile
      (Just session) targetPath [work] Nothing) :: IO (Either SomeException PreparedPipelineResult)
    unless (case changed of Left failure -> "ExecutionSourceChanged" `isInfixOf` show failure; _ -> False) $
      fail "retained execution accepted the refreshed source under its old admission"
  putStrLn "retained execution publication: bytecode retained, only fresh providers captured, source refresh separate"

-- The exact helper makes both its fresh importer and that importer's consumer
-- deferred. Retain the actual finalized interface across request cleanup, then
-- ask GHC to validate it against the next request's source summary and HPT.
-- This checks GHC interface freshness, without issuing a new execution recipe.
deferredOriginalModuleFlags :: FilePath -> SessionScope -> IO ()
deferredOriginalModuleFlags work session = do
  forM_ ["MetadataDeferredModuleFlags.hs", "MetadataDeferredFlagsSibling.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  let owner = mkModuleName "MetadataDeferredModuleFlags"
      sibling = mkModuleName "MetadataDeferredFlagsSibling"
      target = work </> "MetadataDeferredFlagsSibling.hs"
      moduleSummary environment name = case [summary | ModuleNode _ summary <-
          mgModSummaries' (hsc_mod_graph environment), ms_mod_name summary == name] of
        [summary] -> pure summary
        _ -> fail "deferred flags fixture lacks its current source summary"
      checkFlags environment = do
        summary <- moduleSummary environment owner
        siblingSummary <- moduleSummary environment sibling
        unless (xopt LangExt.TypeFamilies (ms_hspp_opts summary)
            && not (xopt LangExt.TypeFamilies (ms_hspp_opts siblingSummary))
            && not (xopt LangExt.TypeFamilies (hsc_dflags environment))) $
          fail "deferred module flags escaped into its sibling or ambient session"
        pure summary
  withResidentPipelineSelectedRequests [work] $ \runRequest -> do
    original <- runRequest (pure ()) $ \compile -> do
      (checked, checkedDiagnostics) <- captureDiagnostics $
        compile CheckedEnvironment Set.empty GeneralCompile (Just session) target [work] Nothing
      _ <- checkFlags (crHscEnv checked)
      unless ("tidepool-checked-interface-retained module=MetadataDeferredModuleFlags consumer=MetadataDeferredFlagsSibling"
          `elem` lines checkedDiagnostics) $
        fail "module flags fixture did not retain a deferred checking interface for its sibling"
      (prepared, preparedDiagnostics) <- captureDiagnostics $
        compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just session) target [work] Nothing
      _ <- checkFlags (prHscEnv (pprPipelineResult prepared))
      unless (Map.member owner (pprFinalizedModules prepared)
          && not ("tidepool-canonical-frontend module=MetadataDeferredModuleFlags"
            `elem` lines preparedDiagnostics)) $
        fail "module flags fixture did not finalize its original through the deferred frontend"
      maybe (fail "deferred original omitted its finalized interface") pure
        (Map.lookup owner (pprProductInterfaces prepared))
    runRequest (pure ()) $ \compile -> do
      checked <- compile CheckedEnvironment Set.empty GeneralCompile (Just session) target [work] Nothing
      let environment = crHscEnv checked
      summary <- checkFlags environment
      decision <- checkOldIface (scopeRetainedSummaryHscEnv summary environment) summary (Just original)
      case decision of
        UpToDateItem _ -> pure ()
        OutOfDateItem reason _ -> fail
          ("next request rejected the deferred original interface: " ++ showSDocUnsafe (ppr reason))
  putStrLn "deferred module flags: checking sibling and ambient flags isolated, finalized original fresh in next request"

exactRetainedQuoter :: IO ()
exactRetainedQuoter = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs", "MetadataQuoter.hs", "MetadataQuotedTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  support <- runPipelineSelected (PreparedProducts Nothing) (work </> "MetadataQuoteSupport.hs") [work]
  let producer = prHscEnv (pprPipelineResult support)
      hi = work </> "retained-quote-support.hi"
      packagesPath = hi ++ ".packages"
      scopePath = work </> "exact-scope.cbor"
      scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath }
  iface <- maybe (fail "retained quoter helper omitted its interface") pure
    (Map.lookup (mkModuleName "MetadataQuoteSupport") (pprProductInterfaces support))
  writeBinIface (targetProfile (hsc_dflags producer)) QuietBinIFace NormalCompression hi iface
  bytes <- BS.readFile hi
  let artifact = ExactIfaceArtifact "main" "MetadataQuoteSupport" hi (digest bytes) []
      packages = encodePackageImports artifact
        (Map.findWithDefault emptyPackageImports (mkModuleName "MetadataQuoteSupport") (pprPackageImports support))
  BS.writeFile packagesPath packages
  writeExactMetadataScopeWithLexical scopePath [(artifact, packagesPath, digest packages)] [(artifact, [])]
  originalScope <- readExactScope scopePath >>= either fail pure
  withResidentPipelineSelected [work] $ \compile -> do
    absent <- try (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
      (work </> "MetadataQuotedTarget.hs") [work] Nothing) :: IO (Either SomeException CheckedEnvironmentResult)
    unless (case absent of Left reason -> "ExecutionSourceMissing" `isInfixOf` show reason; _ -> False) $
      fail "source-free execution borrowed source without an original recipe"
  writeExecutionScope scopePath work support ["MetadataQuoteSupport"]
  let changedPath = work </> "changed-scope.cbor"
      changedScope = scope {ssExactScope=Just changedPath}
      hiddenPath = work </> "hidden-scope.cbor"
      hiddenScope = scope {ssExactScope=Just hiddenPath}
  writeExecutionScope hiddenPath work support []
  copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
  supportB <- runPipelineSelected (PreparedProducts Nothing) (work </> "MetadataQuoteSupport.hs") [work]
  writeExecutionScope changedPath work supportB ["MetadataQuoteSupport"]
  copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
  withResidentPipelineSelectedRequests [work] $ \runRequest -> do
    let helperPath = work </> "MetadataQuoteSupport.hs"
        cancelMarker = work </> "cancel-marker"
        quoterPath = work </> "MetadataQuoter.hs"
    runRequest (pure ()) $ \compile -> do
      checked <- compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
      unless (fmap renderType (crResultType checked) == Just "Int") $
        fail "retained quoter execution changed its result type"
      native <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
      unless (fmap renderType (prResultType (pprPipelineResult native)) == Just "Int"
          && hasIntResultLiteral 42 (prBinds (pprPipelineResult native))
          && not (isJust (hscCompileCoreExprHook (hsc_hooks (prHscEnv (pprPipelineResult native)))))) $
        fail "retained quoter native execution changed its result type"
      copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" helperPath
      refused <- try (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing) :: IO (Either SomeException CheckedEnvironmentResult)
      unless (case refused of Left reason -> "ExecutionSourceChanged" `isInfixOf` show reason; _ -> False) $
        fail "retained quoter executed a changed original source"
      let preprocessor = work </> "changed-preprocessor"
          preprocessMarker = work </> "preprocess-marker"
      writeFile preprocessor ("#!/bin/sh\n: > " ++ show preprocessMarker ++ "\nexit 1\n")
      permissions <- getPermissions preprocessor
      setPermissions preprocessor permissions {executable=True}
      preprocessingSource <- readFile "test-source-boot/fixtures/ExecutionChangedPreprocessor.hs"
      writeFile helperPath (T.unpack (T.replace "EXECUTION_PREPROCESSOR" (T.pack preprocessor) (T.pack preprocessingSource)))
      preprocessed <- try (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing) :: IO (Either SomeException CheckedEnvironmentResult)
      ranPreprocessor <- doesFileExist preprocessMarker
      unless (case preprocessed of Left reason -> "ExecutionSourceChanged" `isInfixOf` show reason && not ranPreprocessor; _ -> False) $
        fail "changed original source executed preprocessing before recipe admission"
      copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" helperPath
      nativeB <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just changedScope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
      unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult nativeB))) $
        fail "execution recipe B linked the previous source owner's bytecode"
      copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" helperPath
      nativeA <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
      unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult nativeA))) $
        fail "execution recipe A/B/A retained a changed helper body"
    runRequest (pure ()) $ \compile -> do
      cancellingSource <- readFile "test-source-boot/fixtures/ExecutionCancellingQuoter.hs"
      writeFile quoterPath (T.unpack (T.replace "\"EXECUTION_CANCEL_MARKER\"" (T.pack (show cancelMarker)) (T.pack cancellingSource)))
      cancelled <- try (timeout 1500000 (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing)) :: IO (Either SomeException (Maybe CheckedEnvironmentResult))
      beganExecution <- doesFileExist cancelMarker
      unless (beganExecution && case cancelled of Right (Just _) -> False; _ -> True) $
        fail "execution cancellation did not reach the scoped splice linker"
      terminal <- try (timeout 1000000 (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing)) :: IO (Either SomeException (Maybe CheckedEnvironmentResult))
      unless (case terminal of Left _ -> True; Right _ -> False) $
        fail "cancelled compiler callback remained usable"
    runRequest (pure ()) $ \compile -> do
      copyFile "test-source-boot/fixtures/MetadataQuoter.hs" quoterPath
      copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" helperPath
      afterCancel <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just changedScope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
      unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult afterCancel))
          && not (isJust (hscCompileCoreExprHook (hsc_hooks (prHscEnv (pprPipelineResult afterCancel)))))) $
        fail "cancelled execution A leaked its linker view into B"
      copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" helperPath
      (hidden,diagnostics) <- captureDiagnostics (try (compile CheckedEnvironment Set.empty GeneralCompile (Just hiddenScope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing) :: IO (Either SomeException CheckedEnvironmentResult))
      unless (case hidden of
        Left reason
          | Just (OriginalSourceSelectionRejected
              (ExecutionSourceUnavailable ("main", "MetadataQuoteSupport"))) <- fromException reason ->
            not ("tidepool-timing phase=ghc_load" `isInfixOf` diagnostics)
        _ -> False) $
        fail ("hidden lexical import did not refuse its missing source-selection authority: "
          ++ either show (const "unexpected success") hidden)
  unless (null (scopeExecutionOwners originalScope)) (fail "legacy scope gained execution authority")
  putStrLn "exact retained quoter: metadata/native, missing/change/preprocess refusals, A/B/A, cancellation, hidden-import preflight passed"

-- The GHC fixture issues the original recipe alongside actual native group
-- bytes and the producer's positive source/resolution evidence. Rust's real
-- prepare_compilation emitter is exercised separately by executionSourceWire.
writeExecutionScope :: FilePath -> FilePath -> PreparedPipelineResult -> [String] -> IO ()
writeExecutionScope path work original lexicalNames = do
  let productRoot = path ++ ".products"
  createDirectoryIfMissing True productRoot
  let environment = prHscEnv (pprPipelineResult original)
      context name = ProjectionContext "test" "matched"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        (SymbolIdentity "main" (T.pack name) "value" "answerValue" Nothing) [] Nothing Nothing Nothing Nothing
      text = TString . T.pack
      key unit name = TList [text unit,text name]
      optional = maybe TNull text
      symbol originalSymbol = TList [TString (symbolUnit originalSymbol),TString (symbolModule originalSymbol)
        ,TString (symbolNamespace originalSymbol),TString (symbolOccurrence originalSymbol)
        ,maybe TNull TString (symbolRecordParent originalSymbol)]
      originalGroup group = TList [TInt (fromIntegral (projectedOriginalOrdinal group))
        ,TList (map symbol (projectedBinders group)),TList [TList [symbol (globalIdentity global)
          ,TBool (isJust (globalRequiredGeneration global))] | global <- projectedGlobals (projectedBody group)]]
      evidence = pprDependencies original
  unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence) $
    fail "execution fixture lacks positive original input evidence"
  rows <- forM (pprModules original) $ \prepared -> do
    let name = moduleNameString (moduleName (pmModule prepared))
        unit = unitString (moduleUnit (pmModule prepared))
        hi = productRoot </> (name ++ ".execution.hi")
        nativePath = hi ++ ".tpmod"
    iface <- maybe (fail "original execution owner lacks an interface") pure
      (Map.lookup (mkModuleName name) (pprProductInterfaces original))
    writeBinIface (targetProfile (hsc_dflags environment)) QuietBinIFace NormalCompression hi iface
    bytes <- BS.readFile hi
    groups <- either (fail . show) pure (projectPreparedModuleGroups (context name) prepared)
    let native = encodeModuleProducts [(T.pack unit,T.pack name,bytes,groups)]
        nativeSha = digest native
        version = nativeSha
    BS.writeFile nativePath native
    requirements <- either fail pure (selectedHomeRequirements evidence unit name)
    let artifact = ExactIfaceArtifact unit name hi (digest bytes) requirements
        packages = encodePackageImports artifact
          (Map.findWithDefault emptyPackageImports (mkModuleName name) (pprPackageImports original))
        packagePath = hi ++ ".packages"
        identity = [text unit,text name,text version,text (digest bytes),text nativeSha]
        owner = TList (map text [unit,name,hi,digest bytes]
          ++ [TList [key u m | (u,m) <- requirements],text packagePath,text (digest packages)])
        productRow = TList [text unit,text name,text version,text (digest bytes),text nativeSha,text nativePath,TList (map originalGroup groups)]
    BS.writeFile packagePath packages
    pure (name,identity,owner,productRow)
  let list f values = TList (map f values)
      source row = TList [text (dependencySourcePath row),text (dependencySourceSha256 row)]
      resolution row = TList [text (renderDependencyQualifier (dependencyResolutionQualifier row)),text (dependencyResolutionModule row),TBool (dependencyResolutionBoot row)
        ,optional (dependencyResolutionSelected row),list text (dependencyResolutionCandidates row)]
      imported row = TList [text (renderDependencyQualifier (dependencyImportQualifier row)),text (dependencyImportName row),TBool (dependencyImportBoot row),optional (dependencyImportSelected row)]
      node row = TList [text (dependencyModuleUnit row),text (dependencyModuleName row),TBool (dependencyModuleBoot row)
        ,text (dependencyModuleSource row),list imported (dependencyModuleImports row),text "ready"]
      proof = TList [TBool True,TBool True,list source (dependencySources evidence),list resolution (dependencyResolutions evidence)
        ,list node (dependencyModules evidence),list text (dependencyPackages evidence)]
      zero = replicate 64 '0'
  origin <- case dependencyModules evidence of
    first:_ -> pure (dependencyModuleSource first)
    [] -> fail "execution fixture has no source owner"
  originalText <- readFile origin
  let graph = TList [text "TPEXECUTIONSOURCE",TInt 1,text "tidepool-ghc-pipeline-v1",text zero,TNull
        ,list text [work],TList [text origin,text originalText],proof
        ,TList [TList (identity ++ [TBool True,TNull]) | (_,identity,_,_) <- rows],TList [],TList []]
      graphBytes = toStrictByteString (encodeTerm graph)
      graphSha = digest graphBytes
      graphPath = takeDirectory path </> ("execution-" ++ graphSha ++ ".cbor")
      scope = TList [text "TPEXACTSCOPE",text "6",text zero,text zero,TList [owner | (_,_,owner,_) <- rows]
        ,TList [TList [key "main" name,TList []] | name <- lexicalNames]
        ,TList [productRow | (_,_,_,productRow) <- rows]
        ,TList [TList [TList [text graphSha,text graphPath]],TList [TList (identity ++ [text graphSha]) | (_,identity,_,_) <- rows]],TNull]
  BS.writeFile graphPath graphBytes
  BS.writeFile path (toStrictByteString (encodeTerm scope))

exactReexportQuoter :: IO ()
exactReexportQuoter = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs","MetadataQuoter.hs","ExecutionReexportFacade.hs"
    ,"ExecutionReexportTarget.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionReexportFacade.hs") [work] Nothing
  let scopePath = work </> "reexport-scope.cbor"
      scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
  writeExecutionScope scopePath work original ["ExecutionReexportFacade"]
  withResidentPipelineSelected [work] $ \compile -> do
    (result,diagnostics) <- captureDiagnostics (compile (PreparedProducts Nothing) Set.empty GeneralCompile
      (Just scope) (work </> "ExecutionReexportTarget.hs") [work] Nothing)
    mapM_ putStrLn [row | row <- lines diagnostics, "tidepool-exact-execution-load " `isPrefixOf` row]
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult result))) $
      fail "retained facade did not execute its original defining quoter"
    unless (counterValues "exact_execution_original_load_owners" diagnostics == [2]) $
      fail "reexport fixture did not select exactly the defining quoter and pure helper"
    let loadRows = [row | row <- lines diagnostics, "tidepool-exact-execution-load " `isPrefixOf` row]
    unless (length loadRows == 2 && all ("allow_object=False bytecode=True" `isInfixOf`) loadRows
        && not (any ("ExecutionReexportFacade" `isInfixOf`) loadRows)) $
      fail "reexport execution did not obtain real bytecode only for its selected original closure"
    let environment = prHscEnv (pprPipelineResult result)
    unless (all (\target -> targetAllowObjCode target)
        (hsc_targets environment)) $
      fail "authenticated execution target policy escaped its load bracket"
    copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
    changed <- try (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionReexportTarget.hs") [work] Nothing)
      :: IO (Either SomeException CheckedEnvironmentResult)
    unless (case changed of Left reason -> "ExecutionSourceChanged" `isInfixOf` show reason; _ -> False) $
      fail "reexport quoter executed a changed authenticated helper"
    copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
    recovered <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionReexportTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult recovered))) $
      fail "failed reexport admission leaked its execution targets into the next cycle"
  putStrLn "execution reexport: thin facade selects only quoter and helper, executes, and restores load targets"

exactExecutionHiddenInstance :: IO ()
exactExecutionHiddenInstance = withTiming $ withScratch $ \work -> do
  forM_ ["ExecutionClass.hs","ExecutionHiddenOrphan.hs","ExecutionSealedQuoter.hs"
    ,"ExecutionFreshQuoter.hs","ExecutionSealedTarget.hs","ExecutionFreshTarget.hs"
    ,"ExecutionQualifiedTarget.hs","ExecutionHiddenQuoteTarget.hs"
    ,"ExecutionClassQuoter.hs","ExecutionClassQuoteTarget.hs","ExecutionClassQuoteHidden.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionSealedQuoter.hs") [work] Nothing
  let scopePath = work </> "sealed-scope.cbor"
      scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
  writeExecutionScope scopePath work original ["ExecutionSealedQuoter"]
  classOriginal <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionClassQuoter.hs") [work] Nothing
  let classScopePath = work </> "class-scope.cbor"
      classScope = scope {ssExactScope=Just classScopePath}
  writeExecutionScope classScopePath work classOriginal ["ExecutionClassQuoter"]
  withResidentPipelineSelected [work] $ \compile -> do
    sealed <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionSealedTarget.hs") [work] Nothing
    unless (fmap renderType (prResultType (pprPipelineResult sealed)) == Just "Int"
        && hasIntResultLiteral 42 (prBinds (pprPipelineResult sealed))) $
      fail "sealed original quoter lost its authenticated private orphan dictionary"
    qualified <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionQualifiedTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult qualified))) $
      fail "qualified quoter lost its full defining original owner"
    (hiddenQuote,hiddenDiagnostics) <- captureDiagnostics (try (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionHiddenQuoteTarget.hs") [work] Nothing) :: IO (Either SomeException CheckedEnvironmentResult))
    unless (case hiddenQuote of Left _ -> counterValues "exact_execution_original_load_owners" hiddenDiagnostics == [0]; _ -> False) $
      fail "hidden qualified export acquired an execution recipe"
    (fresh,diagnostics) <- captureDiagnostics (try (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionFreshTarget.hs") [work] Nothing) :: IO (Either SomeException CheckedEnvironmentResult))
    unless (case fresh of Left reason -> "No instance for" `isInfixOf` (show reason ++ diagnostics); Right _ -> False) $
      fail ("fresh provider borrowed a hidden execution-only instance: "
        ++ either show (const "ACCEPTED") fresh ++ "\n" ++ diagnostics)
    restored <- compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionSealedTarget.hs") [work] Nothing
    unless (fmap renderType (crResultType restored) == Just "Int") $
      fail "fresh-provider refusal leaked its execution environment"
    classQuote <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just classScope)
      (work </> "ExecutionClassQuoteTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult classQuote))) $
      fail "class parent wildcard import lost its exported quoter method"
    (classHidden,classDiagnostics) <- captureDiagnostics (try (compile CheckedEnvironment Set.empty GeneralCompile
      (Just classScope) (work </> "ExecutionClassQuoteHidden.hs") [work] Nothing)
      :: IO (Either SomeException CheckedEnvironmentResult))
    unless (case classHidden of Left _ -> counterValues "exact_execution_original_load_owners" classDiagnostics == [0]; _ -> False) $
      fail "hiding a class parent acquired its child quoter execution capability"
  putStrLn "execution instances: sealed quoter executes, fresh provider cannot borrow private orphan, failure recovers"

-- The returned environment includes this cycle's collector. Removing it must
-- leave the empty boot stack; an older collector would make another pop succeed.
assertSingleDiagnosticCollector :: HscEnv -> IO ()
assertSingleDiagnosticCollector environment = do
  bootLogger <- evaluate (popLogHook (hsc_logger environment))
  older <- try (evaluate (popLogHook bootLogger)) :: IO (Either SomeException Logger)
  unless (case older of Left _ -> True; Right _ -> False) $
    fail "compiler request retained diagnostic collectors from previous cycles"

exactToOrdinary :: IO ()
exactToOrdinary = withTiming $ withScratch $ \work -> do
  forM_ ["ExecutionClass.hs","ExecutionHiddenOrphan.hs","ExecutionSealedQuoter.hs"
    ,"ExecutionSealedTarget.hs","ExecutionClassQuoter.hs","ExecutionClassQuoteTarget.hs"
    ,"MetadataQuoteSupport.hs","MetadataQuoter.hs","MetadataQuotedTarget.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  sealed <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionSealedQuoter.hs") [work] Nothing
  helper <- runPipelineSelected (PreparedProducts Nothing) (work </> "MetadataQuoteSupport.hs") [work]
  let valueDirectory = work </> "Tidepool/Session/Val"
  createDirectoryIfMissing True valueDirectory
  copyFile "test-source-boot/fixtures/CheckedValueG2.hs" (valueDirectory </> "G2.hs")
  copyFile "test-source-boot/fixtures/CheckedValueConsumer.hs" (work </> "CheckedValueConsumer.hs")
  valueProducer <- runPipelineSelected (PreparedProducts Nothing) (work </> "CheckedValueConsumer.hs") [work]
  valueModule <- maybe (fail "invalid legacy value fixture owner") pure (parseValModule "Tidepool.Session.Val.G2")
  valueIface <- maybe (fail "legacy value producer omitted its interface") pure
    (Map.lookup (mkModuleName "Tidepool.Session.Val.G2") (pprProductInterfaces valueProducer))
  writeBinIface (targetProfile (hsc_dflags (prHscEnv (pprPipelineResult valueProducer)))) QuietBinIFace NormalCompression
    (sessionHiPath work valueModule) valueIface
  renameFile (valueDirectory </> "G2.hs") (valueDirectory </> "G2.retained-source")
  let scopePath = work </> "sealed-scope.cbor"
      hiddenPath = work </> "hidden-scope.cbor"
      helperPath = work </> "helper-scope.cbor"
      scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
  writeExecutionScope scopePath work sealed ["ExecutionSealedQuoter"]
  writeExecutionScope hiddenPath work sealed []
  writeExecutionScope helperPath work helper ["MetadataQuoteSupport"]
  withResidentPipelineSelectedRequests [work] $ \runRequest -> do
    let ordinaryQuote compile = do
          result <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
            (work </> "ExecutionClassQuoteTarget.hs") [work] Nothing
          assertSingleDiagnosticCollector (prHscEnv (pprPipelineResult result))
          unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult result))
              && not (isJust (hscCompileCoreExprHook (hsc_hooks (prHscEnv (pprPipelineResult result)))))
              && not (isJust (lookupHpt (hsc_HPT (prHscEnv (pprPipelineResult result))) (mkModuleName "ExecutionHiddenOrphan")))
              && not (isJust (lookupHpt (hsc_HPT (prHscEnv (pprPipelineResult result))) (mkModuleName "Tidepool.Session.Val.G2")))) $
            fail "ordinary request inherited an exact execution environment"
    runRequest (pure ()) $ \compile -> do
      _ <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
        (work </> "ExecutionSealedTarget.hs") [work] Nothing
      ordinary <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile Nothing
        (work </> "ExecutionClassQuoter.hs") [work] Nothing
      unless ("ExecutionClassQuoter" `elem` preparedNames ordinary) $
        fail "ordinary certification after an exact request omitted its source owner"
      ordinaryQuote compile
      ordinaryQuote compile
      _ <- compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
        (work </> "ExecutionSealedTarget.hs") [work] Nothing
      afterExactLegacy <- compile CheckedEnvironment Set.empty GeneralCompile
        (Just emptySessionScope {ssRoot=work,ssValIfaces=[valueModule]}) (work </> "CheckedValueConsumer.hs") [work] Nothing
      unless (fmap renderType (crResultType afterExactLegacy) == Just "Int") $
        fail "legacy value request inherited the preceding exact graph"
      ordinaryQuote compile
      (refused, refusalDiagnostics) <- captureDiagnostics $ try (compile CheckedEnvironment Set.empty GeneralCompile
        (Just scope {ssExactScope=Just hiddenPath}) (work </> "ExecutionSealedTarget.hs") [work] Nothing)
        :: IO (Either SomeException CheckedEnvironmentResult, String)
      unless (case refused of
        Left reason
          | Just (OriginalSourceSelectionRejected
              (ExecutionSourceUnavailable ("main", "ExecutionSealedQuoter"))) <- fromException reason ->
            not ("tidepool-timing phase=ghc_load" `isInfixOf` refusalDiagnostics)
        _ -> False) $
        fail ("hidden original did not refuse its missing source-selection authority: "
          ++ either show (const "unexpected success") refused)
      ordinaryQuote compile
    runRequest (pure ()) $ \compile -> do
      cancelling <- readFile "test-source-boot/fixtures/ExecutionCancellingQuoter.hs"
      let marker = work </> "cancel-marker"
      writeFile (work </> "MetadataQuoter.hs") (T.unpack (T.replace "EXECUTION_CANCEL_MARKER" (T.pack marker) (T.pack cancelling)))
      cancelled <- timeout 1500000 (compile CheckedEnvironment Set.empty GeneralCompile
        (Just scope {ssExactScope=Just helperPath}) (work </> "MetadataQuotedTarget.hs") [work] Nothing)
      started <- doesFileExist marker
      unless (isNothing cancelled && started) (fail "exact cancellation did not reach the real quoter")
      terminal <- try (timeout 1000000 (compile CheckedEnvironment Set.empty GeneralCompile
        (Just scope {ssExactScope=Just helperPath}) (work </> "MetadataQuotedTarget.hs") [work] Nothing))
        :: IO (Either SomeException (Maybe CheckedEnvironmentResult))
      unless (case terminal of Left _ -> True; Right _ -> False) $
        fail "cancelled compiler callback remained usable"
    runRequest (pure ()) $ \compile -> do
      copyFile "test-source-boot/fixtures/MetadataQuoter.hs" (work </> "MetadataQuoter.hs")
      copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
      afterCancel <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
      assertSingleDiagnosticCollector (prHscEnv (pprPipelineResult afterCancel))
      unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult afterCancel))
          && not (isJust (hscCompileCoreExprHook (hsc_hooks (prHscEnv (pprPipelineResult afterCancel)))))) $
        fail "ordinary request after cancellation reused the old helper executable"
      legacy <- compile CheckedEnvironment Set.empty GeneralCompile
        (Just emptySessionScope {ssRoot=work,ssValIfaces=[valueModule]}) (work </> "CheckedValueConsumer.hs") [work] Nothing
      unless (fmap renderType (crResultType legacy) == Just "Int") (fail "legacy value injection did not typecheck")
      ordinaryQuote compile
      provisional <- compile (PreparedProducts (Just (work </> "missing-candidates.cbor"))) Set.empty GeneralCompile Nothing
        (work </> "ExecutionClassQuoteTarget.hs") [work] Nothing
      unless (null (pprAcceptedCandidates provisional)) (fail "missing manifest unexpectedly admitted a candidate")
      ordinaryQuote compile
  putStrLn "exact to ordinary: successful, refused and cancelled scopes reset; ordinary reuse and provisional candidates remain valid"

exactExecutionValues :: IO ()
exactExecutionValues = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs","MetadataQuoter.hs","CheckedValueQuoterProducer.hs","CheckedValueQuoterTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  produced <- runPipelineSelected (PreparedProducts Nothing) (work </> "CheckedValueQuoterProducer.hs") [work]
  let result = pprPipelineResult produced
      scopePath = work </> "value-scope.cbor"
  valueOwner <- maybe (fail "invalid checked value fixture owner") pure (parseValModule "Tidepool.Session.Val.G8")
  _ <- mkBoundBinders ["answer"] 8 work result
  let valuePath = sessionHiPath work valueOwner
  bytes <- BS.readFile valuePath
  let value = ExactIfaceArtifact "main" "Tidepool.Session.Val.G8" valuePath (digest bytes) []
  writeExactMetadataScope scopePath []
  base <- readExactScope scopePath >>= either fail pure
  let admitted = base {scopePurpose=ExactCellPurpose (CheckedCellAdmission (replicate 64 '0') (replicate 64 '0')
        (replicate 64 '0') [] ["Tidepool.Session.Val.G8"] [] [value] Nothing AuthoredCellCheck) [work]}
      scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath,ssValIfaces=[valueOwner]}
  withResidentPipelineSelected [work] $ \compile -> do
    (refused,diagnostics) <- captureDiagnostics (try (compile CheckedEnvironment Set.empty
      (CellProgramCompile GeneralCompile admitted) (Just scope) (work </> "CheckedValueQuoterTarget.hs") [work] Nothing)
      :: IO (Either SomeException CheckedEnvironmentResult))
    unless (case refused of
      Left reason -> "ExecutionSourceMissing" `isInfixOf` show reason
        && "Tidepool.Session.Val.G8" `isInfixOf` show reason
        && not ("tidepool-timing phase=ghc_load" `isInfixOf` diagnostics)
      _ -> False) $ fail "checked value quoter entered GHC execution without an original source capability"
  putStrLn "execution values: protected value quoter refuses missing execution capability before GHC load"

originalPackageProjection :: IO ()
originalPackageProjection = withScratch $ \work -> do
  forM_ ["PackageOriginalSupport.hs", "PackageOriginalHome.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  let valueDirectory = work </> "Tidepool/Session/Val"
  createDirectoryIfMissing True valueDirectory
  copyFile "test-source-boot/fixtures/PackageOriginalVal.hs" (valueDirectory </> "G7.hs")
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "PackageOriginalSupport.hs") [work] Nothing
  let env = prHscEnv (pprPipelineResult original)
      modules = pprModules original
      context = ProjectionContext "test" "matched"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        (SymbolIdentity "main" "PackageOriginalSupport" "value" "packageFunction" Nothing)
        [] Nothing Nothing Nothing Nothing
      imported = [referenceBinder reference | prepared <- modules
        , references <- Map.elems (preparedModuleReferenceFacts context prepared)
        , reference <- references]
        ++ [dataConWorkId member | prepared <- modules
           , (constructor, _) <- preparedConstructors
               (extractPreparedFacts (pmModule prepared) (map fst (pmBindings prepared)))
           , member <- tyConDataCons (dataConTyCon constructor)]
      packages = [preparedRootIdentity binder | binder <- imported
        , Just owner <- [nameModule_maybe (varName binder)]
        , not (isHomeUnit (hsc_home_unit env) (moduleUnit owner))]
      retained = context { projectionRetainedGenerations = Map.fromList [(identity,0) | identity <- packages] }
      executable = preparedModuleProductOutcomes (projectPreparedModuleProducts retained modules)
      sourceProducts = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env (pprProductInterfaces original) retained modules)
      coldProducts = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env (pprProductInterfaces original) context modules)
      globals outcomes = [global | (_,Right groups) <- outcomes, group <- groups
        , global <- projectedGlobals (projectedBody group)]
      packageSet = Set.fromList packages
      normalize group = group {projectedBody = (projectedBody group)
        {projectedGlobals = [if globalIdentity global `Set.member` packageSet
            then global {globalRequiredGeneration=Nothing} else global
          | global <- projectedGlobals (projectedBody group)]}}
      normalized = map (\(owner,groups) -> (owner,fmap (map normalize) groups)) executable
  verifyCertifiedGroupingOrder work original
  unless (not (null packages) && any (isJust . globalRequiredGeneration) (globals executable)
      && all (isNothing . globalRequiredGeneration) (globals sourceProducts)
      && [owner | (owner,_) <- executable] == [owner | (owner,_) <- sourceProducts]) $
    fail "original package projection changed home ownership or kept live package generations"
  unless (normalized == sourceProducts && all (isNothing . globalRequiredGeneration) (globals coldProducts)) $
    fail "original purpose changed native body, ordinals or reference shape beyond package requirement issuance"
  let packageGlobals = filter ((`Set.member` packageSet) . globalIdentity) (globals executable)
  unless (any (isJust . globalEntrySignature) packageGlobals
      && any (\global -> symbolOccurrence (globalIdentity global) == "Nothing"
        && isNothing (globalEntrySignature global) && globalRequiredEvaluated global) packageGlobals
      && any (not . globalRequiredEvaluated) packageGlobals) $
    fail "actual package fixture lacks function, retained nullary constructor or unevaluated CAF"
  forM_ [0,7] $ \generation -> do
    let home = SymbolIdentity "main" "PackageOriginalHome" "value" "homeValue" Nothing
        value = SymbolIdentity "main" "Tidepool.Session.Val.G7" "value" "liveValue" Nothing
        live = retained {projectionRetainedGenerations=Map.union (Map.fromList [(home,generation),(value,generation)])
          (projectionRetainedGenerations retained)}
        homeProducts = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env (pprProductInterfaces original) live modules)
    forM_ [home,value] $ \identity ->
      unless (any (\global -> globalIdentity global == identity && globalRequiredGeneration global == Just generation)
          (globals homeProducts)) $
        fail "original product weakened a live home/value generation requirement"
  let supportName = mkModuleName "PackageOriginalSupport"
      paired = pprProductInterfaces original
  supportInterface <- maybe (fail "source fixture lost its paired native interface") pure (Map.lookup supportName paired)
  let
      supportOwner = mkModule (moduleUnit (mi_module supportInterface)) supportName
      fallback map' = lookup supportOwner
        (preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env map' retained modules))
  unless (fallback (Map.delete supportName paired) == lookup supportOwner executable
      && fallback (Map.insert supportName
        (set_mi_module (mkModule (stringToUnit "wrong-home-unit") supportName) supportInterface) paired)
          == lookup supportOwner executable) $
    fail "missing or wrong-unit native interface granted original product purpose"
  let incomplete = [prepared {pmCoverage=ExactBodySubset} | prepared <- modules]
      conservative = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env paired retained incomplete)
  unless (any (isJust . globalRequiredGeneration) (globals conservative)) $
    fail "incomplete prepared coverage acquired generation-free original package requirements"
  target <- either (fail . show) pure (projectPrepared retained modules)
  coldCertificate <- certifyProjectedProducts work "cold" original coldProducts [] env >>= either fail pure
  originalOnly <- certifyProjectedProducts work "original-only" original sourceProducts [] env >>= either fail pure
  mixed <- certifyProjectedProducts work "mixed" original sourceProducts [("target",target)] env >>= either fail pure
  let packageOwner = \case TList (TString "package":_) -> True; _ -> False
      retainedOwner = \case TList (TString "retained-package":_) -> True; _ -> False
  coldOwners <- certificateOwners coldCertificate
  sourceOwners <- certificateOwners originalOnly
  mixedOwners <- certificateOwners mixed
  unless (any packageOwner coldOwners && any packageOwner sourceOwners
      && not (any retainedOwner sourceOwners) && any retainedOwner mixedOwners && any packageOwner mixedOwners) $
    fail "canonical certification did not preserve original Package and executable RetainedPackage independently"
  verifyOriginalOnlyPackageRefusal work original sourceProducts originalOnly
  bad <- case packageGlobals of
    value : _ -> pure value
    [] -> fail "source fixture lacks a package global"
  let
      corrupt global | globalIdentity global == globalIdentity bad =
            global {globalIdentity=(globalIdentity global) {symbolOccurrence="$absentOriginalPackageGlobal"}}
          | otherwise = global
      invalid = [(owner,fmap (map (\group -> group {projectedBody=(projectedBody group)
            {projectedGlobals=map corrupt (projectedGlobals (projectedBody group))}})) groups)
        | (owner,groups) <- sourceProducts]
  certifyProjectedProducts work "bad-symbol" original invalid [] env >>= \case
    Left _ -> pure ()
    Right _ -> fail "original-only source global sealed a noncanonical package symbol"
  putStrLn "original package projection: functions/constructors/CAF, unchanged native shapes, home0/7, positive original+target witnesses and missing package refusal passed"

originalPackageCohort :: FilePath -> FilePath -> IO ()
originalPackageCohort coreRoot output = do
  createDirectoryIfMissing True output
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (coreRoot </> "Tidepool/Effects/Core.hs") [coreRoot,"lib"] Nothing
  let env = prHscEnv (pprPipelineResult original)
      modules = pprModules original
  formatting <- resolveFormattingAuthority env
  time <- resolveTimeAuthority env
  json <- resolveJsonAuthority env
  text <- resolveTextPackageUnit env
  let context = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        (SymbolIdentity "main" "Tidepool.Effects.Core" "value" "__result" Nothing)
        [] formatting time json text
      imported = [referenceBinder reference | prepared <- modules
        , references <- Map.elems (preparedModuleReferenceFacts context prepared)
        , reference <- references]
        ++ [dataConWorkId member | prepared <- modules
           , (constructor, _) <- preparedConstructors
               (extractPreparedFacts (pmModule prepared) (map fst (pmBindings prepared)))
           , member <- tyConDataCons (dataConTyCon constructor)]
      packages = Set.fromList [preparedRootIdentity binder | binder <- imported
        , Just owner <- [nameModule_maybe (varName binder)]
        , not (isHomeUnit (hsc_home_unit env) (moduleUnit owner))]
      retained = context {projectionRetainedGenerations=Map.fromSet (const 0) packages}
      executable = preparedModuleProductOutcomes (projectPreparedModuleProducts retained modules)
      products = preparedModuleProductOutcomes
        (projectOriginalHomeModuleProducts env (pprProductInterfaces original) retained modules)
      globals groups = [global | group <- groups, global <- projectedGlobals (projectedBody group)]
      names = ["Tidepool.Data.Time","Tidepool.FilePath","Tidepool.QQ.Fmt.Runtime","Tidepool.Prelude","Tidepool.Effects.Core"]
  forM_ names $ \name -> do
    (owner,groups) <- case [(owner,groups) | (owner,Right groups) <- products
      , moduleNameString (moduleName owner) == name] of
      [pair] -> pure pair
      _ -> fail ("actual cohort lost its paired original: " ++ name)
    old <- maybe (fail ("cohort lacks its executable outcome: " ++ name))
      (either (fail . show) pure) (lookup owner executable)
    unless (any (isJust . globalRequiredGeneration) (globals old)
        && all (isNothing . globalRequiredGeneration) (globals groups)) $
      fail ("cohort did not separate original package requirements: " ++ name)
    putStrLn (name ++ " groups=" ++ show (length groups)
      ++ " executable_live_globals=" ++ show (length (filter (isJust . globalRequiredGeneration) (globals old))))
  let eligible = Set.fromList [(unitString (moduleUnit owner),moduleNameString (moduleName owner))
        | (owner,Right groups) <- products, all (isNothing . globalRequiredGeneration) (globals groups)]
      evidence = pprDependencies original
  unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence) $
    fail "actual original source cohort lacks complete tracked evidence"
  forM_ names $ \name -> do
    required <- either fail pure (selectedHomeRequirements evidence "main" name)
    unless (all (`Set.member` eligible) required) $
      fail ("actual original cohort is not closed: " ++ name ++ " requires " ++ show required)
  certified <- certifyProjectedProducts output "cohort" original products [] env >>= either fail pure
  BS.writeFile (output </> "certified-products.cbor") certified
  fresh <- forM products $ \(owner,projected) -> do
    groups <- either (fail . show) pure projected
    let name = moduleNameString (moduleName owner)
        path = output </> ("cohort-" ++ name ++ ".hi")
    ifaceBytes <- BS.readFile path
    roots <- maybe (fail ("actual cohort lacks direct package roots: " ++ name)) pure
      (Map.lookup (moduleName owner) (pprPackageImports original))
    BS.writeFile (path ++ ".packages")
      (encodePackageImports (ExactIfaceArtifact (unitString (moduleUnit owner)) name path (digest ifaceBytes) []) roots)
    pure (T.pack (unitString (moduleUnit owner)),T.pack name,ifaceBytes,groups)
  BS.writeFile (output </> "module-products.cbor") (encodeModuleProducts fresh)
  writeFile (output </> "dependency-evidence.json") (renderDependencyEvidence evidence)
  writeFile (output </> "original-eligibility.txt") (unlines (map show (Set.toAscList eligible)))
  putStrLn "actual original cohort: Time/FilePath/Fmt.Runtime and Prelude/Core closed with generation-free package requirements and canonical package certificates"

certifyProjectedProducts :: FilePath -> String -> PreparedPipelineResult
  -> [(Module, Either ProjectionError [ProjectedGroup])]
  -> [(String,WireProgram)] -> HscEnv -> IO (Either String BS.ByteString)
certifyProjectedProducts work label original outcomes targets env = do
  fresh <- forM outcomes $ \(owner,projected) -> do
    groups <- either (fail . show) pure projected
    let name = moduleNameString (moduleName owner)
        path = work </> (label ++ "-" ++ name ++ ".hi")
    iface <- maybe (fail "source product lost its paired actual interface") pure
      (Map.lookup (moduleName owner) (pprProductInterfaces original))
    writeBinIface (targetProfile (hsc_dflags env)) QuietBinIFace NormalCompression path iface
    bytes <- BS.readFile path
    pure (T.pack (unitString (moduleUnit owner)),T.pack name,bytes,groups)
  let ready = Set.fromList [(T.unpack unit,T.unpack name) | (unit,name,_,_) <- fresh]
      evidence = (pprDependencies original) {dependencyModules =
        [if (dependencyModuleUnit node,dependencyModuleName node) `Set.member` ready
            then node {dependencyModuleProduct=ProductReady} else node
        | node <- dependencyModules (pprDependencies original)]}
      bytes = encodeModuleProducts fresh
  originals <- newOriginalInterfaceArtifacts env (pprFinalizedModules original) [] work
  finalized <- captureFinalizedModuleArtifacts originals env (pprFinalizedModules original)
    (pprPackageImports original) evidence work
  encodeCertifiedProducts env OrdinaryExecutionSource (pprProductInterfaces original) finalized [] Nothing fresh targets evidence bytes
    (BSC.pack (renderDependencyEvidence evidence))

certificateOwners :: BS.ByteString -> IO [Term]
certificateOwners bytes = do
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
  case term of
    TList [TString "TPCERT", TInt 8, _, _, _, TList rows, _, TList [TString "ordinary"], TList coordinates] ->
      forM rows $ \case
        TList [identity,_,_,_,TList [TString "source", TInt index, ordinal]] -> do
          coordinate <- coordinateAt coordinates index
          case coordinate of
            TList [TString "source",unit,name,version] -> pure
              (TList [TString "source",unit,name,version,ordinal,identity])
            _ -> fail "source witness used a package coordinate"
        TList [identity,_,_,_,TList [TString "retained", generation]] -> pure
          (TList [TString "retained",identity,generation])
        TList [identity,_,_,_,TList [TString "package", TInt index]] -> do
          coordinate <- coordinateAt coordinates index
          case coordinate of
            TList [TString "package",unit,name,sha] -> pure
              (TList [TString "package",unit,name,sha,identity])
            _ -> fail "package witness used a source coordinate"
        TList [identity,_,_,_,TList [TString "retained-package", TInt index,generation]] -> do
          coordinate <- coordinateAt coordinates index
          case coordinate of
            TList [TString "package",unit,name,sha] -> pure
              (TList [TString "retained-package",unit,name,sha,identity,generation])
            _ -> fail "retained package witness used a source coordinate"
        _ -> fail "original certificate global lacks its exact owner"
    _ -> fail "original product lacks its canonical ownership certificate"
  where
    coordinateAt coordinates index
      | index >= 0 && index < length coordinates = pure (coordinates !! index)
      | otherwise = fail "original certificate owner coordinate out of range"

verifyOriginalOnlyPackageRefusal :: FilePath -> PreparedPipelineResult
  -> [(Module, Either ProjectionError [ProjectedGroup])] -> BS.ByteString -> IO ()
verifyOriginalOnlyPackageRefusal work original outcomes certified = do
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict certified))
  (unit,name,path) <- case term of
    TList [TString "TPCERT",TInt 8,_,TList [],TList (TList [TString unit,TString name,TString path,_]:_),_,_,_,_] ->
      pure (T.unpack unit,T.unpack name,T.unpack path)
    _ -> fail "original-only package requirement did not issue its own positive witness"
  let env = prHscEnv (pprPipelineResult original)
      owner = mkModule (stringToUnit unit) (mkModuleName name)
      missing = work </> "missing-original-package.hi"
  (_,location) <- readExactInterface env owner >>= either (fail . show) pure
  finder <- initFinderCache
  addModuleToFinder finder (GWIB owner NotBoot) location {ml_hi_file=missing}
  certifyProjectedProducts work "missing" original outcomes [] env {hsc_FC=finder} >>= \case
    Left _ -> pure ()
    Right _ -> fail "original-only package demand sealed without its defining interface"
  wrong <- maybe (fail "original package fixture has no home interface for owner refusal") pure
    (Map.lookup (mkModuleName "PackageOriginalSupport") (pprProductInterfaces original))
  writeBinIface (targetProfile (hsc_dflags env)) QuietBinIFace NormalCompression missing wrong
  certifyProjectedProducts work "wrong-owner" original outcomes [] env {hsc_FC=finder} >>= \case
    Left _ -> pure ()
    Right _ -> fail "original-only package demand sealed another actual interface owner"
  BS.writeFile missing =<< BS.readFile path
  restored <- certifyProjectedProducts work "restored" original outcomes [] env {hsc_FC=finder}
  either fail (const (pure ())) restored

verifyCertifiedGroupingOrder :: FilePath -> PreparedPipelineResult -> IO ()
verifyCertifiedGroupingOrder work original = do
  let env = prHscEnv (pprPipelineResult original)
      emptyEvidence = DependencyEvidence False False [] [] [] []
      name = "PackageOriginalHome"
      path = work </> "certified-group-order.hi"
      source = work </> name ++ ".hs"
  iface <- maybe (fail "group ordering fixture lost its actual original interface") pure
    (Map.lookup (mkModuleName name) (pprProductInterfaces original))
  writeBinIface (targetProfile (hsc_dflags env)) QuietBinIFace NormalCompression path iface
  interfaceBytes <- BS.readFile path
  sourceBytes <- BS.readFile source
  base <- structuralCandidate work
  let candidate = base
        { candidateUnit = unitString (moduleUnit (mi_module iface))
        , candidateModule = name
        , candidateSource = source
        , candidateSourceSha256 = digest sourceBytes
        , candidateInterface = path
        , candidateInterfaceSha256 = digest interfaceBytes
        , candidateModuleVersion = replicate 64 '1'
        , candidateProductSha256 = replicate 64 '2'
        , candidateEvidenceSha256 = replicate 64 '3'
        , candidateImports = []
        , candidateGroups = [CandidateGroup 91 [] [], CandidateGroup 3 [] []]
        , candidatePackageImports = path ++ ".packages"
        , candidatePackageImportsSha256 = replicate 64 '4'
        , candidateProductPath = path ++ ".product.cbor"
        , candidateExecutionSource = Nothing
        }
  bytes <- encodeCertifiedProducts env OrdinaryExecutionSource (pprProductInterfaces original) (emptyFinalizedModuleArtifacts env) [candidate] Nothing
    [] [] emptyEvidence BS.empty BS.empty >>= either fail pure
  term <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
  case term of
    TList [TString "TPCERT", TInt 8, TList [TList fields], TList [], TList [], TList [], _, TList [TString "ordinary"], _]
      | length fields == 10 -> case drop 8 fields of
          [TList groups,TList []] -> unless (map ordinal groups == [91,3]) $
            fail "certified module grouping changed nonmonotone encounter order"
          _ -> fail "certified module grouping omitted its exact group or interface inventory"
    _ -> fail "certified module grouping produced an unexpected certificate envelope"
  let absent = candidate {candidateModule="MissingOriginalInterface"}
  encodeCertifiedProducts env OrdinaryExecutionSource Map.empty (emptyFinalizedModuleArtifacts env) [absent] Nothing [] [] emptyEvidence BS.empty BS.empty >>= \case
    Left _ -> pure ()
    Right _ -> fail "cached original without its exact interface acquired a certificate"
  where
    ordinal (TList [TInt value,TList []]) = value
    ordinal _ = -1

-- Synthetic descriptors exercise producer group framing only. This decoder
-- does not grant them a durable certificate or compiler admission.
structuralCandidate :: FilePath -> IO ModuleCandidate
structuralCandidate work = do
  let path = work </> "structural-candidate.cbor"
      seal = TString (T.replicate 64 "a")
      row = TList [TString "main",TString "Fixture",TString "/fixture/Source.hs",seal
        ,TString "/fixture/Source.hi",seal,seal,seal,seal,TList [],TList []
        ,TString "/fixture/packages",seal,TString "/fixture/products.tpmod",TList []
        ,TList [TString "module",TString "/fixture/module.cbor",seal,TString "/fixture/Core",seal]]
      packet = TList [TString "TPMCAN",TString "10",TList [],TList [],TList [row]
        ,TList [TList [],TList []],seal]
  BS.writeFile path (toStrictByteString (encodeTerm packet))
  readModuleCandidates path >>= \case
    Right [candidate] -> pure candidate
    _ -> fail "structural producer-group candidate did not decode"

originalProjectionProducts :: IO ()
originalProjectionProducts = withScratch $ \work -> do
  let identity :: String -> String -> String -> SymbolIdentity
      identity unit moduleName' occurrence =
        SymbolIdentity (T.pack unit) (T.pack moduleName') (T.pack "value") (T.pack occurrence) Nothing
      owners :: Map.Map SymbolIdentity (String, Word32)
      dependencies :: Map.Map (String, Word32) (Set.Set SymbolIdentity)
      unavailable = identity "main" "A" "unavailable"
      middle = identity "main" "B" "middle"
      terminal = identity "main" "C" "terminal"
      unrelated = identity "main" "D" "unrelated"
      cycleA = identity "main" "E" "cycleA"
      cycleB = identity "main" "F" "cycleB"
      owners = Map.fromList
        [(unavailable, ("A", 0)), (middle, ("B", 3)),
         (terminal, ("C", 7)), (unrelated, ("D", 2)),
         (cycleA, ("E", 1)), (cycleB, ("F", 4))]
      dependencies = Map.fromList
        [ (("B", 3), Set.singleton unavailable)
        , (("C", 7), Set.singleton middle)
        , (("E", 1), Set.singleton cycleB)
        , (("F", 4), Set.singleton cycleA)
        , (("D", 2), Set.empty) ]
      blocked = closeUnavailableOriginalGroups dependencies owners
        (Set.fromList [unavailable, cycleA])
  unless (blocked == Set.fromList [("A", 0), ("B", 3), ("C", 7), ("E", 1), ("F", 4)]) $
    fail "original product closure did not handle cross-module chains, cycles and unrelated groups"
  copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "MetadataQuoteSupport.hs") [work] Nothing
  prepared <- case [value | value <- pprModules original
      , moduleNameString (moduleName (pmModule value)) == "MetadataQuoteSupport"] of
    [value] -> pure value
    _ -> fail "projection fixture lacks its actual GHC source product"
  binder <- case [binder | (binding, _) <- pmBindings prepared, binder <- topBinders binding] of
    value : _ -> pure value
    [] -> fail "projection fixture lacks an original binder"
  let context = ProjectionContext "test" "matched"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        (SymbolIdentity "main" "MetadataQuoteSupport" "value" "answerValue" Nothing)
        [] Nothing Nothing Nothing Nothing
      rejected = prepared { pmSiteRejections = [SiteRejection binder "projection fixture refusal"] }
      originals = [prepared, rejected]
      products = projectPreparedModuleProducts context originals
      expected = [(pmModule value, projectPreparedModuleGroups context value) | value <- originals]
      encode outcomes = [encodeModuleProducts [(T.pack (unitString (moduleUnit owner)),
            T.pack (moduleNameString (moduleName owner)), BS.empty, groups)]
          | (owner, Right groups) <- outcomes]
      actual = preparedModuleProductOutcomes products
  unless (actual == expected && encode actual == encode expected
      && encode (preparedModuleProductOutcomes products) == encode expected
      && any (\case (_, Left (RejectedTypedSite "projection fixture refusal")) -> True; _ -> False) actual) $
    fail "shared original projection changed module bytes, ordinals or typed refusal"
  putStrLn "original projection products: actual source bytes/ordinals and retained typed refusal passed"

candidateManifestProducts :: FilePath -> IO ()
candidateManifestProducts path = withScratch $ \work -> do
  candidates <- readModuleCandidates path >>= either fail pure
  case [candidate | candidate <- candidates, candidateModule candidate == "Tidepool.Effects.Core"] of
    [candidate] | length (candidateGroups candidate) == 6037 -> pure ()
    _ -> fail "production candidate manifest lost the actual 6037-group Core product"
  bytes <- BS.readFile path
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
  (magic, version, symbols, globals, fields, execution) <- case term of
    TList [magic, version, symbols, globals, TList [TList fields], execution]
      | length fields == 14 -> pure (magic, version, symbols, globals, fields, execution)
    _ -> fail "actual production emitter lost the single candidate row"
  let changedAt index value = TList [magic, version, symbols, globals, TList
        [TList (take index fields ++ [value] ++ drop (index + 1) fields)], execution]
      tooMany = TList (replicate 65537 (TList [TInt 0, TList [], TList []]))
  forM_ [("groups", changedAt 10 tooMany), ("digest", changedAt 5 (TString "invalid"))] $
    \(name, changed) -> do
      let altered = work </> (name ++ ".cbor")
      BS.writeFile altered (toStrictByteString (encodeTerm changed))
      refused <- readModuleCandidates altered
      unless (case refused of Left _ -> True; Right _ -> False) $
        fail ("candidate reader accepted invalid " ++ name)
  let trailing = work </> "trailing.cbor"
      oversized = work </> "oversized.cbor"
  BS.writeFile trailing (bytes <> BS.singleton 0)
  BS.writeFile oversized (BS.replicate (4 * 1024 * 1024 + 1) 0)
  forM_ [trailing, oversized] $ \altered -> do
    refused <- readModuleCandidates altered
    unless (case refused of Left _ -> True; Right _ -> False) $
      fail "candidate reader accepted trailing or oversized bytes"
  putStrLn "candidate manifest products: actual Rust Core6037, group/digest/trailing/byte refusal bounds passed"

candidateSitedSiblings :: IO ()
candidateSitedSiblings = withScratch candidateSitedSiblingsAt

candidateSitedSiblingsAt :: FilePath -> IO ()
candidateSitedSiblingsAt work = do
  let unfoldDir = work </> "Tidepool" </> "Actors"
      replyDir = work </> "Tidepool" </> "Agent" </> "Reply"
      owner = unfoldDir </> "Unfold.hs"
      target = work </> "HydratedChildTarget.hs"
      scopePath = work </> "exact-scope.cbor"
      capturedPath = work </> "captured-scope.cbor"
      scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath }
      owners = ["Tidepool.Agent.Reply.Internal", "Tidepool.Actors.Unfold"]
  createDirectoryIfMissing True unfoldDir
  createDirectoryIfMissing True replyDir
  copyFile "test-source-boot/fixtures/HydratedChildOwner.hs" owner
  copyFile "test-source-boot/fixtures/HydratedReplyOwner.hs" (replyDir </> "Internal.hs")
  copyFile "test-source-boot/fixtures/HydratedChildTarget.hs" target
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing owner [work] Nothing
  originals <- newOriginalInterfaceArtifacts (prHscEnv (pprPipelineResult original))
    (pprFinalizedModules original) [] work
  writeGenuineCandidateLexicalScope owners work owner [work] capturedPath original
  writeGenuineEmptyMetadataScope scopePath
  -- A fresh compiler admits the source/interface candidates without preparing
  -- their bodies. This must not accidentally rely on a previous worker memo.
  reused <- runPipelineSessionSelected (PreparedProducts (Just (manifest work)))
    Set.empty GeneralCompile (Just scope) target [work] Nothing
  unless (Set.fromList (map candidateModule (pprAcceptedCandidates reused)) == Set.fromList owners
      && preparedNames reused == ["HydratedChildTarget"]) $
    fail "typed sibling regression did not exercise the hydrated candidate path"
  forM_ (pprAcceptedCandidates reused) $ \candidate -> do
    bytes <- BS.readFile (candidateInterface candidate)
    captured <- originalInterfaceBytes originals
      (mkModule (stringToUnit (candidateUnit candidate)) (mkModuleName (candidateModule candidate)))
      >>= maybe (fail "typed sibling candidate lacks its captured original interface") pure
    unless (candidateUnit candidate == "main"
        && bytes == captured && candidateInterfaceSha256 candidate == digest captured) $
      fail "typed sibling candidate changed its exact original interface custody"
  case pprModules reused of
    [prepared] | null (pmSiteRejections prepared), length (pmYieldSites prepared) == 1 -> pure ()
    _ -> fail "hydrated child surface lost its exact typed sibling or site identity"
  retainedScope <- readExactScope capturedPath >>= either fail pure
  unless (Set.fromList (map (snd . fst) (scopeLexical retainedScope)) == Set.fromList owners
      && null (scopeProducts retainedScope) && null (scopeExecutionOwners retainedScope)) $
    fail "typed sibling metadata closure lost source authority or acquired native execution"
  removeFile owner
  removeFile (replyDir </> "Internal.hs")
  captured <- runPipelineSessionSelected (PreparedProducts Nothing)
    Set.empty GeneralCompile (Just (scope {ssExactScope=Just capturedPath})) target [work] Nothing
  unless (null (pprAcceptedCandidates captured)
      && preparedNames captured == ["HydratedChildTarget"]
      && all ((`notElem` owners) . dependencyModuleName) (dependencyModules (pprDependencies captured))) $
    fail "typed sibling regression did not exclude captured originals from downsweep"
  case pprModules captured of
    [prepared] | null (pmSiteRejections prepared), length (pmYieldSites prepared) == 1 -> pure ()
    _ -> fail "source-free exact child surface lost its typed sibling or site identity"
  unless (map pmYieldSites (pprModules captured) == map pmYieldSites (pprModules reused)) $
    fail "source-free sibling hydration changed the original typed suspension site"
  putStrLn "candidate typed siblings: certified candidates and source-free exact owners retain childSited and typed sites without dependency recompilation"

-- The fixture encoder keys complete legacy values by their canonical CBOR.
-- Its tables therefore preserve identity fields and every global requirement.
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

candidateCompactInventory :: IO ()
candidateCompactInventory = withScratch $ \work -> do
  let identity = SymbolIdentity "main" "Fixture" "value" "entry" Nothing
      identities = [identity,identity {symbolRecordParent=Just "Parent"}
        ,identity {symbolUnit="other"},identity {symbolModule="Other"}
        ,identity {symbolNamespace="data"},identity {symbolOccurrence="other"}]
      plain = CandidateGlobal identity LiftedRefRep Nothing False Nothing
      globals = [plain,plain {candidateGlobalRep=IntRep 64}
        ,plain {candidateGlobalSignature=Just (Signature [LiftedRefRep] (Returns [IntRep 64]))}
        ,plain {candidateGlobalSignature=Just (Signature [AddressRep] (Returns [IntRep 64]))}
        ,plain {candidateGlobalSignature=Just (Signature [LiftedRefRep] (Returns [WordRep 64]))}
        ,plain {candidateGlobalSignature=Just (Signature [LiftedRefRep] NoSuccess)}
        ,plain {candidateGlobalSignature=Just (Signature [LiftedRefRep] CallerResult)}
        ,plain {candidateGlobalEvaluated=True},plain {candidateGlobalGeneration=Just 0}
        ,plain {candidateGlobalGeneration=Just 7}]
      groups = [CandidateGroup 91 identities globals,CandidateGroup 3 [identity] (reverse globals)]
      legacyRows = [fixtureCandidate "Fixture" (map groupTerm groups)
        ,fixtureCandidate "Other" (map groupTerm (reverse groups))]
      emptyParcel = TList [TList [],TList []]
      envelope symbols globalTable rows = TList
        [TString "TPMCAN",TString "10",symbols,globalTable,TList rows,emptyParcel
        ,TString (T.replicate 64 "a")]
      readFixture name value = do
        let path = work </> (name ++ ".cbor")
            bytes = toStrictByteString (encodeTerm value)
        unless (BS.length bytes <= 4 * 1024 * 1024) (fail "decoder fixture exceeds wire bound")
        BS.writeFile path bytes
        readModuleCandidates path
      refuse name expected value = readFixture name value >>= \case
        Left reason | expected `isInfixOf` reason -> pure ()
                    | otherwise -> fail (name ++ " failed at the wrong bound: " ++ reason)
        Right _ -> fail ("candidate compact decoder accepted " ++ name)
  (symbols,globalTable,rows) <- either fail pure (compactInventoryRows legacyRows)
  case (symbols,globalTable) of
    (TList symbolRows,TList globalRows) | length symbolRows == length identities
      && length globalRows == length globals -> pure ()
    _ -> fail "fixture encoder merged complete identities or global requirements"
  decoded <- readFixture "exact" (envelope symbols globalTable rows) >>= either fail pure
  unless (map candidateGroups decoded == [groups,reverse groups]) $
    fail "compact inventory changed exact legacy values, order or ordinal"
  let badGroup binders globalRefs = [fixtureCandidate "Fixture" [TList [TInt 91,TList binders,TList globalRefs]]]
  refuse "unavailable-symbol" "unavailable" (envelope symbols globalTable (badGroup [TInt 65535] []))
  refuse "unavailable-global" "unavailable" (envelope symbols globalTable (badGroup [] [TInt 65535]))
  forM_ [("out-of-range",TInt 65536),("negative",TInt (-1))
      ,("u64-max",TInteger (2 ^ (64 :: Int) - 1)),("beyond-u64",TInteger (2 ^ (64 :: Int)))] $ \(label,index) ->
    forM_ [("symbol",badGroup [index] []),("global",badGroup [] [index])] $ \(kind,invalidRows) ->
      readFixture (kind ++ "-" ++ label) (envelope symbols globalTable invalidRows) >>= \case
        Left _ -> pure ()
        Right _ -> fail ("candidate compact decoder accepted " ++ kind ++ " " ++ label ++ " index")
  let danglingGlobals = TList [TList [TInt 65535,TList [TString "lifted",TInt 0],TNull,TBool False,TNull]]
  refuse "dangling-global-symbol" "unavailable" (envelope symbols danglingGlobals rows)
  refuse "duplicate-owner" "duplicate module candidate"
    (envelope symbols globalTable (take 1 rows ++ take 1 rows))
  refuse "oversized-symbol-table" "table exceeds" (envelope (TList (replicate 65537 TNull)) globalTable rows)
  refuse "oversized-global-table" "table exceeds" (envelope symbols (TList (replicate 65537 TNull)) rows)
  let largeIdentity = identity {symbolOccurrence=T.replicate 2048 "x"}
      expandedGroup = TList [TInt 0,TList (replicate 1536 (TInt 0)),TList []]
      largeSymbols = TList [symbolTerm largeIdentity]
      oneLarge = [fixtureCandidate "Fixture" [expandedGroup]]
  readFixture "expanded-within-bound" (envelope largeSymbols (TList []) oneLarge) >>= either fail (const (pure ()))
  refuse "expanded-aggregate" "expanded candidate inventory exceeds"
    (envelope largeSymbols (TList []) (oneLarge ++ [fixtureCandidate "Other" [expandedGroup]]))
  refuse "unsupported6" "unsupported" (TList [TString "TPMCAN",TString "6",TList legacyRows])
  refuse "unsupported7" "unsupported" (TList [TString "TPMCAN",TString "7",TList legacyRows,emptyParcel])
  putStrLn "candidate compact inventory: exact legacy values/order/ordinals, complete interning, unavailable/out-of-range indices, dangling globals, duplicate owners, table/expanded bounds and unsupported6/7 passed"
  where
    fixtureCandidate name groups = TList
      ([TString "main",TString name,TString "/fixture/source.hs",sha,TString "/fixture/interface.hi",sha,sha,sha,sha]
        ++ [TList [],TList groups,TString "/fixture/packages",sha,TString "/fixture/products.tpmod"
          ,TList [],TList [TString "module",TString "/fixture/module.cbor",proofSeal
            ,TString "/fixture/Core",proofSeal]])
      where
        sha = TString (T.replicate 64 "0")
        proofSeal = TString (T.replicate 64 "a")
    groupTerm group = TList [TInt (fromIntegral (candidateGroupOrdinal group))
      ,TList (map symbolTerm (candidateGroupBinders group)),TList (map globalTerm (candidateGroupGlobals group))]
    globalTerm global = TList [symbolTerm (candidateGlobalIdentity global),repTerm (candidateGlobalRep global)
      ,maybe TNull signatureTerm (candidateGlobalSignature global),TBool (candidateGlobalEvaluated global)
      ,maybe TNull (TInt . fromIntegral) (candidateGlobalGeneration global)]
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

candidateGhcLoad :: IO ()
candidateGhcLoad = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs", "MetadataQuoter.hs", "MetadataQuotedTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  let helper = mkModuleName "MetadataQuoteSupport"
      scopePath = work </> "exact-scope.cbor"
      scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath }
      restore = copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
      changed = copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
  writeGenuineEmptyMetadataScope scopePath
  original <- runPipelineSelected (PreparedProducts Nothing) (work </> "MetadataQuoter.hs") [work]
  writeGenuineCandidateManifestFor ["MetadataQuoteSupport"] work
    (work </> "MetadataQuoter.hs") [work] original
  bracket (lookupEnv "TIDEPOOL_COMPILER_PRODUCER")
    (maybe (unsetEnv "TIDEPOOL_COMPILER_PRODUCER") (setEnv "TIDEPOOL_COMPILER_PRODUCER")) $ \configured -> do
      producer <- maybe (fail "genuine producer configuration disappeared") pure configured
      let altered = case producer of
            first : remaining -> (if first == 'f' then 'e' else 'f') : remaining
            [] -> error "genuine producer was validated before packet issuance"
      forM_ [(Nothing, "CompilerProducerUnavailable"), (Just altered, "CompilerProducerScopeMismatch")] $
        \(replacement, expected) -> do
          maybe (unsetEnv "TIDEPOOL_COMPILER_PRODUCER") (setEnv "TIDEPOOL_COMPILER_PRODUCER") replacement
          refused <- try (runPipelineSessionSelected CheckedEnvironment Set.empty GeneralCompile
            (Just scope) (work </> "MetadataQuotedTarget.hs") [work] Nothing)
            :: IO (Either SomeException CheckedEnvironmentResult)
          case refused of
            Left failure | expected `isInfixOf` show failure -> pure ()
            _ -> fail ("exact fixture trusted its scope without independent producer: " ++ expected)
  withResidentPipelineSelected [work] $ \compile ->
    forM_ [(42, restore), (43, changed), (42, restore)] $ \(expected, install) -> do
      install
      (reused, diagnostics) <- captureDiagnostics $
        compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile (Just scope)
          (work </> "MetadataQuotedTarget.hs") [work] Nothing
      let accepted = map candidateModule (pprAcceptedCandidates reused)
          required = if expected == 42 then 1 :: Int else 0
          fresh = preparedNames reused
          result = pprPipelineResult reused
      unless (accepted == (if expected == 42 then ["MetadataQuoteSupport"] else [])
          && ("MetadataQuoteSupport" `elem` fresh) == (expected /= 42)
          && counterValues "candidate_executable_required" diagnostics == [fromIntegral required]
          && hasIntResultLiteral expected (prBinds result)) $
        fail ("native candidate reuse skipped GHC execution or retained an old quoted helper body: "
          ++ show (expected, accepted, fresh) ++ "\n" ++ diagnostics ++ "\n"
          ++ showSDocUnsafe (ppr (prBinds result)))
      case lookupHpt (hsc_HPT (prHscEnv result)) helper of
        Just hmi | let linkable = hm_linkable hmi
                 , isJust (homeMod_bytecode linkable) || isJust (homeMod_object linkable) -> pure ()
        _ -> fail "native candidate retention discarded its actual GHC executable"
      putStrLn ("candidate GHC load evidence: expected=" ++ show expected
        ++ " accepted=" ++ show accepted ++ " fresh=" ++ show fresh
        ++ " executable_demand=" ++ show (counterValues "candidate_executable_required" diagnostics)
        ++ " helper_frontends=" ++ show (length (filter (==
          "tidepool-canonical-frontend module=MetadataQuoteSupport") (lines diagnostics))))
  putStrLn "candidate GHC load: original native reuse, actual quoter execution and source A/B/A passed"

-- Pure issuer and scope-budget controls; the runtime suite separately drives
-- cold parser -> whole checked program -> per-item original certification.
nativeCheckedSignaturesTest :: IO ()
nativeCheckedSignaturesTest = withScratch $ \work -> do
  let target = work </> "CheckedNativeSignatures.hs"
  forM_ ["CheckedNativeTypeOwner.hs", "CheckedNativeSignatures.hs"] $ \file ->
    copyFile ("test-source-boot/fixtures" </> file) (work </> file)
  libdir <- getLibdir
  withResidentPipelineSelected [work] $ \compile -> do
    checked <- compile CheckedEnvironment Set.empty GeneralCompile Nothing target [work] Nothing
    let original = Map.fromList
          [(getOccString identifier, idType identifier)
          | identifier <- typeEnvIds (tcg_type_env (crTargetTcGblEnv checked))
          , "__tidepool_cell_pin_" `isPrefixOf` getOccString identifier]
    unless (Map.size original == 7) (fail "native signature fixture lost a case")
    signatures <- forM (Map.toList original) $ \(key, ty) -> do
      signature <- captureCheckedSignature (crHscEnv checked) key ty
      pure (key, signature { signaturePresentation = "not Haskell syntax !!!" })
    summary <- case [summary | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph (crHscEnv checked))
      , ms_mod_name summary == mkModuleName "CheckedNativeSignatures"] of
      [value] -> pure value
      _ -> fail "native signature target summary missing"
    runGhc (Just libdir) $ do
      setSession (crHscEnv checked)
      parsed <- parseModule summary
      rewritten <- liftIO (rewriteCheckedAnnotations (crHscEnv checked) signatures parsed)
      rechecked <- typecheckNativeModule rewritten
      let (environment, _) = tm_internals_ rechecked
          actual = Map.fromList [(getOccString identifier, idType identifier)
            | identifier <- typeEnvIds (tcg_type_env environment)]
      liftIO $ forM_ (Map.toList original) $ \(key, expected) ->
        unless (maybe False (eqType expected) (Map.lookup key actual))
          (fail ("native signature changed exact type: " ++ key))
      let damaged = [(key, signature { signatureNames = [] }) | (key, signature) <- signatures]
      refused <- liftIO (try (rewriteCheckedAnnotations (crHscEnv checked) damaged parsed)
        :: IO (Either SomeException NativeParsedModule))
      liftIO $ unless (case refused of Left _ -> True; Right _ -> False)
        (fail "native signature admitted a substituted Name inventory")
      let inputSignature = lookup "__tidepool_cell_pin_0_nominal" signatures
          inputType = Map.lookup "__tidepool_cell_pin_0_nominal" original
      signature <- maybe (fail "native nominal signature missing") pure inputSignature
      expected <- maybe (fail "native nominal type missing") pure inputType
      let parseSlot body = do
            liftIO (writeFile target ("module CheckedNativeSignatures where\n" ++ body))
            parseModule (summary { ms_hspp_buf = Nothing, ms_hspp_file = target })
      slot <- parseSlot "__result :: TidepoolActivationInput\n__result = undefined\nwarningProbe = ()\n"
      native <- liftIO (rewriteHostInputType (crHscEnv checked) 1 signature slot)
      let warningSource = nativeParsedModule native
          warningSummary = pm_mod_summary warningSource
          warningNative = native { nativeParsedModule = warningSource
            { pm_mod_summary = warningSummary { ms_hspp_opts = wopt_set
                (ms_hspp_opts warningSummary) Opt_WarnMissingSignatures } } }
      ((nativeEnvironment, _), nativeDiagnostics) <- liftIO
        (typecheckNativeModuleWithDiagnostics (crHscEnv checked) warningNative)
      liftIO $ unless (not (isEmptyMessages nativeDiagnostics))
        (fail "native typecheck hook dropped missing-signature diagnostics")
      let actualInput = [idType identifier | identifier <- typeEnvIds (tcg_type_env nativeEnvironment)
            , getOccString identifier == "__result"]
      liftIO $ unless (case actualInput of [ty] -> eqType expected ty; _ -> False)
        (fail "native host slot lost its nominal owner without a source import")
      nativeEnvironmentOwner <- getSession
      details <- liftIO (mkBootModDetailsTc (hsc_logger nativeEnvironmentOwner) nativeEnvironment)
      interface <- liftIO (mkIfaceTc nativeEnvironmentOwner Sf_None details summary Nothing nativeEnvironment)
      let ownerUsage UsageHomeModule { usg_mod_name = owner, usg_unit_id = unit } =
            moduleNameString owner == "CheckedNativeTypeOwner" && unitIdString unit == "main"
          ownerUsage _ = False
          sourceImports = Map.keys (imp_mods (tcg_imports nativeEnvironment))
      liftIO $ unless (any ownerUsage (mi_usages interface)
          && all ((/= "CheckedNativeTypeOwner") . moduleNameString . moduleName) sourceImports)
        (fail "native type owner lacks GHC usage evidence or acquired a source import")
      forM_ ["Int", "Missing.TidepoolActivationInput", "(TidepoolActivationInput, TidepoolActivationInput)"] $ \bad -> do
        malformed <- parseSlot ("__result :: " ++ bad ++ "\n__result = undefined\n")
        refusedSlot <- liftIO (try (rewriteHostInputType (crHscEnv checked) 1 signature malformed)
          :: IO (Either SomeException NativeParsedModule))
        liftIO $ unless (case refusedSlot of Left _ -> True; Right _ -> False)
          (fail "host input admitted a missing, qualified or duplicated native slot")
      -- A request carries native reply/progress leaves independently of their
      -- presentation. Neither nominal owner is introduced as a source import.
      reply <- liftIO (captureCheckedSignature (crHscEnv checked) "request-reply" expected)
      progressType <- maybe (fail "native progress fixture missing") pure
        (Map.lookup "__tidepool_cell_pin_6_tuple" original)
      progress <- liftIO (captureCheckedSignature (crHscEnv checked) "request-progress" progressType)
      let requestBody replySlot progressSlot = unlines
            ([ "sessionReply :: " ++ replySlot
             , "sessionReply = undefined"
             , "respond :: " ++ replySlot ++ " -> ()"
             , "respond _ = ()"
             ] ++ case progressSlot of
               Nothing -> []
               Just slotName -> ["reportProgress :: " ++ slotName ++ " -> ()", "reportProgress _ = ()"])
          presentationOnly nativeSignature = nativeSignature
            { signaturePresentation = "this is deliberately not a Haskell type" }
      forM_ [Nothing, Just progress] $ \maybeProgress -> do
        let contract = RequestTypeSignatures (presentationOnly reply) (presentationOnly <$> maybeProgress)
            progressSlot = "TidepoolRequestProgress" <$ maybeProgress
        requestParsed <- parseSlot (requestBody "TidepoolRequestReply" progressSlot)
        requestNative <- liftIO (rewriteRequestTypes (crHscEnv checked) ActorReplyHelpers contract requestParsed)
        requestChecked <- typecheckNativeModule requestNative
        let (requestEnvironment, _) = tm_internals_ requestChecked
            replyTypes = [idType identifier | identifier <- typeEnvIds (tcg_type_env requestEnvironment)
              , getOccString identifier == "sessionReply"]
        liftIO $ unless (case replyTypes of [ty] -> eqType expected ty; _ -> False)
          (fail "request annotation reconstructed a type from presentation")
      let plainContract = RequestTypeSignatures reply Nothing
      forM_ [plainContract, RequestTypeSignatures reply (Just progress)] $ \contract -> do
        noHelpers <- parseSlot "__result :: Int\n__result = 0\n"
        rewritten <- liftIO (rewriteRequestTypes (crHscEnv checked) NoRequestHelpers contract noHelpers)
        _ <- typecheckNativeModule rewritten
        composed <- liftIO (thenNativeModule native
          (rewriteRequestTypes (crHscEnv checked) NoRequestHelpers contract))
        liftIO $ unless (nameSetElemsStable (nativeTypeUses composed)
            == nameSetElemsStable (nativeTypeUses native))
          (fail "helper-free request transform dropped native input dependency uses")
        composedChecked <- typecheckNativeModule composed
        let (composedEnvironment, _) = tm_internals_ composedChecked
            composedTypes = [idType identifier | identifier <- typeEnvIds (tcg_type_env composedEnvironment)
              , getOccString identifier == "__result"]
        liftIO $ unless (case composedTypes of [ty] -> eqType expected ty; _ -> False)
          (fail "helper-free request transform lost the prior native input type")
        forM_ [requestBody "TidepoolRequestReply" Nothing
              ,"__result :: TidepoolRequestProgress\n__result = undefined\n"] $ \body -> do
          malformed <- parseSlot body
          refused <- liftIO (try (rewriteRequestTypes (crHscEnv checked) NoRequestHelpers contract malformed)
            :: IO (Either SomeException NativeParsedModule))
          liftIO $ unless (case refused of Left _ -> True; Right _ -> False)
            (fail "helper-free source recipe admitted a reserved request slot")
      let rejectedBodies =
            [ requestBody "Int" Nothing
            , requestBody "Missing.TidepoolRequestReply" Nothing
            , requestBody "(TidepoolRequestReply, TidepoolRequestReply)" Nothing
            , requestBody "TidepoolRequestReply" (Just "TidepoolRequestProgress")
            ]
      forM_ rejectedBodies $ \body -> do
        malformed <- parseSlot body
        refusedRequest <- liftIO (try (rewriteRequestTypes (crHscEnv checked) ActorReplyHelpers plainContract malformed)
          :: IO (Either SomeException NativeParsedModule))
        liftIO $ unless (case refusedRequest of Left _ -> True; Right _ -> False)
          (fail "request recipe admitted missing, qualified, duplicated or unauthorized native slots")
  putStrLn "native checked signatures: 7 shapes, source-free nominal slot and authority/slot refusals passed"

hostActivationPurposeTest :: Maybe FilePath -> IO ()
hostActivationPurposeTest destination = withScratch $ \work -> do
  let previewMarker = "{{ACTIVATION_PREVIEW}}"
      template = "preview = " ++ previewMarker ++ "\n__result = do {\n{{TURN_STMT}}\n}\n"
      literalInput = "sessionInput <- pure \"" ++ previewMarker ++ "\""
  replaced <- either fail pure (replaceTemplateMarker previewMarker "opaque" template)
  unless (literalInput `isInfixOf` spliceTemplate replaced literalInput "sessionInput"
      && case replaceTemplateMarker previewMarker "opaque" (template ++ previewMarker) of
        Left _ -> True; Right _ -> False) $
    fail "preview replacement rescanned input text or accepted duplicated protected markers"
  let sourcePath = work </> "HostActivationInput.hs"
      issuedPath = work </> "issued-host-scope.cbor"
  inputTemplate <- readFile "test-source-boot/fixtures/HostActivationInput.hs"
  originalSource <- either fail pure (replaceTemplateMarker "{{CHECKED_TYPE}}" "Int" inputTemplate)
  writeFile sourcePath originalSource
  original <- runPipelineSessionSelected CheckedEnvironment Set.empty GeneralCompile Nothing sourcePath [work] Nothing
  inputType <- maybe (fail "host input fixture has no inferred type") pure (crResultType original)
  capturedSignature <- captureCheckedSignature (crHscEnv original) "__tidepool_cell_pin_0_sessionInput" inputType
  signature <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict (toStrictByteString (encodeCheckedSignature capturedSignature))))
  issuedFields <- writeGenuineEmptyScopeFields issuedPath
  let sha = TString (T.replicate 64 "a")
      empty = TList []
      text = TString . T.pack
      authorization = [text "host-activation-input2",sha,sha,TInt 0,sha,text "bind"
        ,TList [text "sessionInput"],TList [TList [text "bind",sha]],empty,TList [signature]
        ,TNull,TInt 1,sha,empty,TNull,TNull,empty,empty,empty,TList [text work]]
      -- These stage/purpose syntax checks wrap a genuinely issued immutable
      -- scope and native GHC signatures; they do not issue execution receipts.
      hostManifest auth = TList (replace 8 (TList auth) issuedFields)
      path = work </> "host-scope.cbor"
      decodeManifest manifest = BS.writeFile path (toStrictByteString (encodeTerm manifest)) >> readExactScope path
      decode = decodeManifest . hostManifest
      replace index value fields = [if ordinal == index then value else field | (ordinal,field) <- zip [0::Int ..] fields]
  admitted <- decode authorization >>= either fail pure
  unless (fmap itemPurpose (scopeCheckedItem admitted) == Just HostActivationInput) $
    fail "host purpose lost its sealed role"
  authored <- decode (replace 0 (text "checked-item3") authorization) >>= either fail pure
  unless (fmap itemPurpose (scopeCheckedItem authored) == Just AuthoredCheckedItem) $
    fail "ordinary purpose acquired host authority"
  forM_ [(0,text "host-activation-input1"),(3,TInt 1),(5,text "expr")
      ,(6,TList [text "other"]),(7,TList [TList [text "expr",sha]])
      ,(9,empty),(11,TInt 0),(12,TString (T.replicate 64 "0"))] $ \(index,value) -> do
    refused <- decode (replace index value authorization)
    unless (case refused of Left _ -> True; Right _ -> False) $
      fail ("invalid host purpose field was admitted: " ++ show index)
  _ <- decode authorization >>= either fail pure
  replySignature <- captureCheckedSignature (crHscEnv original) "request-reply" inputType
  requestTerm <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict (toStrictByteString
      (encodeRequestTypeSignatures (RequestTypeSignatures replySignature Nothing)))))
  let nativeAuthorization recipe inner = [text "request-types2", requestTerm, text recipe, inner]
  forM_ [("none",NoRequestHelpers),("actor-reply",ActorReplyHelpers)] $ \(tag,recipe) -> do
    requestScope <- decode (nativeAuthorization tag TNull) >>= either fail pure
    unless (scopeRequestTypes requestScope == Just (recipe,RequestTypeSignatures replySignature Nothing)
        && scopePurpose requestScope == NoCheckedPurpose
        && isNothing (scopeIncludePaths requestScope))
      (fail "native request wrapper lost recipe or granted an inner purpose")
    graphFree <- decodeManifest (TList (replace 7 TNull
      (replace 8 (TList (nativeAuthorization tag TNull)) issuedFields))) >>= either fail pure
    unless (scopeRequestTypes graphFree == scopeRequestTypes requestScope
        && isNothing (scopeCheckedItem graphFree)
        && null (scopeExecutionGraphs graphFree) && null (scopeExecutionOwners graphFree))
      (fail "graph-free request wrapper changed native recipe authority")
    legacy <- decodeManifest (TList [text "TPEXACTSCOPE",text "4",sha,sha,empty,empty,empty
      ,TList (nativeAuthorization tag TNull)])
    unless (case legacy of Left reason -> "unsupported exact scope" `isInfixOf` reason; Right _ -> False)
      (fail "native request wrapper admitted a legacy scope envelope")
    wrappedHost <- decode (nativeAuthorization tag (TList authorization)) >>= either fail pure
    unless (fmap itemPurpose (scopeCheckedItem wrappedHost) == Just HostActivationInput
        && fmap fst (scopeRequestTypes wrappedHost) == Just recipe)
      (fail "native request wrapper lost its protected inner purpose")
  forM_ [[text "request-types1",requestTerm,TNull]
        ,nativeAuthorization "unknown" TNull
        ,nativeAuthorization "none" (TList [text "request-types2",requestTerm,text "none",TNull])
        ,nativeAuthorization "actor-reply" (TList (replace 3 (TInt 1) authorization))
        ,nativeAuthorization "none" TNull ++ [TNull]] $ \invalid -> do
    refused <- decode invalid
    unless (case refused of Left _ -> True; Right _ -> False)
      (fail "native request wrapper admitted legacy, malformed or invalid inner authority")
  let checkedSignature = capturedSignature { signaturePresentation = "presentation is not Haskell syntax !" }
  encodedSignature <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict (toStrictByteString (encodeCheckedSignature checkedSignature))))
  _ <- decode (replace 9 (TList [encodedSignature]) authorization) >>= either fail pure
  initialSignature <- captureCheckedSignature (crHscEnv original) "activation-input" inputType
  initialTerm <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict (toStrictByteString (encodeCheckedSignature initialSignature))))
  let initialAuthorization = [text "host-input-check1",sha,sha,sha,TList [TList [text "bind",sha]]
        ,empty,empty,empty,initialTerm,TList [text work]]
      initialSource = "module HostActivationInput where\n__result :: TidepoolActivationInput\n__result = undefined\n"
      initialSession = emptySessionScope {ssRoot=work,ssExactScope=Just path}
  initialAdmitted <- decode initialAuthorization >>= either fail pure
  unless (fmap checkedCellPurpose (scopeCheckedCell initialAdmitted) == Just (HostInputCellCheck initialSignature))
    (fail "host input check lost its original native signature")
  writeFile sourcePath initialSource
  initialChecked <- runPipelineSessionSelected CheckedEnvironment Set.empty (HostActivationCheck initialSignature)
    (Just initialSession) sourcePath [work] Nothing
  unless (maybe False (eqType inputType) (crResultType initialChecked))
    (fail "host input first check lost the original type")
  forM_ [replace 0 (text "cell-check2") initialAuthorization
        ,replace 6 (TList [text "unexpected-declaration"]) initialAuthorization] $ \invalid -> do
    refused <- decode invalid
    unless (case refused of Left _ -> True; Right _ -> False)
      (fail "host input check admitted authored/declaration purpose substitution")
  _ <- decode (replace 9 (TList [encodedSignature]) authorization) >>= either fail pure
  untypedSource <- either fail pure (replaceTemplateMarker "{{CHECKED_TYPE}}" "()" inputTemplate)
  resultSlot <- either fail pure (replaceTemplateMarker "__result :: Int" "__result :: TidepoolActivationInput" untypedSource)
  checkedSource <- either fail pure (replaceTemplateMarker "__activationPreview :: Int" "__activationPreview :: TidepoolActivationInput" resultSlot)
  writeFile sourcePath checkedSource
  let session = emptySessionScope {ssRoot=work,ssExactScope=Just path}
      purpose = HostActivationInputCompile [("__tidepool_checked_annotation_0",
        checkedSignature)] Nothing []
      reject action = do
        refused <- try (void action) :: IO (Either SomeException ())
        unless (case refused of Left reason -> "host activation input" `isInfixOf` show reason; Right _ -> False) $
          fail "host input compiled through an unsealed or general purpose"
  reject (runPipelineSessionSelected CheckedEnvironment Set.empty purpose Nothing sourcePath [work] Nothing)
  reject (runPipelineSessionSelected CheckedEnvironment Set.empty GeneralCompile (Just session) sourcePath [work] Nothing)
  checked <- runPipelineSessionSelected CheckedEnvironment Set.empty purpose (Just session) sourcePath [work] Nothing
  unless (fmap renderType (crResultType checked) == Just "Int") $
    fail "host checked annotation changed the inferred input"
  actualInput <- either fail pure (activationPreviewInputType (crTargetTcGblEnv checked))
  originalInterfaces <- newOriginalInterfaceArtifacts (crHscEnv checked) Map.empty [] work
  let witness ty = captureCheckedTypeWitness (crHscEnv checked) ty
        >>= maybe (fail "complete fixture type has no canonical witness") pure
      sealedBytes ty = do
        raw <- witness ty
        sealed <- sealCheckedTypeWitness originalInterfaces raw
          >>= maybe (fail "fixture type witness has no original interface") pure
        maybe (fail "fixture type witness is unsealed") (pure . toStrictByteString) (encodeCheckedTypeWitness sealed)
  mismatchedSource <- either fail pure (replaceTemplateMarker "__activationPreview :: Int" "__activationPreview :: Bool" originalSource)
  writeFile sourcePath mismatchedSource
  mismatched <- runPipelineSessionSelected CheckedEnvironment Set.empty GeneralCompile Nothing sourcePath [work] Nothing
  unless (case activationPreviewInputType (crTargetTcGblEnv mismatched) of Left _ -> True; Right _ -> False) $
    fail "host preview admitted another type than its checked Val binder"
  writeFile sourcePath checkedSource
  originalBytes <- sealedBytes inputType
  actualBytes <- sealedBytes actualInput
  forward <- sealedBytes (mkVisFunTyMany intTy boolTy)
  backward <- sealedBytes (mkVisFunTyMany boolTy intTy)
  supply <- mkSplitUniqSupply 'w'
  let (firstUnique, remaining) = takeUniqFromSupply supply
      (secondUnique, _) = takeUniqFromSupply remaining
      alphaVariable unique name = mkTyVar (mkInternalName unique (mkTyVarOcc name) noSrcSpan) liftedTypeKind
      firstVariable = alphaVariable firstUnique "a"
      secondVariable = alphaVariable secondUnique "renamed"
      alphaType variable = mkForAllTy (Bndr variable (Invisible SpecifiedSpec))
        (mkVisFunTyMany (mkTyVarTy variable) (mkTyVarTy variable))
  alphaFirst <- sealedBytes (alphaType firstVariable)
  alphaSecond <- sealedBytes (alphaType secondVariable)
  freeWitness <- captureCheckedTypeWitness (crHscEnv checked) (mkTyVarTy firstVariable)
  unless (isNothing freeWitness) $
    fail "canonical type witness admitted a free type variable"
  let ownerPath = work </> "HostActivationOwner.hs"
      ownerWitness fixture = do
        copyFile ("test-source-boot/fixtures" </> fixture) ownerPath
        produced <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile Nothing ownerPath [work] Nothing
        let pipeline = pprPipelineResult produced
        ty <- maybe (fail "owner fixture has no input type") pure (prResultType pipeline)
        raw <- captureCheckedTypeWitness (prHscEnv pipeline) ty
          >>= maybe (fail "owner fixture has no canonical witness") pure
        artifacts <- newOriginalInterfaceArtifacts (prHscEnv pipeline) (pprFinalizedModules produced) [] work
        sealed <- sealCheckedTypeWitness artifacts raw
          >>= maybe (fail "owner fixture lacks its original interface") pure
        maybe (fail "owner witness is unsealed") (pure . toStrictByteString) (encodeCheckedTypeWitness sealed)
  originalOwner <- ownerWitness "HostActivationOwnerOriginal.hs"
  changedOwner <- ownerWitness "HostActivationOwnerChanged.hs"
  unless (originalOwner /= changedOwner) $ fail "same original Name ignored changed owner interface"
  forM_ (maybeToList destination) $ \directory -> do
    createDirectoryIfMissing True directory
    forM_ [("original-input.cbor",originalBytes),("preview-input.cbor",actualBytes)
      ,("forward-function.cbor",forward),("backward-function.cbor",backward),("alpha-first.cbor",alphaFirst),("alpha-second.cbor",alphaSecond),("original-owner.cbor",originalOwner),("changed-owner.cbor",changedOwner)] $ \(name,bytes) ->
      BS.writeFile (directory </> name) bytes
  unless (originalBytes == actualBytes && forward /= backward) $
    fail "canonical input witness lost original correspondence or function argument/result order"
  putStrLn "host activation purpose: sealed payload, checked type pass and ordinary/unsealed refusals passed"

freshExecutionRecipeTest :: IO ()
freshExecutionRecipeTest = withScratch $ \work -> do
  let source = "module Expr where\nanswer = 42\n"
      supportSource = "module Support where\nvalue = 42\n"
      supportPath = work </> "Support.hs"
      sha = replicate 64 'a'
      identity name = ExecutionSourceIdentity "main" name sha sha sha
      support = identity "Support"
      target = identity "Expr"
      evidence = DependencyEvidence True True
        [DependencySource "@generated-source" (digest (BSC.pack source)),
         DependencySource supportPath (digest (BSC.pack supportSource))]
        [] [] [DependencyModule "main" "Expr" False "@generated-source" [] ProductReady,
               DependencyModule "main" "Support" False supportPath [] ProductReady]
      recipe = ExecutionSourceRecipe sha (Just sha) [work] (work </> "Expr.hs",source)
        evidence [ExecutionSourceOwner target True Nothing,ExecutionSourceOwner support True Nothing]
        [] []
      issue value = either (fail . show) (maybe (fail "supported recipe was withheld") pure)
        (issueExecutionSourceRecipe value)
      refused value = case issueExecutionSourceRecipe value of Left _ -> True; _ -> False
  BS.writeFile supportPath (BSC.pack supportSource)
  graph <- issue recipe
  -- The graph envelope is independent of both metadata and authored source.
  -- Reuse this fixed recipe and its compiler evidence; no extra GHC cycle.
  let sourceRecipe value = recipe
        { recipeGeneratedOrigin=(fst (recipeGeneratedOrigin recipe),value)
        , recipeEvidence=evidence {dependencySources=
            [if dependencySourcePath row == "@generated-source"
              then row {dependencySourceSha256=digest (TE.encodeUtf8 (T.pack value))}
              else row | row <- dependencySources evidence]}}
      largeSource = source ++ replicate (4*1024*1024) ' '
      excessiveSource = replicate (8*1024*1024+1) '\x1f600'
  large <- issue (sourceRecipe largeSource)
  unless (BS.length (executionGraphBytes large) > 4*1024*1024) $
    fail "valid graph above metadata budget was withheld by local recipe issuance"
  unless (case issueExecutionSourceRecipe (sourceRecipe excessiveSource) of
      Right Nothing -> True; _ -> False) $
    fail "local recipe issuer accepted source above its 32 MiB UTF-8 bound"
  (_,graphTerm) <- either (fail . show) pure
    (deserialiseFromBytes decodeTerm (BSL.fromStrict (executionGraphBytes graph)))
  let oversizedSourceGraph = case graphTerm of
        TList fields -> TList [case (index,field) of
          (6,TList [origin,_]) -> TList [origin,TString (T.pack excessiveSource)]
          _ -> field | (index,field) <- zip [0::Int ..] fields]
        other -> other
      oversizedSourceBytes = toStrictByteString (encodeTerm oversizedSourceGraph)
  unless (case decodeExecutionSourceGraph (digest oversizedSourceBytes) oversizedSourceBytes of
      Left reason -> "32 MiB UTF-8 bound" `isInfixOf` reason; _ -> False) $
    fail "authenticated graph decoder accepted source above its UTF-8 bound"
  let reference = ExecutionSourceRef support (executionGraphSha256 graph)
  selected <- either (fail . show) pure (executionSourceProspectiveReferences [graph] [] [reference])
  unless (selected == [reference]) $ fail "fresh supported recipe did not issue exact original"
  -- These owner-level packets exercise contextual search witnesses without
  -- issuing a native certificate or compiling another immutable fixture.
  let firstAbsent = work </> "first-request" </> "Prelude.hs"
      secondAbsent = work </> "second-request" </> "Prelude.hs"
      contextual path = recipe {recipeIncludes=[takeDirectory path,work],recipeEvidence=evidence {
        dependencyModules=[if dependencyModuleName node == "Support"
          then node {dependencyModuleImports=[DependencyImport DependencyUnqualified "Prelude" False Nothing]}
          else node | node <- dependencyModules evidence],
        dependencyResolutions=[DependencyResolution DependencyUnqualified "Prelude" False Nothing [path]]}}
  firstContext <- issue (contextual firstAbsent)
  secondContext <- issue (contextual secondAbsent)
  let contexts = [firstContext,secondContext]
      firstRef = reference {executionRefGraph=executionGraphSha256 firstContext}
      secondRef = reference {executionRefGraph=executionGraphSha256 secondContext}
      contextRefs = [firstRef,secondRef]
  sharedContexts <- either (fail . show) pure (executionSourceOriginalClosure contexts contextRefs)
  sharedSupport <- case sharedContexts of
    [value] -> pure value
    _ -> fail "equivalent request contexts did not share one original source owner"
  retainedResolutions <- either (fail . show) pure (executionNodeOriginalResolutions contexts sharedSupport)
  unless (executionNodeOriginalGraphs sharedSupport == Set.fromList (map executionGraphSha256 contexts)
      && Set.fromList (concatMap dependencyResolutionCandidates retainedResolutions)
          == Set.fromList [firstAbsent,secondAbsent]) $
    fail "shared original source lost a request's negative-resolution witnesses"
  unless (case executionNodeOriginalResolutions [firstContext] sharedSupport of
      Left (ExecutionSourceMissing _) -> True; _ -> False) $
    fail "shared original source accepted a missing retained witness context"
  unless (fmap (map dependencyResolutionCandidates)
        (executionNodeOriginalResolutions (firstContext:contexts) sharedSupport)
        == Right (map dependencyResolutionCandidates retainedResolutions)
      && case executionNodeOriginalResolutions [firstContext,secondContext {
          executionGraphSha256=executionGraphSha256 firstContext}] sharedSupport of
        Left (ExecutionSourceConflicting _) -> True; _ -> False) $
    fail "original witness inventory did not deduplicate identical graphs or refuse conflicting graph bytes"
  let changedSource = secondContext {executionGraphEvidence=(executionGraphEvidence secondContext) {
        dependencySources=[if dependencySourcePath row == supportPath
          then row {dependencySourceSha256=replicate 64 'f'} else row
          | row <- dependencySources (executionGraphEvidence secondContext)]}}
      changedNative = secondContext {executionGraphOwners=[if executionOwnerIdentity owner' == support
        then owner' {executionOwnerIdentity=support {executionNativeSha256=replicate 64 'f'}} else owner'
        | owner' <- executionGraphOwners secondContext]}
      changedResolution = secondContext {executionGraphEvidence=(executionGraphEvidence secondContext) {
        dependencyResolutions=[row {dependencyResolutionSelected=Just secondAbsent}
          | row <- dependencyResolutions (executionGraphEvidence secondContext)]}}
  forM_ [(changedSource,secondRef),(changedNative,secondRef {
        executionRefIdentity=support {executionNativeSha256=replicate 64 'f'}})
      ,(changedResolution,secondRef)] $ \(changed,changedRef) ->
    unless (case executionSourceOriginalClosure [firstContext,changed] [firstRef,changedRef] of
        Left (ExecutionSourceConflicting _) -> True; _ -> False) $
      fail "context sharing accepted a different source/native/selected-resolution identity"
  unless (refused recipe {recipeProducer=replicate 64 '0'}
      && refused recipe {recipeOwners=[ExecutionSourceOwner support True Nothing]}
      && refused recipe {recipeExactImports=[(("main","Absent"),[])]}
      && refused recipe {recipeEvidence=evidence {dependencySources=[]}}) $
    fail "issuer admitted an incomplete owner/generated/producer/exact-import proof"
  let legacyRecipe = recipe {recipeOwners=[ExecutionSourceOwner target True Nothing,
        ExecutionSourceOwner support False Nothing]}
  legacy <- issue legacyRecipe
  unavailable <- either (fail . show) pure (executionSourceProspectiveReferences [legacy] []
    [reference {executionRefGraph=executionGraphSha256 legacy}])
  unless (null unavailable) $ fail "source-free legacy owner acquired a current-source recipe"
  promised <- issue legacyRecipe {recipeOwners=[ExecutionSourceOwner target True Nothing,
    ExecutionSourceOwner support False (Just (replicate 64 'b'))]}
  unless (case executionSourceProspectiveReferences [promised] []
      [reference {executionRefGraph=executionGraphSha256 promised}] of Left _ -> True; _ -> False) $
    fail "missing promised original graph became optional unavailability"
  let retainedLegacy = legacy {executionGraphSha256=replicate 64 'c'}
  nested <- issue legacyRecipe {recipeOwners=[ExecutionSourceOwner target True Nothing,
    ExecutionSourceOwner support False (Just (executionGraphSha256 retainedLegacy))]}
  unless (case executionSourceProspectiveReferences [nested,retainedLegacy] []
      [reference {executionRefGraph=executionGraphSha256 nested}] of Left _ -> True; _ -> False) $
    fail "promised original graph lost its strict legacy-capability refusal"
  unless (case executionSourceProspectiveReferences [legacy]
      [reference {executionRefGraph=replicate 64 'b'}] [] of Left _ -> True; _ -> False) $
    fail "unsupported prospective recipe hid corrupt inherited advertised proof"
  let original = ExactProduct "main" "Support" sha sha sha "" []
      scope = ExactScope "" sha sha sha
        [(ExactIfaceArtifact "main" "Support" "" sha [],"",sha)] Map.empty [] [original]
        [] [] NoCheckedPurpose Nothing Set.empty
      oversized = graph {executionGraphBytes=BS.replicate (executionSourceGraphBytesLimit+1) 0}
  unless (case decodeExecutionSourceGraph (executionGraphSha256 graph)
      (executionGraphBytes oversized) of Left _ -> True; _ -> False) $
    fail "direct graph decoder admitted an oversized byte envelope"
  bounded <- either (fail . show) pure
    (extendExactExecutionSourcesWithinBudget [oversized] [reference] scope)
  unless (isNothing bounded && case extendExactExecutionSources [oversized] [reference] scope of
      Left _ -> True; _ -> False) $ fail "optional/advertised aggregate budget policies diverged"
  unless (case extendExactExecutionSourcesWithinBudget [oversized]
      [reference {executionRefGraph=replicate 64 'b'}] scope of Left _ -> True; _ -> False) $
    fail "aggregate budget withholding hid corrupt advertised graph"
  putStrLn "fresh execution recipe: issuer, original lineage, shared negative contexts and strict/optional budget controls passed"

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

candidateExecutionWire :: FilePath -> IO ()
candidateExecutionWire path = do
  candidates <- readModuleCandidates path >>= either fail pure
  let parcels = [value | candidate <- candidates, Just value <- [candidateExecutionSources candidate]]
      graphs = concatMap fst parcels
      references = map snd parcels
      native = map candidateOriginalIdentity candidates
      quoter = [executionIdentityKey value | value <- native, executionModule value == "Quoter"]
  unless (length candidates == 2 && length parcels == 2 && length quoter == 1) $
    fail "production Rust candidate fixture lost its two original recipes"
  forM_ candidates $ \candidate -> do
    iface <- BS.readFile (candidateInterface candidate)
    products <- BS.readFile (candidateProductPath candidate)
    source <- BS.readFile (candidateSource candidate)
    packages <- BS.readFile (candidatePackageImports candidate)
    unless (digest iface == candidateInterfaceSha256 candidate
        && digest products == candidateProductSha256 candidate
        && digest source == candidateSourceSha256 candidate
        && digest packages == candidatePackageImportsSha256 candidate) $
      fail "production Rust encoder changed its framed native/interface/source/package pairing"
  nodes <- either (fail . show) pure (executionSourceClosure graphs references native quoter)
  unless (length nodes == 2) (fail "production Rust retained graph edge lost its exact original source closure")
  quoterReference <- case [reference | reference <- references
    , executionModule (executionRefIdentity reference) == "Quoter"] of
    [value] -> pure value
    _ -> fail "Rust fixture lacks its quoter reference"
  let wrongRetained = [if executionModule (executionRefIdentity reference) == "Fresh"
        then reference {executionRefGraph=executionRefGraph quoterReference} else reference | reference <- references]
      wrongNative = [if executionModule original == "Fresh"
        then original {executionVersion=replicate 64 'f'} else original | original <- native]
  forM_ [(wrongRetained,native),(references,wrongNative)] $ \(selected,current) ->
    case executionSourceClosure graphs selected current quoter of
      Left _ -> pure ()
      Right _ -> fail "retained candidate execution admitted another current owner or graph digest"
  putStrLn ("Rust TPMCAN8 decoder/retained closure: owners=" ++ show (map candidateModule candidates)
    ++ " graphs=" ++ show (Set.size (Set.fromList (map executionGraphSha256 graphs)))
    ++ "; source/native/interface/package bytes match; owner/digest refusals passed (synthetic decoder fixture, not GHC admission)")
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

-- A real splice executes in both dependency and target. The recorded side
-- effect rejects replay even if two frontend executions produce identical Core.
finalizedFrontendOnce :: IO ()
finalizedFrontendOnce = withTiming $ withScratch $ \work -> do
  let fixture name = "test-source-boot/fixtures" </> name
      counter = work </> "frontend-counts"
      target = work </> "FinalizedSpliceTarget.hs"
      owner = mkModuleName "FinalizedSpliceOwner"
      unused = mkModuleName "FinalizedSpliceUnused"
      targetOwner = mkModule (stringToUnit "main") (mkModuleName "FinalizedSpliceTarget")
      hasNativeResult expected = any (\(binder,rhs) ->
        nameModule_maybe (idName binder) == Just targetOwner
          && getOccString binder == "result" && containsLiteral expected rhs) . Core.flattenBinds
      containsLiteral expected = \case
        Core.Lit (LitNumber LitNumInt value) -> value == expected
        Core.App function argument -> containsLiteral expected function || containsLiteral expected argument
        Core.Lam _ body -> containsLiteral expected body
        Core.Let binding body -> any (containsLiteral expected . snd) (Core.flattenBinds [binding])
          || containsLiteral expected body
        Core.Case scrutinee _ _ alternatives -> containsLiteral expected scrutinee
          || any (\(Core.Alt _ _ rhs) -> containsLiteral expected rhs) alternatives
        Core.Cast body _ -> containsLiteral expected body
        Core.Tick _ body -> containsLiteral expected body
        _ -> False
      executions expected diagnostics = do
        actual <- lines <$> readFile counter
        require "native splice executions" [("exact-inventory",sort actual == sort expected)]
          diagnostics ("expected=" ++ show expected ++ "\nactual=" ++ show actual)
      capturedCount phase name diagnostics = length
        [line | line <- lines diagnostics, line == "tidepool-canonical-" ++ phase
          ++ " module=" ++ name]
      require :: String -> [(String,Bool)] -> String -> String -> IO ()
      require label checks diagnostics details = unless (all snd checks) $ do
        hPutStrLn stderr (label ++ " assertions=" ++ show checks ++ "\n" ++ details)
        hPutStrLn stderr diagnostics
        fail (label ++ " failed: " ++ show [name | (name,False) <- checks])
      preparedDetails :: PreparedPipelineResult -> String
      preparedDetails prepared =
        "warnings=" ++ show (prWarnings (pprPipelineResult prepared))
        ++ "\nfinalized=" ++ show (map moduleNameString (Map.keys (pprFinalizedModules prepared)))
        ++ "\nprepared=" ++ show (preparedNames prepared)
        ++ "\nCore=" ++ showSDocUnsafe (ppr (prBinds (pprPipelineResult prepared)))
        ++ "\nfinalized-Core=" ++ showSDocUnsafe (ppr
          [(name,cg_binds (finalizedTidyGuts finalized))
          | (name,finalized) <- Map.toAscList (pprFinalizedModules prepared)])
  forM_ ["FinalizedSpliceOwner.hs", "FinalizedSpliceUnused.hs", "FinalizedSpliceTarget.hs"] $ \name ->
    copyFile (fixture name) (work </> name)
  -- CheckOnly stops before desugaring, where the fixture's incomplete-pattern
  -- warning is issued. Also exercise a warning from native typechecking.
  targetSource <- readFile target
  evaluate (length targetSource) >> writeFile target
    ("{-# OPTIONS_GHC -Wmissing-signatures #-}\n" ++ targetSource
      ++ "\nnativeTypecheckWarning = ()\n")
  bracket (lookupEnv "TIDEPOOL_TEST_FINALIZATION_COUNTER")
    (maybe (unsetEnv "TIDEPOOL_TEST_FINALIZATION_COUNTER")
      (setEnv "TIDEPOOL_TEST_FINALIZATION_COUNTER")) $ \_ -> do
    setEnv "TIDEPOOL_TEST_FINALIZATION_COUNTER" counter
    withResidentPipelineSelected [work] $ \compile -> do
      let run = captureDiagnostics $
            compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] Nothing
      (result, diagnostics) <- run
      executions ["owner", "target"] diagnostics
      require "cold finalized frontend"
        [ ("native-result",hasNativeResult 42 (prBinds (pprPipelineResult result)))
        , ("warnings",not (null (prWarnings (pprPipelineResult result))))
        , ("provider-owner",Map.member owner (pprFinalizedModules result))
        , ("unused-owner",Map.member unused (pprFinalizedModules result))
        , ("lazy-STG","FinalizedSpliceUnused" `notElem` preparedNames result)
        ] diagnostics (preparedDetails result)
      forM_ ["FinalizedSpliceOwner", "FinalizedSpliceUnused", "FinalizedSpliceTarget"] $ \name ->
        require ("canonical phase for " ++ name)
          [("frontend-once",capturedCount "frontend" name diagnostics == 1)
          ,("finalization-once",capturedCount "finalization" name diagnostics == 1)]
          diagnostics (preparedDetails result)
      writeFile counter ""
      (checked, checkedDiagnostics) <- captureDiagnostics $
        compile CheckedEnvironment Set.empty GeneralCompile Nothing target [work] Nothing
      actual <- lines <$> readFile counter
      require "checked finalized frontend"
        [("target-splice-once",length (filter (== "target") actual) == 1)
        ,("provider-splice-capture",length (filter (== "owner") actual)
          == capturedCount "frontend" "FinalizedSpliceOwner" checkedDiagnostics)
        ,("provider-splice-at-most-once",length (filter (== "owner") actual) <= 1)
        ,("splice-inventory",all (`elem` ["owner", "target"]) actual)
        ,("result-type",isJust (crResultType checked))
        ,("warnings",not (null (crWarnings checked)))]
        checkedDiagnostics ("splices=" ++ show actual ++ "\nresult-type="
          ++ show (fmap renderType (crResultType checked)) ++ "\nwarnings=" ++ show (crWarnings checked))
      -- A changed provider forces a fresh authoritative native frontend; its
      -- splice and the target splice must still each execute once.
      source <- T.pack <$> readFile (fixture "FinalizedSpliceOwner.hs")
      writeFile (work </> "FinalizedSpliceOwner.hs")
        (T.unpack (T.replace "[| 42 :: Int |]" "[| 43 :: Int |]" source))
      writeFile counter ""
      (changed, changedDiagnostics) <- run
      executions ["owner", "target"] changedDiagnostics
      require "changed finalized frontend"
        [("native-result",hasNativeResult 43 (prBinds (pprPipelineResult changed)))]
        changedDiagnostics (preparedDetails changed)
  putStrLn "finalized frontend: splice once, native bytecode, warnings and lazy STG passed"

quasiQuoteCodegenTransition :: IO ()
quasiQuoteCodegenTransition = withTiming $ withScratch $ \work -> do
  let fixture name = "test-source-boot/fixtures" </> name
      target = work </> "MetadataQuoteFreeTarget.hs"
      providers = ["MetadataQuoter", "MetadataQuoteSupport"]
      providerExecutable name env = case lookupHpt (hsc_HPT env) (mkModuleName name) of
        Just hmi -> let linkable = hm_linkable hmi
          in isJust (homeMod_bytecode linkable) || isJust (homeMod_object linkable)
        Nothing -> False
  forM_ ["MetadataQuoteFreeTarget.hs", "MetadataQuoter.hs", "MetadataQuoteSupport.hs"] $ \name ->
    copyFile (fixture name) (work </> name)
  quoteFree <- readFile (fixture "MetadataQuoteFreeTarget.hs")
  quoted <- readFile (fixture "MetadataQuotedTarget.hs")
  let quotedTarget = T.unpack (T.replace "MetadataQuotedTarget" "MetadataQuoteFreeTarget" (T.pack quoted))
  withResidentPipelineSelected [work] $ \compile -> do
    let run = captureDiagnostics $
          compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] Nothing
        requireFree label (result, diagnostics) = do
          unless (hasIntResultLiteral 0 (prBinds (pprPipelineResult result))
              && counterValues "quasiquote_codegen_elided_modules" diagnostics == [2]
              && all (\name -> not (providerExecutable name (prHscEnv (pprPipelineResult result)))) providers) $
            fail (label ++ ": quote-free graph retained executable providers or changed its result")
    run >>= requireFree "cold"
    writeFile target quotedTarget
    (executing, diagnostics) <- run
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult executing))
        && null (counterValues "quasiquote_codegen_elided_modules" diagnostics)
        && all (\name -> providerExecutable name (prHscEnv (pprPipelineResult executing))) providers) $
      fail "warm real quote did not provision and execute its transitive providers"
    writeFile target quoteFree
    run >>= requireFree "warm after quotation"
    copyFile (fixture "MetadataQuoteSupportExternalPlugin.hs") (work </> "MetadataQuoteSupport.hs")
    (pluginResult, pluginDiagnostics) <- captureDiagnostics $
      try (compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] Nothing)
        :: IO (Either SomeException PreparedPipelineResult, String)
    case pluginResult of
      Left failure
        | "tidepool-quotation-plugin" `isInfixOf` show failure
        , null (counterValues "quasiquote_codegen_elided_modules" pluginDiagnostics) -> pure ()
      _ -> fail "external library plugin input entered quotation elision or bypassed GHC loading"
    copyFile (fixture "MetadataQuoteSupport.hs") (work </> "MetadataQuoteSupport.hs")
    run >>= requireFree "after plugin refusal"
  putStrLn "quasiquote codegen: resident quote-free/real-quote/quote-free, external plugin refusal and recovery passed"

exactLoadedMetadata :: IO ()
exactLoadedMetadata = withTiming $ withScratch $ \work -> do
  let fixture name = "test-source-boot/fixtures" </> name
      install name = copyFile (fixture name) (work </> name)
      scopePath = work </> "exact-scope.cbor"
      scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath
        , ssIncarnation = Just "loaded-metadata-test" }
      resultType = fmap renderType . crResultType
      loadedOwner = "tidepool-checked-loaded-source module=MetadataOwner"
      checkedOwner = "tidepool-checked module=MetadataOwner target=False"
      frontendCount name diagnostics = length (filter (==
          "tidepool-canonical-frontend module=" ++ name) (lines diagnostics))
        + length (filter (== "tidepool-checked module=" ++ name ++ " target=False") (lines diagnostics))
  forM_ ["MetadataOwner.hs", "MetadataTarget.hs", "MetadataExtensionOnlyTarget.hs", "MetadataLoadedFamily.hs"
    , "MetadataFamilyTarget.hs", "MetadataHiddenFamily.hs", "MetadataUntracked.hs"
    , "MetadataUntrackedTarget.hs", "MetadataQuoter.hs", "MetadataQuotedTarget.hs", "MetadataQuoteSupport.hs"] install
  writeGenuineEmptyMetadataScope scopePath
  withResidentPipelineSelected [work] $ \compile -> do
    let checked name = compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
          (work </> name) [work] Nothing
    ordinary <- compile CheckedEnvironment Set.empty GeneralCompile Nothing
      (work </> "MetadataTarget.hs") [work] Nothing
    (exact, diagnostics) <- captureDiagnostics (checked "MetadataTarget.hs")
    unless (resultType exact == Just "Int" && resultType ordinary == resultType exact
        && frontendCount "MetadataOwner" diagnostics == 1
        && not (loadedOwner `elem` lines diagnostics && checkedOwner `elem` lines diagnostics)
        && "tidepool-checked module=MetadataTarget target=True" `elem` lines diagnostics) $
      fail "exact metadata repeated the loaded source frontend or changed its instance result"
    putStrLn ("exact loaded metadata evidence: owner_frontends="
      ++ show (frontendCount "MetadataOwner" diagnostics))
    (extensionOnly, extensionDiagnostics) <- captureDiagnostics
      (checked "MetadataExtensionOnlyTarget.hs")
    let executableOwner env = case lookupHpt (hsc_HPT env) (mkModuleName "MetadataOwner") of
          Just hmi -> let linkable = hm_linkable hmi
            in isJust (homeMod_bytecode linkable) || isJust (homeMod_object linkable)
          Nothing -> False
    unless (resultType extensionOnly == resultType exact
        && counterValues "quasiquote_codegen_elided_modules" extensionDiagnostics == [1]
        && not (executableOwner (crHscEnv extensionOnly))) $
      fail "extension-only metadata unnecessarily provisioned an executable dependency"
    (extensionNative, nativeDiagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
        (work </> "MetadataExtensionOnlyTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult extensionNative))
        && counterValues "quasiquote_codegen_elided_modules" nativeDiagnostics == [1]
        && not (executableOwner (prHscEnv (pprPipelineResult extensionNative)))) $
      fail "extension-only native compilation changed its result or retained dependency execution"
    copyFile (fixture "MetadataOwnerWithoutInstance.hs") (work </> "MetadataOwner.hs")
    changed <- try (checked "MetadataTarget.hs") :: IO (Either SomeException CheckedEnvironmentResult)
    case changed of
      Left _ -> pure ()
      Right _ -> fail "fresh exact scope reused the previous source instance"
    install "MetadataOwner.hs"
    recovered <- checked "MetadataTarget.hs"
    unless (resultType recovered == Just "Int") $ fail "exact metadata did not recover after source refusal"
    (quoted, quoteDiagnostics) <- captureDiagnostics (checked "MetadataQuotedTarget.hs")
    unless (resultType quoted == Just "Int"
        && null (counterValues "quasiquote_codegen_elided_modules" quoteDiagnostics)
        && "tidepool-checked-dependency-executable module=MetadataQuoter bytecode=True object=False" `elem` lines quoteDiagnostics
        && frontendCount "MetadataQuoter" quoteDiagnostics == 1) $
      fail "exact metadata discarded the loaded quoter's executable linkable"
    quoterProducer <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "MetadataQuoter.hs") [work] Nothing
    -- The helper has no TH extension or quotation itself. GHC's graph still
    -- requires its bytecode when the quoter executes in the target.
    writeGenuineCandidateManifestFor ["MetadataQuoteSupport"] work
      (work </> "MetadataQuoter.hs") [work] quoterProducer
    (quotedCandidate, candidateQuoteDiagnostics) <- captureDiagnostics $
      compile (CheckedEnvironmentProducts (manifest work)) Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
    unless (resultType quotedCandidate == Just "Int"
        && frontendCount "MetadataQuoter" candidateQuoteDiagnostics == 1
        && "tidepool-checked-dependency-executable module=MetadataQuoteSupport bytecode=True object=False" `elem` lines candidateQuoteDiagnostics
        && counterValues "candidate_executable_required" candidateQuoteDiagnostics == [1]) $
      fail "source candidate discarded a GHC-required quoter executable"
    putStrLn ("candidate loaded metadata evidence: accepted_count="
      ++ show (counterValues "candidate_admission.CandidateAccepted" candidateQuoteDiagnostics)
      ++ " executable_demand=" ++ show (counterValues "candidate_executable_required" candidateQuoteDiagnostics)
      ++ " helper_frontends=" ++ show (frontendCount "MetadataQuoteSupport" candidateQuoteDiagnostics)
      ++ " quoter_frontends=" ++ show (frontendCount "MetadataQuoter" candidateQuoteDiagnostics))
    hidden <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "MetadataHiddenFamily.hs") [work] Nothing
    writeGenuineMetadataScope scopePath work (work </> "MetadataHiddenFamily.hs") [work]
      ["MetadataHiddenFamily"] hidden
    ordinaryProducts <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "MetadataTarget.hs") [work] Nothing
    writeGenuineCandidateManifestFor ["MetadataOwner"] work
      (work </> "MetadataTarget.hs") [work] ordinaryProducts
    disjoint <- compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile (Just scope)
      (work </> "MetadataTarget.hs") [work] Nothing
    unless (map candidateModule (pprAcceptedCandidates disjoint) == ["MetadataOwner"]
        && fmap renderType (prResultType (pprPipelineResult disjoint)) == Just "Int"
        && "MetadataHiddenFamily" `notElem`
          [moduleNameString (ms_mod_name summary)
          | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph (prHscEnv (pprPipelineResult disjoint)))]) $
      fail "disjoint source candidate lost exact hidden-owner graph isolation"
    -- Metadata-only admission consumes the certified interface without asking
    -- for its companion Core. Exercise the real final checked receipt path.
    delivered <- readModuleCandidates (manifest work) >>= either fail pure
    owner <- case [candidate | candidate <- delivered, candidateModule candidate == "MetadataOwner"] of
      [candidate] -> pure candidate
      _ -> fail "genuine metadata fixture lost its native owner"
    let corePath = fst (candidateCoreDescriptor (candidateModuleInterface owner))
    originalCore <- BS.readFile corePath
    (do
      removeFile corePath
      (metadataOnly, metadataDiagnostics) <- captureDiagnostics $
        compile (CheckedEnvironmentProducts (manifest work)) Set.empty GeneralCompile (Just scope)
          (work </> "MetadataTarget.hs") [work] Nothing
      restoredCore <- doesFileExist corePath
      unless (resultType metadataOnly == Just "Int" && not restoredCore
          && counterValues "candidate_admission.CandidateAccepted" metadataDiagnostics == [1]
          && counterValues "candidate_executable_required" metadataDiagnostics == [0]) $
        fail "metadata-only candidate required or restored executable Core"
      putStrLn ("metadata-only candidate evidence: accepted_count="
        ++ show (counterValues "candidate_admission.CandidateAccepted" metadataDiagnostics)
        ++ " executable_demand=" ++ show (counterValues "candidate_executable_required" metadataDiagnostics)
        ++ " owner_frontends=" ++ show (frontendCount "MetadataOwner" metadataDiagnostics))
      ) `finally` BS.writeFile corePath originalCore
    family <- try (checked "MetadataFamilyTarget.hs") :: IO (Either SomeException CheckedEnvironmentResult)
    case family of
      Left failure | "retained family consistency" `isInfixOf` show failure -> pure ()
      _ -> fail "loaded metadata lost the hidden original family conflict"
    writeGenuineEmptyMetadataScope scopePath
    (_, untrackedDiagnostics) <- captureDiagnostics (checked "MetadataUntrackedTarget.hs")
    unless (frontendCount "MetadataUntracked" untrackedDiagnostics == 1
        && "tidepool-checked-loaded-source module=MetadataUntracked" `elem` lines untrackedDiagnostics) $
      fail "untracked compile-time input repeated its native frontend"
    receipts <- listDirectory (work </> ".exact-compilations")
    receiptSafety <- fmap catMaybes $ forM receipts $ \entry -> do
      bytes' <- BS.readFile (work </> ".exact-compilations" </> entry </> "receipt.cbor")
      either (\reason -> fail ("invalid exact compilation receipt " ++ entry ++ ": " ++ reason)) pure $
        exactCompilationCacheSafety (work </> "MetadataUntrackedTarget.hs") bytes'
    case receiptSafety of
      [cacheSafe] -> do
        putStrLn ("untracked target receipt: schema=TPEXACTCOMPILE/2 fields=10 "
          ++ "dependency_evidence=v4 cache_safe=" ++ show cacheSafe)
        unless (not cacheSafe) $
          fail "untracked dependency receipt marked dependency evidence cache_safe=true"
      [] -> fail "no exact compilation v2 receipt matched the untracked target source"
      _ -> fail ("multiple exact compilation receipts matched the untracked target: "
        ++ show (length receiptSafety))
  putStrLn "exact loaded metadata: parity, extension-only execution elision, source drift, quoter bytecode, hidden family and untracked input passed"

-- Native candidates and exact owners bypass fresh preparation. Their defining
-- interfaces must still supply typed site siblings without widening imports.
hydratedSiteSiblings :: IO ()
hydratedSiteSiblings = withScratch $ \work -> do
  let unfoldName = "Tidepool.Actors.Unfold"
      replyName = "Tidepool.Agent.Reply.Internal"
      names = [replyName,unfoldName]
      target = work </> "HydratedSiteExpr.hs"
      unfoldPath = work </> "Tidepool/Actors/Unfold.hs"
      replyPath = work </> "Tidepool/Agent/Reply/Internal.hs"
      scopePath = work </> "exact-scope.cbor"
      scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
      compile selection session = runPipelineSessionSelected selection Set.empty GeneralCompile
        session target [work] Nothing
      targetModule prepared = case [value | value <- pprModules prepared
          , moduleNameString (moduleName (pmModule value)) == "HydratedSiteExpr"] of
        [value] -> pure value
        _ -> fail "hydrated sibling fixture lost its target"
      evidence prepared = do
        target' <- targetModule prepared
        unless (null (pmSiteRejections target')) $
          fail ("hydrated child site was rejected: " ++ show (map srMessage (pmSiteRejections target')))
        let root = SymbolIdentity "main" "HydratedSiteExpr" "value" "__result" Nothing
            sibling = SymbolIdentity "main" "Tidepool.Actors.Unfold" "value" "childSited" Nothing
            context = ProjectionContext "test" "matched"
              (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
              root [] Nothing Nothing Nothing Nothing
            tops (NonRecursive binding) = [binding]
            tops (Recursive bindings) = bindings
        program <- either (fail . show) pure (projectPrepared context [target'])
        unless ([length arguments | TopBinding identity (HeapBinding _ (Function _ arguments _ _))
              <- concatMap tops (programBindings program), identity == root] == [1]
            && any ((== sibling) . globalIdentity) (programGlobals program)) $
          fail "hydrated sibling changed the capture root arity or original defining global"
        case pmYieldSites target' of
          [site] | ysOrigin site == "HydratedSiteExpr.__result"
            , stType (ysAnswer site) == "Bool"
            , map stType (ysInputs site) == ["Char"] -> pure site
          actual -> fail ("hydrated sibling changed the lexical site/root/input arity: " ++ show actual)
  createDirectoryIfMissing True (work </> "Tidepool/Actors")
  createDirectoryIfMissing True (work </> "Tidepool/Agent/Reply")
  copyFile "test-source-boot/fixtures/HydratedSiteUnfold.hs" unfoldPath
  copyFile "test-source-boot/fixtures/HydratedSiteReply.hs" replyPath
  copyFile "test-source-boot/fixtures/HydratedSiteExpr.hs" target
  cold <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing target [work] Nothing
  originalSite <- evidence cold
  writeManifestFor names work cold
  warm <- compile (PreparedProducts (Just (manifest work))) Nothing
  unless (sortOn id (map candidateModule (pprAcceptedCandidates warm)) == sortOn id names
      && all (`notElem` preparedNames warm) names) $
    fail "hydrated sibling regression did not take native-candidate reuse"
  warmSite <- evidence warm
  unless (warmSite == originalSite) (fail "native-candidate hydration changed exact child-site identity")
  let env = prHscEnv (pprPipelineResult cold)
  owners <- forM names $ \name -> do
    let hi = work </> (name ++ ".candidate.hi")
        packages = hi ++ ".packages"
    bytes <- BS.readFile hi
    packageBytes <- BS.readFile packages
    requirements <- either fail pure (selectedHomeRequirements (pprDependencies cold) "main" name)
    pure (ExactIfaceArtifact "main" name hi (digest bytes) requirements,packages,digest packageBytes)
  writeExactMetadataScopeWithLexical scopePath owners [(artifact,exactRequirements artifact) | (artifact,_,_) <- owners]
  exact <- compile (PreparedProducts Nothing) (Just scope)
  unless (all (`notElem` preparedNames exact) names) $
    fail "hydrated sibling regression recompiled an exact defining owner"
  exactSite <- evidence exact
  unless (exactSite == originalSite) (fail "exact hydration changed child-site identity")
  targetSource <- BSC.unpack <$> BS.readFile target
  writeFile target (T.unpack (T.replace "module HydratedSiteExpr where"
    "module HydratedSiteExpr (result) where" (T.pack targetSource)))
  let compilePrivate session = runPipelineSessionSelected (PreparedProducts (Just (manifest work)))
        Set.empty OriginalDeclarationCompile session target [work] Nothing
  privateCandidate <- compilePrivate Nothing
  privateExact <- compilePrivate (Just scope)
  forM_ [privateCandidate,privateExact] $ \prepared -> do
    unless (all (`notElem` preparedNames prepared) names) $
      fail "private capture regression recompiled a hydrated defining owner"
    privateInterface <- maybe (fail "private capture fixture lacks its target interface") pure
      (Map.lookup (mkModuleName "HydratedSiteExpr") (pprProductInterfaces prepared))
    unless (all ((/= "__result") . getOccString) (concatMap availNames (mi_exports privateInterface))) $
      fail "private capture root became a lexical module export"
    privateSite <- evidence prepared
    unless (privateSite == originalSite) (fail "private capture root changed its child-site identity")
  writeFile target targetSource
  home <- maybe (fail "hydrated sibling fixture lacks its original owner") pure
    (lookupHpt (hsc_HPT env) (mkModuleName unfoldName))
  let wrong = home {hm_iface=set_mi_module (mkModule (stringToUnit "other") (mkModuleName unfoldName)) (hm_iface home)}
      invalid = hscUpdateHPT (\table -> addToHpt table (mkModuleName unfoldName) wrong) env
  -- A same-spelling interface with another defining unit cannot authorize IDs
  -- whose Names still belong to the original owner.
  unless (Map.notMember "child" (resolvePreparedInterfaceSiblings invalid)) $
    fail "wrong defining interface owner authorized a sibling"
  surface <- case [binder | binder <- typeEnvIds (md_types (hm_details home)), getOccString binder == "child"] of
    [binder] -> pure binder
    _ -> fail "cold HPT lacks its genuine child surface Id"
  spec <- maybe (fail "cold HPT child did not match its declared surface module") pure (lookupPreparedVerb surface)
  let siblings = resolvePreparedInterfaceSiblings env
      arguments = map Core.Type [boolTy,intTy,charTy,stringTy]
  case classifySiteOccurrence siblings spec surface arguments of
    Right _ -> pure ()
    Left _ -> fail "genuine cold HPT child/sibling pair was refused"
  sibling <- maybe (fail "cold HPT lacks its genuine child sibling Id") pure (Map.lookup "child" siblings)
  uniqueSupply <- mkSplitUniqSupply 's'
  let (foreignUnique,remaining) = takeUniqFromSupply uniqueSupply
      (surfaceUnique,remaining') = takeUniqFromSupply remaining
      (siblingUnique,_) = takeUniqFromSupply remaining'
      originalName = idName surface
      foreignSurface = setIdName surface (mkExternalName foreignUnique
        (mkModule (stringToUnit "other") (mkModuleName unfoldName))
        (nameOccName originalName) (nameSrcSpan originalName))
      unnamedSurface = setIdName surface (mkInternalName surfaceUnique
        (nameOccName originalName) (nameSrcSpan originalName))
      unnamedSibling = setIdName sibling (mkInternalName siblingUnique
        (nameOccName (idName sibling)) (nameSrcSpan (idName sibling)))
  -- Alter only the surface's defining unit; its occurrence, module and type
  -- remain identical to GHC's genuine child Id, and the home sibling is valid.
  case classifySiteOccurrence siblings spec foreignSurface arguments of
    Left MismatchedSiblingUnit -> pure ()
    _ -> fail "a foreign-unit child surface acquired the valid home sibling"
  forM_ [(siblings,unnamedSurface),(Map.insert "child" unnamedSibling siblings,surface)] $ \(available,verb) ->
    case classifySiteOccurrence available spec verb arguments of
      Left MissingSiteOwner -> pure ()
      _ -> fail "a site pair without a defining module acquired sibling authority"
  source <- BSC.unpack <$> BS.readFile unfoldPath
  let withoutSibling = unlines (takeWhile (/= "{-# OPAQUE childSited #-}") (lines source))
  writeFile unfoldPath withoutSibling
  missing <- compile (PreparedProducts Nothing) Nothing >>= targetModule
  unless (any (isInfixOf "missing generated site-aware sibling" . srMessage) (pmSiteRejections missing)) $
    fail "missing typed sibling did not remain a source rejection"
  writeFile unfoldPath (T.unpack (T.replace ". Int -> input -> Maybe result" ". Bool -> input -> Maybe result" (T.pack source)))
  incompatible <- compile (PreparedProducts Nothing) Nothing >>= targetModule
  unless (any (isInfixOf "incompatible type" . srMessage) (pmSiteRejections incompatible)) $
    fail "incompatible typed sibling did not remain a source rejection"
  putStrLn "hydrated site siblings: 10 checks passed (native/exact, private native/exact, wrong interface owner, foreign surface unit, two missing owners, missing sibling, incompatible sibling)"

writeExactMetadataScope :: FilePath -> [(ExactIfaceArtifact, FilePath, String)] -> IO ()
writeExactMetadataScope path owners = writeExactMetadataScopeWithLexical path owners []

writeExactMetadataScopeWithLexical
  :: FilePath -> [(ExactIfaceArtifact, FilePath, String)]
  -> [(ExactIfaceArtifact, [(String, String)])] -> IO ()
writeExactMetadataScopeWithLexical path owners lexical = do
  let text = encodeString . T.pack
      identity (unit, name) = encodeListLen 2 <> text unit <> text name
      owner (artifact, packages, sha) = encodeListLen 7
        <> foldMap text [exactUnit artifact, exactModule artifact, exactPath artifact, exactSha256 artifact]
        <> encodeListLen 0 <> text packages <> text sha
      selected (artifact, requirements) = encodeListLen 2
        <> identity (exactUnit artifact, exactModule artifact)
        <> encodeListLen (fromIntegral (length requirements)) <> foldMap identity requirements
  BS.writeFile path (toStrictByteString (encodeListLen 7
    <> encodeString "TPEXACTSCOPE" <> encodeString "2"
    <> foldMap encodeString (replicate 2 (T.replicate 64 "0"))
    <> encodeListLen (fromIntegral (length owners)) <> foldMap owner owners
    <> encodeListLen (fromIntegral (length lexical)) <> foldMap selected lexical <> encodeListLen 0))

exactBashMetadata :: FilePath -> IO ()
exactBashMetadata effects = withTiming $ withScratch $ \work -> do
  let target = work </> "MetadataBashTarget.hs"
      scopePath = work </> "exact-scope.cbor"
      scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath }
  copyFile "test-source-boot/fixtures/MetadataBashTarget.hs" target
  writeExactMetadataScope scopePath []
  withResidentPipelineSelected [work, "lib", effects] $ \compile -> do
    normal <- compile CheckedEnvironment Set.empty GeneralCompile Nothing target [] Nothing
    (checked, diagnostics) <- captureDiagnostics $
      compile CheckedEnvironment Set.empty GeneralCompile (Just scope) target [] Nothing
    unless (fmap renderType (crResultType checked) == Just "Command"
        && fmap renderType (crResultType normal) == fmap renderType (crResultType checked)
        && "tidepool-checked-dependency-executable module=Tidepool.QQ.Bash bytecode=True object=False" `elem` lines diagnostics
        && "tidepool-checked-loaded-source module=Tidepool.QQ.Bash" `elem` lines diagnostics) $
      fail "exact bash metadata lost GHC load bytecode or signature parity"
    native <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [] Nothing
    unless (fmap renderType (prResultType (pprPipelineResult native)) == Just "Command"
        && dependencyCacheSafe (pprDependencies native)) $
      fail "exact bash native compilation lost its quote or input evidence"
  putStrLn "exact bash: GHC metadata parity, retained bytecode and native compilation passed"

-- Input identity follows checked imports even when authenticated candidates
-- cause the same source request to produce additional unused native products.
packageInputs :: IO ()
packageInputs = withScratch $ \work -> do
  forM_ ["OptionalRoot", "OptionalSupport", "OptionalAnchor", "OptionalWarmer", "OptionalWiredRoot", "OptionalWiredSupport", "OptionalPrimExt"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name ++ ".hs") (work </> name ++ ".hs")
  withResidentPipelineSelected [work] $ \compile -> do
    let root selection = compile selection Set.empty GeneralCompile Nothing
          (work </> "OptionalRoot.hs") [] Nothing
    cold <- root (PreparedProducts Nothing)
    unless ("OptionalSupport" `notElem` preparedNames cold) $
      fail "cold input fixture did not leave its unused support validation-only"
    warmer <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "OptionalWarmer.hs") [] Nothing
    unless (null (pprAcceptedCandidates warmer)
        && all (`elem` preparedNames warmer) ["OptionalAnchor", "OptionalSupport"]
        && all (\name -> Map.member (mkModuleName name) (pprFinalizedModules warmer))
          ["OptionalAnchor", "OptionalSupport"]) $
      fail "warmer input fixture lacks both fresh finalized candidate owners"
    writeGenuineCandidateManifestFor ["OptionalAnchor"] work
      (work </> "OptionalWarmer.hs") [work] warmer
    warm <- root (PreparedProducts (Just (manifest work)))
    unless (map candidateModule (pprAcceptedCandidates warm) == ["OptionalAnchor"]) $
      fail "warm input fixture did not admit its authenticated anchor candidate"
    unless ("OptionalSupport" `elem` preparedNames warm) $
      fail "warm input fixture did not retain the executable support product"
    coldBody <- proof work "cold" cold
    warmBody <- proof work "warm" warm
    verifyCollectivePackageProof work cold
    case coldBody of
      TList [TString "checked", TList owners, TList closure] -> do
        let direct = Set.fromList [(unit, name) | TList [_, _, TList entries] <- owners,
              TList [TString unit, TString name, _, _] <- entries]
            complete = Set.fromList [(unit, name) | TList [TString unit, TString name, _, _] <- closure]
        unless (Set.size complete > Set.size direct) $
          fail "input fixture did not exercise transitive installed interface dependencies"
      _ -> fail "ordinary checked input fixture lacks a complete package proof"
    unless (normalized cold == normalized warm && coldBody == warmBody) $
      fail "optional native availability changed checked compilation inputs"
    putStrLn ("package-input-products cold=" ++ show (preparedNames cold)
      ++ " candidate=" ++ show (preparedNames warm)
      ++ " checked=" ++ show (length (dependencyModules (pprDependencies cold))))
    -- Both offers share the complete fresh warmer capture. This fixture
    -- checks input identity; mixed fresh/retained reissuance has separate owners.
    writeGenuineCandidateManifestFor ["OptionalAnchor", "OptionalSupport"] work
      (work </> "OptionalWarmer.hs") [work] warmer
    reused <- root (PreparedProducts (Just (manifest work)))
    unless (map candidateModule (pprAcceptedCandidates reused) == ["OptionalAnchor", "OptionalSupport"]) $
      fail "input fixture did not exercise authenticated candidate hydration"
    reusedBody <- proof work "candidate" reused
    unless (normalized cold == normalized reused && coldBody == reusedBody) $
      fail "accepted candidate lost its checked direct package inputs"
    let incomplete = reused { pprPackageImports = Map.delete (mkModuleName "OptionalSupport")
          (pprPackageImports reused) }
    refused <- try (proof work "missing-owner" incomplete) :: IO (Either SomeException Term)
    case refused of
      Left _ -> pure ()
      Right _ -> fail "compiler input issuer accepted missing candidate package roots"
    let wiredRoot selection purpose = compile selection Set.empty purpose Nothing
          (work </> "OptionalWiredRoot.hs") [] Nothing
    wired <- wiredRoot (PreparedProducts Nothing) CertifyHomeProductsCompile
    wiredBody <- proof work "wired-fresh" wired
    case wiredBody of
      TList [TString "unsupported-wired", TList [TString "main", TString "OptionalWiredSupport"], _,
          TList [TList [TString "primitive", TString unit, TString name]]]
        | unit == T.pack (unitString (moduleUnit gHC_PRIM))
        , name == T.pack (moduleNameString (moduleName gHC_PRIM)) -> pure ()
      _ -> do
        observed <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
          (work </> "OptionalWiredSupport.hs") [] Nothing
        let imported = Map.keys (imp_mods (tcg_imports (prTargetTcGblEnv (pprPipelineResult observed))))
        fail ("direct compiler-provided input lacked its typed unsupported category: body="
          ++ take 512 (show wiredBody) ++ " checked-imports="
          ++ show [(dependencyModuleName node, map dependencyImportName (dependencyModuleImports node))
            | node <- dependencyModules (pprDependencies wired)]
          ++ " resolved-imports=" ++ show [(unitString (moduleUnit owner),
              moduleNameString (moduleName owner), owner == gHC_PRIM) | owner <- imported])
    writeGenuineCandidateManifestFor ["OptionalWiredSupport"] work
      (work </> "OptionalWiredRoot.hs") [work] wired
    wiredCandidates <- readModuleCandidates (manifest work) >>= either fail pure
    wiredCandidate <- case filter ((== "OptionalWiredSupport") . candidateModule) wiredCandidates of
      [candidate] -> pure candidate
      _ -> fail "wired input fixture lacks one genuinely issued support candidate"
    verifyPackageSidecar wiredCandidate (prHscEnv (pprPipelineResult wired))
    wiredReused <- wiredRoot (PreparedProducts (Just (manifest work))) GeneralCompile
    unless (map candidateModule (pprAcceptedCandidates wiredReused) == ["OptionalWiredSupport"]) $
      fail "wired input fixture did not exercise authenticated candidate hydration"
    wiredReusedBody <- proof work "wired-candidate" wiredReused
    unless (wiredBody == wiredReusedBody) $
      fail "candidate hydration changed compiler-provided input classification"
    primExt <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "OptionalPrimExt.hs") [] Nothing
    imports <- maybe (fail "primitive extension lacks checked import evidence") pure
      (Map.lookup (mkModuleName "OptionalPrimExt") (pprPackageImports primExt))
    unless (null (compilerProvided imports) && any ((== "GHC.Prim.Ext") . packageModule) (packageInterfaces imports)) $
      fail "primitive extension was confused with the compiler-provided primitive"
    proof work "primitive-extension" primExt >>= \case
      TList [TString "checked", _, _] -> pure ()
      _ -> fail "real primitive-extension interface did not retain a complete input proof"
  putStrLn "package inputs: cold/warm native divergence, identical checked closure, candidate roots, omission refusal and wired fresh/candidate refusal passed"
  where
    normalized result = renderDependencyEvidence ((pprDependencies result)
      { dependencyModules = sortOn (\node -> (dependencyModuleUnit node, dependencyModuleName node))
          [node { dependencyModuleProduct = ProductInterfaceOnly }
          | node <- dependencyModules (pprDependencies result)] })
    proof work label result = do
      let directory = work </> ("input-proof-" ++ label)
          evidence = pprDependencies result
      createDirectory directory
      writeFile (directory </> "dependencies.json") (renderDependencyEvidence evidence)
      writeCompileInputProof directory (prHscEnv (pprPipelineResult result)) evidence
        (pprPackageImports result)
      bytes <- BS.readFile (directory </> "compiler-inputs.cbor")
      term <- either (fail . show) (pure . snd)
        (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
      case term of
        TList [TString "TPCINPUT", TInt 1, _, body] -> pure body
        _ -> fail "compiler input producer returned another proof category"

-- Reuse real compiled owner bytes and real installed roots. Copies plus an
-- isolated finder make mutation tests local without changing the Nix packages.
verifyCollectivePackageProof :: FilePath -> PreparedPipelineResult -> IO ()
verifyCollectivePackageProof work prepared = withTiming $ do
  let producer = prHscEnv (pprPipelineResult prepared)
      installed = Set.toAscList (Set.fromList
        (concatMap packageInterfaces (Map.elems (pprPackageImports prepared))))
  selected <- case installed of
    first:second:_ -> pure [first,second]
    _ -> fail "package proof fixture needs two distinct installed owners"
  finder <- initFinderCache
  mutable <- forM (zip [0 :: Int ..] selected) $ \(index, root) -> do
    let owner = mkModule (stringToUnit (packageUnit root)) (mkModuleName (packageModule root))
        path = work </> ("package-proof-root-" ++ show index ++ ".hi")
    (_, location) <- readExactInterface producer owner >>= either (fail . show) pure
    bytes <- BS.readFile (packagePath root)
    BS.writeFile path bytes
    addModuleToFinder finder (GWIB owner NotBoot) (location { ml_hi_file = path })
    pure (root { packagePath = path }, location { ml_hi_file = path }, bytes)
  (root, location, rootBytes) <- case mutable of
    first:_ -> pure first
    _ -> fail "package proof lost its mutable installed owners"
  resolverCalls <- newIORef (0 :: Int)
  let observedFinder = finder { lookupFinderCache = \owner -> do
        modifyIORef' resolverCalls (+1)
        lookupFinderCache finder owner }
      environment = producer { hsc_FC = observedFinder }
      roots = [selectedRoot | (selectedRoot,_,_) <- mutable]
      firstRoot = root
      count :: String -> String -> Integer
      count = counterTotal
      requireRight label = \case
        Right () -> pure ()
        Left reason -> fail (label ++ " refused: " ++ reason)
      requireLeft label = \case
        Left _ -> pure ()
        Right () -> fail (label ++ " admitted changed proof inputs")
  interface <- maybe (fail "package proof lacks the compiled owning interface") pure
    (Map.lookup (mkModuleName "OptionalRoot") (pprProductInterfaces prepared))
  let ownerPath = work </> "package-proof-owner.hi"
  writeBinIface (targetProfile (hsc_dflags producer)) QuietBinIFace NormalCompression ownerPath interface
  ownerBytes <- BS.readFile ownerPath
  let owner = mi_module interface
      artifact = ExactIfaceArtifact (unitString (moduleUnit owner))
        (moduleNameString (moduleName owner)) ownerPath (digest ownerBytes) []
      sidecar label iface evidence = do
        let path = work </> ("package-proof-" ++ label ++ ".packages")
            bytes = encodePackageImports iface (PackageImportEvidence evidence [])
        BS.writeFile path bytes
        pure (iface,path,digest bytes)
  fanout <- forM [1 :: Int .. 64] $ \index -> do
    let path = work </> ("package-proof-owner-" ++ show index ++ ".hi")
    BS.writeFile path ownerBytes
    sidecar ("fanout-" ++ show index) (artifact { exactPath = path }) [firstRoot]
  firstWitness <- case fanout of
    first:_ -> pure first
    _ -> fail "package proof lost its fanout owner fixtures"
  forM_ [1,8,64] $ \size -> do
    writeIORef resolverCalls 0
    (result, diagnostics) <- captureDiagnostics (revalidatePackageImports environment (take size fanout))
    requireRight "overlapping package roots" result
    calls <- readIORef resolverCalls
    unless (calls == 1
      && count "package_proof.authenticated_sidecars" diagnostics == fromIntegral size
      && count "package_proof.staged_references" diagnostics == fromIntegral size
      && count "package_proof.staged_full_witnesses" diagnostics == 1) $
      fail "collective proof repeated resolution or skipped owning authentication"
    putStrLn ("package proof fanout=" ++ show size ++ " full_witnesses=1 resolver_calls=" ++ show calls)
  separate <- forM (zip [1 :: Int ..] roots) $ \(index, selectedRoot) ->
    sidecar ("distinct-" ++ show index) artifact [selectedRoot]
  writeIORef resolverCalls 0
  revalidatePackageImports environment separate >>= requireRight "distinct package roots"
  readIORef resolverCalls >>= \calls -> unless (calls == 2) (fail "distinct owners were collapsed")
  copied <- sidecar "same-owner-other-path" artifact [firstRoot { packagePath = ownerPath }]
  changedSha <- sidecar "same-owner-other-sha" artifact [firstRoot { packageSha256 = replicate 64 '0' }]
  forM_ [copied,changedSha] $ \conflict -> do
    writeIORef resolverCalls 0
    revalidatePackageImports environment [firstWitness,conflict] >>= requireLeft "conflicting full witness"
    readIORef resolverCalls >>= \calls -> unless (calls == 0) (fail "conflicting proof reached package resolution")
  malformed <- forM [("duplicate-root",[firstRoot,firstRoot]),
    ("ambiguous-owner",[firstRoot,firstRoot { packagePath = ownerPath }]),
    ("relative-path",[firstRoot { packagePath = "relative.hi" }])] $ \(label,evidence) ->
      sidecar label artifact evidence
  forM_ malformed $ \witness ->
    revalidatePackageImports environment [witness] >>= requireLeft "malformed package sidecar"
  otherUnit <- sidecar "same-content-other-unit" artifact
    [firstRoot { packageUnit = "not-the-selected-package-unit" }]
  (unitResult, unitDiagnostics) <- captureDiagnostics
    (revalidatePackageImports environment [firstWitness,otherUnit])
  requireLeft "unresolved equal-content owner" unitResult
  unless (count "package_proof.staged_full_witnesses" unitDiagnostics == 2) $
    fail "equal-content owners in different units were collapsed during staging"
  let proof = take 8 fanout
  (lastArtifact,lastSidecar,_) <- case reverse proof of
    lastWitness:_ -> pure lastWitness
    _ -> fail "package proof lost its mutation fixtures"
  sidecarBytes <- BS.readFile lastSidecar
  forM_ [("sidecar",lastSidecar,sidecarBytes),("owning interface",exactPath lastArtifact,ownerBytes)] $ \(label,path,bytes) -> do
    stamp <- getModificationTime path
    BS.writeFile path "changed proof bytes"
    setModificationTime path stamp
    writeIORef resolverCalls 0
    revalidatePackageImports environment proof >>= requireLeft label
    readIORef resolverCalls >>= \calls -> unless (calls == 0) (fail "partially authenticated owners reached resolution")
    retained <- BS.readFile path
    unless (retained == "changed proof bytes") (fail "proof refusal changed its input")
    BS.writeFile path bytes
    revalidatePackageImports environment proof >>= requireRight ("restored " ++ label)
  stamp <- getModificationTime (packagePath root)
  BS.writeFile (packagePath root) "changed selected root"
  setModificationTime (packagePath root) stamp
  revalidatePackageImports environment proof >>= requireLeft "same-path package mutation"
  removeFile (packagePath root)
  revalidatePackageImports environment proof >>= requireLeft "missing selected root"
  BS.writeFile (packagePath root) rootBytes
  revalidatePackageImports environment proof >>= requireRight "restored selected root"
  let alternate = work </> "package-proof-alternate.hi"
      installedOwner = mkModule (stringToUnit (packageUnit root)) (mkModuleName (packageModule root))
  BS.writeFile alternate rootBytes
  alternateExpected <- sidecar "alternate-expected" artifact [root { packagePath = alternate }]
  revalidatePackageImports environment [alternateExpected] >>= requireLeft "alternate expected path"
  addModuleToFinder finder (GWIB installedOwner NotBoot) (location { ml_hi_file = alternate })
  revalidatePackageImports environment proof >>= requireLeft "changed current finder selection"
  addModuleToFinder finder (GWIB installedOwner NotBoot) location
  entered <- newEmptyMVar
  gate <- newEmptyMVar :: IO (MVar ())
  settled <- newEmptyMVar
  cancellationCalls <- newIORef (0 :: Int)
  let blocked = environment { hsc_FC = finder { lookupFinderCache = \selectedOwner -> do
        modifyIORef' cancellationCalls (+1)
        calls <- readIORef cancellationCalls
        when (calls == 2) $ do
          putMVar entered ()
          takeMVar gate
        lookupFinderCache finder selectedOwner } }
  bracket (forkIO ((try (revalidatePackageImports blocked separate) :: IO (Either SomeException (Either String ()))) >>= putMVar settled)) killThread $ \thread -> do
    timeout 1000000 (takeMVar entered) >>= \case
      Nothing -> fail "package cancellation fixture never reached its second root"
      Just () -> pure ()
    readIORef cancellationCalls >>= \calls ->
      unless (calls == 2) (fail "package cancellation did not follow one validated root")
    killThread thread
    timeout 1000000 (takeMVar settled) >>= \case
      Just (Left exception) | fromException exception == Just ThreadKilled -> pure ()
      _ -> fail "collective proof swallowed cancellation or returned success"
  BS.writeFile (packagePath root) "changed after cancellation"
  revalidatePackageImports environment proof >>= requireLeft "next proof after cancellation"
  BS.writeFile (packagePath root) rootBytes
  revalidatePackageImports environment proof >>= requireRight "restored proof after cancellation"
  BS.writeFile alternate rootBytes
  renameFile alternate (packagePath root)
  revalidatePackageImports environment proof >>= requireRight "same-byte replacement"
  putStrLn "collective-package-proof scenarios=23 assertion_groups=7 status=passed"

-- Mutated fixtures retain their bytes; only the owning sidecar reader decides
-- whether old, cross-owner or unknown compiler facts can be admitted.
verifyPackageSidecar :: ModuleCandidate -> HscEnv -> IO ()
verifyPackageSidecar candidate environment = do
  let path = candidateInterface candidate
      packages = candidatePackageImports candidate
      artifact = ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
        path (candidateInterfaceSha256 candidate) (candidateInterfaceRequirements candidate)
  original <- BS.readFile packages
  unless (digest original == candidatePackageImportsSha256 candidate) $
    fail "wired candidate package sidecar changed after issuance"
  term <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict original))
  case term of
    TList [magic, TString "2", owner, installed, TList [TList [category, unit, _]]] -> do
      let variants =
            [ ("legacy", TList [magic, TString "1", owner, installed])
            , ("unknown", TList [magic, TString "2", owner, installed,
                TList [TList [category, unit, TString "GHC.Prim.Ext"]]]) ]
      forM_ variants $ \(label, changed) -> do
        let bytes = toStrictByteString (encodeTerm changed)
            changedPath = packages ++ "." ++ label
        BS.writeFile changedPath bytes
        readPackageImports changedPath (digest bytes) artifact >>= \case
          Left _ -> pure ()
          Right _ -> fail ("sidecar admitted " ++ label ++ " compiler-provided evidence")
        revalidatePackageImports environment [(artifact, changedPath, digest bytes)] >>= \case
          Left _ -> pure ()
          Right _ -> fail ("collective proof admitted " ++ label ++ " compiler-provided evidence")
        retained <- BS.readFile changedPath
        unless (retained == bytes) (fail "sidecar refusal changed retained input bytes")
      readPackageImports packages (digest original) (artifact { exactModule = "WrongOwner" }) >>= \case
        Left _ -> pure ()
        Right _ -> fail "sidecar admitted another checked interface owner"
      readPackageImports packages (digest original) (artifact { exactSha256 = replicate 64 '0' }) >>= \case
        Left _ -> pure ()
        Right _ -> fail "sidecar admitted another checked interface digest"
      forM_ [artifact { exactModule = "WrongOwner" }, artifact { exactSha256 = replicate 64 '0' }] $ \changed ->
        revalidatePackageImports environment [(changed,packages,digest original)] >>= \case
          Left _ -> pure ()
          Right _ -> fail "collective proof admitted changed owning interface identity"
      putStrLn "collective-package-sidecar-negative scenarios=4 status=passed"
    _ -> fail "wired candidate sidecar lacked its typed v2 compiler evidence"

selectedHomeInstanceEdges :: IO ()
selectedHomeInstanceEdges = withScratch $ \work -> do
  forM_ ["InstanceOwner", "InstanceRelay", "InstanceConsumer"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name ++ ".hs") (work </> name ++ ".hs")
  cold <- runPipelineSelected (PreparedProducts Nothing)
    (work </> "InstanceConsumer.hs") [work]
  let evidence = pprDependencies cold
      producer = prHscEnv (pprPipelineResult cold)
      consumers = [node | node@(ModuleNode _ summary) <- mgModSummaries' (hsc_mod_graph producer)
        , ms_mod_name summary == mkModuleName "InstanceConsumer"]
  verifyRetainedPackageWitness producer evidence
  lexical <- forM ["InstanceOwner", "InstanceRelay"] $ \name -> do
    requirements <- either fail pure (selectedHomeRequirements evidence "main" name)
    pure (ExactIfaceArtifact "main" name (work </> name ++ ".hi") "" requirements, requirements)
  unless (map snd lexical == [[], [("main", "InstanceOwner")]]) $
    fail "selected home receipt omitted the transitive instance owner or admitted a package import"
  let altered = evidence { dependencyModules =
        [node { dependencyModuleSource = "missing-owner.hs" }
        | node <- dependencyModules evidence] }
  case selectedHomeRequirements altered "main" "InstanceRelay" of
    Left _ -> pure ()
    Right _ -> fail "selected home receipt accepted an unmatched source owner"
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    rebuilt <- liftIO (installExactLexicalGraph (mkModuleGraph consumers) lexical noCheckedValueImports producer)
    setSession =<< either (liftIO . fail) pure rebuilt
    case consumers of
      [ModuleNode _ summary] -> do
        _ <- parseModule summary >>= typecheckModule
        pure ()
      _ -> liftIO (fail "instance graph lacks one source consumer")
  putStrLn "selected home instance edges: transitive instance, package exclusion and owner mismatch passed"

verifyRetainedPackageWitness :: HscEnv -> DependencyEvidence -> IO ()
verifyRetainedPackageWitness producer evidence = do
  let package = SymbolIdentity "ghc-internal" "GHC.Internal.Base" "value" "map" Nothing
      home = SymbolIdentity "main" "InstanceConsumer" "value" "result" Nothing
      global identity generation = GlobalDecl identity LiftedRefRep Nothing False (Just generation)
      program globals = WireProgram
        { programEnvelope = ProgramEnvelope schemaVersion "test" "matched" executionAbiVersion
            (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" [])
        , programSignatures = [], programGlobals = globals, programConstructors = []
        , programOperations = [], programBindings = [], programEntry = ValueId 0
        , programTypes = [], programSites = [], programVerbSites = [], programJsonLayout = Nothing }
      encode globals = encodeCertifiedProducts producer OrdinaryExecutionSource Map.empty (emptyFinalizedModuleArtifacts producer) [] Nothing []
        [("target", program globals)] evidence "" ""
  withTiming $ do
    let repeated = replicate 1000 (global package 0)
          ++ [global (package { symbolOccurrence = "id" }) 0]
        certify = do
          started <- getMonotonicTimeNSec
          certified <- captureDiagnostics $ do
            bytes <- encode repeated >>= either fail pure
            _ <- evaluate (BS.length bytes)
            pure bytes
          finished <- getMonotonicTimeNSec
          putStrLn ("package certification requests=1001 wall_ms="
            ++ show ((finished - started) `div` 1000000))
          pure certified
    (first, firstLog) <- certify
    (second, secondLog) <- certify
    retainDiagnosticOracle "repeated-package-first" firstLog
    retainDiagnosticOracle "repeated-package-second" secondLog
    unless (first == second) $ fail "repeated package certification changed its wire evidence"
    retainByteOracle "repeated-package" first
    repeatedTerm <- either (fail . show) (pure . snd)
      (deserialiseFromBytes decodeTerm (BSL.fromStrict first))
    case repeatedTerm of
      TList [TString "TPCERT", TInt 8, TList [],
          TList [TList [TString "target", TList references]],
          TList _, TList globals, _, TList [TString "ordinary"], _]
        | length references == 1001
            && length globals == 2 -> unless (sameRepeatedReferences references globals) $
              fail ("repeated package witnesses lost canonical deduplication or reference indices: "
                ++ show (take 3 references, drop 999 references,
                  map witnessOccurrence globals))
      _ -> fail "repeated package witnesses lost canonical deduplication or reference indices"
    forM_ [firstLog, secondLog] $ \diagnostics -> do
      unless (count "certified_package_global_requests" diagnostics == 1001
          && count "certified_package_owner_loads" diagnostics == 1
          && count "certified_package_catalog_builds" diagnostics == 1
          && count "certified_package_owner_revalidations" diagnostics == 1) $
        fail "package certification rebuilt one owner for repeated or distinct symbols"
      let actual = count "certified_package_revalidation_bytes" diagnostics
          repeatedBytes = count "certified_package_reference_bytes" diagnostics
      unless (actual > 0 && repeatedBytes == 1001 * actual) $
        fail "package certification lost its counted duplicate-read evidence"
      putStrLn ("package certification requests=1001 owner_loads=1 final_reads=1"
        ++ " final_bytes=" ++ show actual ++ " former_final_bytes=" ++ show repeatedBytes)
    encode [global package 0, global (package { symbolOccurrence = "$missingSibling" }) 0] >>= \case
      Left _ -> pure ()
      Right _ -> fail "checked package interface authorized an absent sibling"
  verifyChangedPackageInterface producer package evidence program global
  bytes <- encode [global package 0, global home 7] >>= either fail pure
  retainByteOracle "mixed-retained" bytes
  owners <- certificateOwners bytes
  unless (any (\case
      TList [TString "retained-package", TString "ghc-internal", TString "GHC.Internal.Base", TString packageHash, _, TInt 0] -> T.length packageHash == 64
      _ -> False) owners
      && any (\case TList [TString "retained", _, TInt 7] -> True; _ -> False) owners) $
    fail "positive package evidence lost its lease or confused a retained home owner"
  encode [global (package { symbolModule = "Missing.Package.Owner" }) 0] >>= \case
    Left _ -> pure ()
    Right _ -> fail "retained package owner without loaded interface evidence was accepted"
  let constructor = SymbolIdentity "ghc-internal" "GHC.Internal.Stack.Types" "value" "EmptyCallStack" Nothing
      synthetic = constructor { symbolOccurrence = "$internalSyntheticPackageSibling" }
      localProgram identities = (program [])
        { programConstructors = [ConstructorDecl
            (constructor { symbolNamespace = "constructor" })
            (constructor { symbolNamespace = "type", symbolOccurrence = "CallStack" })
            LiftedRefRep [] [] (CheckedLayout [] 8 0 []) 0 2 0]
        , programBindings = [NonRecursive (TopBinding identity
            (HeapBinding (ValueId (fromIntegral index)) (Constructor (ConstructorId 0) [])))
            | (index, identity) <- zip [0 :: Int ..] identities] }
      encodeProgram target = encodeCertifiedProducts producer OrdinaryExecutionSource Map.empty (emptyFinalizedModuleArtifacts producer) [] Nothing []
        [("target", target)] evidence "" ""
      decode bytes' = either (fail . show) (pure . snd)
        (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes'))
  let wired = preparedRootIdentity (dataConWorkId intDataCon)
      identities = [package, package { symbolOccurrence = "id" },
        package { symbolOccurrence = "fmap" }, constructor, wired]
  lookupStarted <- getMonotonicTimeNSec
  forM_ identities $ \identity -> do
    (identifier, _) <- resolvePackageGlobal producer identity >>= either fail pure
    _ <- evaluate (idName identifier)
    unless (preparedRootIdentity identifier == identity) $
      fail "package catalog selected a noncanonical defining Name"
  lookupFinished <- getMonotonicTimeNSec
  putStrLn ("standalone package lookups requests=5 wall_ms="
    ++ show ((lookupFinished - lookupStarted) `div` 1000000))
  let baseOwner = mkModule (stringToUnit "ghc-internal") (mkModuleName "GHC.Internal.Base")
      implicit = package { symbolOccurrence = "fmap" }
  (baseInterface, _) <- readExactInterface producer baseOwner >>= either (fail . show) pure
  unless (any (\(_, declaration) -> mkVarOcc "fmap" `elem` ifaceDeclImplicitBndrs declaration)
      (mi_decls baseInterface)) $
    fail "package catalog fixture does not demand an implicit class binder"
  implicitName <- case Set.toList (Set.fromList
      [name | available <- mi_exports baseInterface, name <- availNames available
        , nameModule_maybe name == Just baseOwner, nameOccName name == mkVarOcc "fmap"]) of
    [name] -> pure name
    _ -> fail "implicit class binder lacks one canonical exported Name"
  forM_ [identities, reverse identities] $ \ordered -> do
    forM_ ordered $ \identity -> do
      (identifier, _) <- resolvePackageGlobal producer identity >>= either fail pure
      _ <- evaluate (idName identifier)
      when (identity == implicit) $ unless (idName identifier == implicitName) $
        fail "package catalog minted another Unique for an implicit class binder"
      when (identity == wired) $ unless (idName identifier == idName (dataConWorkId intDataCon)) $
        fail "package catalog lost the wired-in defining Name"
  withTiming $ do
    let repeatedImplicit = replicate 32 implicit
        ordered = identities ++ repeatedImplicit
    (forward, forwardLog) <- captureDiagnostics
      (encode (map (`global` 0) ordered) >>= either fail pure)
    (backward, backwardLog) <- captureDiagnostics
      (encode (map (`global` 0) (reverse ordered)) >>= either fail pure)
    retainDiagnosticOracle "implicit-forward" forwardLog
    retainDiagnosticOracle "implicit-backward" backwardLog
    forwardTerm <- decode forward
    backwardTerm <- decode backward
    restored <- case backwardTerm of
      TList [magic, version, modules, TList [TList [name, TList references]], packages, globals, envelope, recipe, coordinates] ->
        pure (TList [magic, version, modules, TList [TList [name, TList (reverse references)]], packages, globals, envelope, recipe, coordinates])
      _ -> fail "package catalog fixture lacks its target reference inventory"
    unless (forwardTerm == restored) $
      fail "package lookup order changed canonical witness inventory or reference order"
    forM_ [forwardLog, backwardLog] $ \diagnostics ->
      unless (count "certified_package_global_requests" diagnostics == 37
          && count "certified_package_owner_loads" diagnostics == 3
          && count "certified_package_catalog_builds" diagnostics == 3
          && count "certified_package_owner_revalidations" diagnostics == 3) $
        fail "repeated implicit lookup escaped its certification-owned catalog capture"
  withTiming $ do
    (_, diagnostics) <- captureDiagnostics
      (encode (map (`global` 0) identities) >>= either fail pure)
    retainDiagnosticOracle "five-package-identities" diagnostics
    unless (count "certified_package_global_requests" diagnostics == 5
        && count "certified_package_owner_loads" diagnostics == 3
        && count "certified_package_catalog_builds" diagnostics == 3
        && count "certified_package_owner_revalidations" diagnostics == 3) $
      fail "package catalog did not share ordinary, class, constructor and wired owner captures"
  forM_ [package { symbolModule = "GHC.Internal.Prelude" },
      package { symbolNamespace = "type" }] $ \identity -> do
    encode [global identity 0] >>= \case
      Left _ -> pure ()
      Right _ -> fail "package catalog admitted a reexport owner or another namespace"
    resolvePackageGlobal producer identity >>= \case
      Left _ -> pure ()
      Right _ -> fail "standalone package lookup admitted a reexport owner or another namespace"
  localBytes <- encodeProgram (localProgram [constructor, synthetic]) >>= either fail pure
  retainByteOracle "local-package" localBytes
  local <- decode localBytes
  case local of
    TList [TString "TPCERT", TInt 8, _, _, TList [TList
      [TString "ghc-internal", TString "GHC.Internal.Stack.Types", TString _, TString sha]], TList [], _, TList [TString "ordinary"], _]
      | T.length sha == 64 -> pure ()
    _ -> fail "local package constructor without incoming globals lacks exact interface evidence"
  internal <- encodeProgram (localProgram [synthetic]) >>= either fail decode
  case internal of
    TList [TString "TPCERT", TInt 8, _, _, TList [], TList [], _, TList [TString "ordinary"], _] -> pure ()
    _ -> fail "noncanonical internal package helper supplied external interface authority"
  encodeProgram ((localProgram [constructor, synthetic])
    { programGlobals = [global synthetic 0] }) >>= \case
      Left _ -> pure ()
      Right _ -> fail "witnessed package module authorized a noncanonical retained sibling"
  putStrLn "local package exports: canonical constructor without globals, internal helper and synthetic demand refusal passed"
  putStrLn "retained package witnesses: authenticated map/gen0, home/gen7 and missing package refusal passed"

  where
    count :: String -> String -> Integer
    count = counterTotal
    retainByteOracle label bytes =
      lookupEnv "TIDEPOOL_CERTIFICATE_BYTE_ORACLE" >>= mapM_ (\prefix ->
        BS.writeFile (prefix ++ "-" ++ label ++ ".cbor") bytes)
    retainDiagnosticOracle label diagnostics =
      lookupEnv "TIDEPOOL_CERTIFICATE_BYTE_ORACLE" >>= mapM_ (\prefix ->
        writeFile (prefix ++ "-" ++ label ++ ".log") diagnostics)
    sameRepeatedReferences references globals = case (references, globals) of
      (TInt firstIndex : rest, [firstWitness, secondWitness]) ->
        all (== TInt firstIndex) (take 999 rest)
          && case (drop 999 rest, witnessOccurrence firstWitness, witnessOccurrence secondWitness) of
            ([TInt secondIndex], Just firstOccurrence, Just secondOccurrence) ->
              secondIndex /= firstIndex
                && occurrenceAt firstIndex firstOccurrence secondOccurrence == "map"
                && occurrenceAt secondIndex firstOccurrence secondOccurrence == "id"
            _ -> False
      _ -> False
    witnessOccurrence = \case
      TList [TList [_, _, _, TString occurrence, _], _, _, _, _] -> Just occurrence
      _ -> Nothing
    occurrenceAt 0 firstOccurrence _ = firstOccurrence
    occurrenceAt 1 _ secondOccurrence = secondOccurrence
    occurrenceAt _ _ _ = ""

-- A mutable installed interface exercises the same environment on successive
-- certifications. No successful owner selection may survive into the next one.
verifyChangedPackageInterface :: HscEnv -> SymbolIdentity -> DependencyEvidence
  -> ([GlobalDecl] -> WireProgram) -> (SymbolIdentity -> Word64 -> GlobalDecl) -> IO ()
verifyChangedPackageInterface producer identity evidence program global = withScratch $ \work -> do
  let owner = mkModule (stringToUnit (T.unpack (symbolUnit identity)))
        (mkModuleName (T.unpack (symbolModule identity)))
      path = work </> "mutable-package.hi"
  (_, location) <- readExactInterface producer owner >>= either (fail . show) pure
  original <- BS.readFile (ml_hi_file location)
  BS.writeFile path original
  finder <- initFinderCache
  addModuleToFinder finder (GWIB owner NotBoot) (location { ml_hi_file = path })
  let environment = producer { hsc_FC = finder }
      encode = encodeCertifiedProducts environment OrdinaryExecutionSource Map.empty (emptyFinalizedModuleArtifacts environment) [] Nothing []
        [("target", program [global identity 0, global identity 0])] evidence "" ""
  first <- encode >>= either fail pure
  (firstId, _) <- resolvePackageGlobal environment identity >>= either fail pure
  BS.writeFile path "invalid interface"
  encode >>= \case
    Left _ -> pure ()
    Right _ -> fail "package certification reused an owner after its interface changed"
  resolvePackageGlobal environment identity >>= \case
    Left _ -> pure ()
    Right _ -> fail "standalone package lookup reused a changed interface"
  BS.writeFile path original
  restored <- encode >>= either fail pure
  unless (restored == first) $ fail "restored package interface did not recover certification"
  (restoredId, _) <- resolvePackageGlobal environment identity >>= either fail pure
  unless (idName restoredId == idName firstId) $
    fail "restored standalone package lookup changed its canonical Name"
  putStrLn "package certification: changed interface refused and restored bytes recovered"

withTiming :: IO a -> IO a
withTiming action = bracket (lookupEnv "TIDEPOOL_TIMING") restore $ \_ ->
  setEnv "TIDEPOOL_TIMING" "1" >> action
  where restore = maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING")

captureDiagnostics :: IO a -> IO (a, String)
captureDiagnostics action = do
  temporary <- getTemporaryDirectory
  bracket (openTempFile temporary "package-certification.log")
    (\(path, output) -> hClose output >> removeFile path) $ \(_, output) -> do
      hFlush stderr
      result <- bracket (hDuplicate stderr) hClose $ \saved ->
        (hDuplicateTo output stderr >> action)
          `finally` (hFlush stderr >> hDuplicateTo saved stderr)
      hSeek output AbsoluteSeek 0
      diagnostics <- BSC.unpack <$> BS.hGetContents output
      pure (result, diagnostics)

resolutionPaths :: IO ()
resolutionPaths = do
  original <- getCurrentDirectory
  let fixtureRoot = original </> "test-source-boot" </> "fixtures"
  withScratch $ \work -> withCurrentDirectory work $ do
    forM_ ["InstanceOwner", "InstanceRelay", "InstanceConsumer"] $ \name ->
      copyFile (fixtureRoot </> name ++ ".hs") (work </> name ++ ".hs")
    createDirectory (work </> "padding")
    createDirectory (work </> "later")
    cwd <- getCurrentDirectory
    let parentRoot = "padding" </> ".."
        roots = [parentRoot, ".", work, "later", parentRoot]
        target = work </> "InstanceConsumer.hs"
        extensions = [".hs", ".lhs", ".hsig", ".lhsig"]
        expectedNegative =
          [normalise (root </> "Prelude" ++ extension)
          | root <- [cwd, cwd </> parentRoot, cwd </> "later"], extension <- extensions]
        resolution name evidence = case
          [row | row <- dependencyResolutions evidence
            , dependencyResolutionQualifier row == DependencyUnqualified
            , dependencyResolutionModule row == name, not (dependencyResolutionBoot row)] of
          [row] -> pure row
          _ -> fail ("resolution path fixture lacks one import: " ++ name)
        resolutionBytes evidence = renderDependencyEvidence (evidence
          {dependencySources = [], dependencyModules = [], dependencyPackages = []})
        verify evidence = do
          package <- resolution "Prelude" evidence
          unless (isNothing (dependencyResolutionSelected package)
              && dependencyResolutionCandidates package == expectedNegative) $
            fail "root aliases changed ordered negative package candidates or collapsed parent segments"
          home <- resolution "InstanceRelay" evidence
          let chosen = normalise (cwd </> "InstanceRelay.hs")
          unless (dependencyResolutionSelected home == Just chosen
              && dependencyResolutionCandidates home == [chosen]) $
            fail "selected source cutoff retained a lower root or another extension"
    cold <- runPipelineSelected (PreparedProducts Nothing) target roots
    verify (pprDependencies cold)
    writeManifestFor ["InstanceOwner"] work cold
    withResidentPipelineSelected roots $ \compile -> do
      let reuse = compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
            Nothing target [] Nothing
      warm <- reuse
      verify (pprDependencies warm)
      unless (resolutionBytes (pprDependencies warm) == resolutionBytes (pprDependencies cold)) $
        fail "warm reuse changed serialized ordered resolution evidence"
      unless (Set.fromList (map candidateModule (pprAcceptedCandidates warm))
          == Set.fromList ["InstanceOwner"]) $
        fail "root alias fixture failed to admit its unchanged candidate owners"
      let source = work </> "Prelude.hs"
      (do writeFile source (unlines ["{-# LANGUAGE PackageImports #-}"
            , "module Prelude (module PackagePrelude) where"
            , "import \"base\" Prelude as PackagePrelude"])
          reuse >>= requireRefused "new home selection with root aliases")
        `finally` removeFile source
      restored <- reuse
      verify (pprDependencies restored)
      unless (resolutionBytes (pprDependencies restored) == resolutionBytes (pprDependencies cold)) $
        fail "restored selection changed serialized ordered resolution evidence"
      unless (length (pprAcceptedCandidates restored) == 1) $
        fail "restored negative root evidence did not recover unchanged owners"
    putStrLn "resolution-paths scenarios=4: cold/warm ordered aliases and parent roots, new home shadow refusal, restored selection passed"

-- The fixed two-module SOURCE SCC is surrounded by ordinary candidate
-- products. A separate case makes Independent1 a real ordinary+boot input;
-- it must stay in GHC's fresh-load closure while the other members stay out.
mixedGraph :: Bool -> Int -> IO ()
mixedGraph required count = withScratch $ \work -> do
  forM_ ["CacheEven.hs", "CacheEven.hs-boot", "CacheOdd.hs", "CacheEntry.hs"] $ \file ->
    copyFile ("test-source-boot/fixtures" </> file) (work </> file)
  let independent = ["Independent" ++ show index | index <- [1 .. count]]
      expected = ["CacheEven", "CacheOdd"] ++ independent
  forM_ independent $ \name -> writeFile (work </> name ++ ".hs") (unlines
    ["module " ++ name ++ " where", "data Token = Token", "value :: Int", "value = 1"])
  entry <- BSC.unpack <$> BS.readFile (work </> "CacheEntry.hs")
  writeFile (work </> "CacheEntry.hs") (unlines
    (take 4 (lines entry) ++ ["import qualified " ++ name | name <- independent]
      ++ drop 4 (lines entry)) ++ "\nindependentTotal :: Int\nindependentTotal = "
      ++ foldr1 (\left right -> left ++ " + " ++ right) [name ++ ".value" | name <- independent] ++ "\n")
  if required
    then forM_ ["CacheEven.hs", "CacheEven.hs-boot"] $ \file -> do
      content <- BSC.unpack <$> BS.readFile (work </> file)
      let body = unlines (take 3 (lines content) ++ ["import qualified Independent1"] ++ drop 3 (lines content))
          anchor = "\nanchor :: Independent1.Token\n"
            ++ if file == "CacheEven.hs" then "anchor = Independent1.Token\n" else ""
      writeFile (work </> file) (body ++ anchor)
    else pure ()
  cold <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
    (work </> "CacheEntry.hs") [work] (Just (work </> "build-products"))
  unless (Set.fromList (preparedNames cold) == Set.fromList ("CacheEntry" : expected)) $
    fail "mixed SOURCE producer omitted an original module"
  writeGenuineCandidateManifestFor expected work (work </> "CacheEntry.hs") [work] cold
  withResidentPipelineSelected [work] $ \compile ->
    forM_ [1 .. 3 :: Int] $ \sample -> do
      hPutStrLn stderr ("mixed-source-start independent=" ++ show count
        ++ " required=" ++ show required ++ " sample=" ++ show sample)
      start <- getMonotonicTimeNSec
      result <- compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
        Nothing (work </> "CacheEntry.hs") [] Nothing
      end <- getMonotonicTimeNSec
      requireMixed count result
      let evidence = pprDependencies result
          negative = length [resolution | resolution <- dependencyResolutions evidence
            , dependencyResolutionSelected resolution == Nothing
            , not (null (dependencyResolutionCandidates resolution))]
      unless (negative > 0) (fail "mixed reuse omitted negative home lookup witnesses")
      hPutStrLn stderr ("mixed-source-result independent=" ++ show count
        ++ " required=" ++ show required ++ " sample=" ++ show sample
        ++ " elapsed_ns=" ++ show (end - start)
        ++ " accepted=" ++ show (length (pprAcceptedCandidates result))
        ++ " extracted=" ++ show (length (pprModules result))
        ++ " negative_lookups=" ++ show negative)
    -- Refusals run on the smallest mixed graph and are outside measurement.
  if count == 1 then do
    exerciseRefusalsWith (\_ -> requireMixed count) work (reuseFresh work)
    exerciseFamilyRefusal work
    exerciseIndependentDrift work
    exerciseNegativeHomeSelection work
    else pure ()
  executable <- getExecutablePath
  (exit, _, errors) <- readProcessWithExitCode executable ["--mixed-fresh", work, show count] ""
  unless (exit == ExitSuccess) $ fail ("fresh mixed worker failed: " ++ errors)
  let expectedLoad = if required then 3 else 2 :: Int
      loadCounts = counterValues "home_products_source_load_owners" errors
  unless (loadCounts == [fromIntegral expectedLoad]) $
    fail ("fresh mixed worker loaded unrelated owners: " ++ show loadCounts)
  putStrLn ("mixed SOURCE: PASS independent=" ++ show count ++ " required=" ++ show required)

requireMixed :: Int -> PreparedPipelineResult -> IO ()
requireMixed count result = unless
  (Set.fromList (map candidateModule (pprAcceptedCandidates result)) == Set.fromList
      (["CacheEven", "CacheOdd"] ++ ["Independent" ++ show index | index <- [1 .. count]])
    && preparedNames result == ["CacheEntry"]
    && dependencyCacheSafe (pprDependencies result)
    && dependencySelectionComplete (pprDependencies result)) $
  fail ("mixed SOURCE reuse changed closure: accepted="
    ++ show (map candidateModule (pprAcceptedCandidates result))
    ++ " prepared=" ++ show (preparedNames result))

exerciseFamilyRefusal :: FilePath -> IO ()
exerciseFamilyRefusal work = do
  let boot = work </> "CacheEven.hs-boot"
  original <- BS.readFile boot
  -- The same nominal family has a different kind in the current boot input.
  (do writeFile boot (unlines [if line == "type family Payload a"
        then "type family Payload a b" else line | line <- lines (BSC.unpack original)])
      changed <- try (reuseFresh work) :: IO (Either SomeException PreparedPipelineResult)
      case changed of
        Left _ -> pure ()
        Right result -> requireRefused "changed boot family arity" result)
    `finally` BS.writeFile boot original
  reuseFresh work >>= requireMixed 1

exerciseIndependentDrift :: FilePath -> IO ()
exerciseIndependentDrift work = do
  let source = work </> "Independent1.hs"
  original <- BS.readFile source
  (do writeFile source (unlines [if line == "value = 1"
        then "value = 2" else line | line <- lines (BSC.unpack original)])
      result <- reuseFresh work
      unless (Set.fromList (map candidateModule (pprAcceptedCandidates result))
          == Set.fromList ["CacheEven", "CacheOdd"]
        && Set.fromList (preparedNames result) == Set.fromList ["Independent1", "CacheEntry"]
        && dependencyCacheSafe (pprDependencies result)
        && dependencySelectionComplete (pprDependencies result)) $
        fail "unrelated source drift skipped a changed product or lost the SOURCE SCC")
    `finally` BS.writeFile source original
  reuseFresh work >>= requireMixed 1

exerciseNegativeHomeSelection :: FilePath -> IO ()
exerciseNegativeHomeSelection work = do
  let source = work </> "Prelude.hs"
  -- This was an absent home path in every producer's package-import witness.
  -- Its current presence changes GHC's selection even though it reexports
  -- the same package Names. No original candidate can skip that new owner.
  (do writeFile source (unlines ["{-# LANGUAGE PackageImports #-}"
        , "module Prelude (module PackagePrelude) where"
        , "import \"base\" Prelude as PackagePrelude"])
      result <- reuseFresh work
      requireRefused "new home Prelude selection" result)
    `finally` removeFile source
  reuseFresh work >>= requireMixed 1

exerciseRefusals :: FilePath -> IO PreparedPipelineResult -> IO ()
exerciseRefusals = exerciseRefusalsWith requireReused

exerciseRefusalsWith
  :: (String -> PreparedPipelineResult -> IO ()) -> FilePath -> IO PreparedPipelineResult -> IO ()
exerciseRefusalsWith requireAccepted work reuse = do
  -- The type ABI changes while the ordinary source bytes and interfaces
  -- remain unchanged. Fresh boot validation must refuse the old SCC.
  let boot = work </> "CacheEven.hs-boot"
  original <- BS.readFile boot
  (do BS.writeFile boot "module CacheEven where\nimport Prelude\neven' :: Bool -> Bool\n"
      changed <- try reuse :: IO (Either SomeException PreparedPipelineResult)
      case changed of
        Left _ -> pure ()
        Right result -> requireRefused "changed boot ABI" result)
    `finally` BS.writeFile boot original
  reuse >>= requireAccepted "resident reuse after ABI refusal"
  -- CPP has readable inputs outside the bounded source graph. Refuse the
  -- entire SCC even when this particular source happens to typecheck.
  (BS.writeFile boot ("{-# LANGUAGE CPP #-}\n" <> original) >>
    reuse >>= requireRefused "untracked boot CPP")
    `finally` BS.writeFile boot original
  reuse >>= requireAccepted "resident reuse after refusal"


manifest :: FilePath -> FilePath
manifest work = work </> "module-candidates.cbor"

reuseFresh :: FilePath -> IO PreparedPipelineResult
reuseFresh work = runPipelineSelected (PreparedProducts (Just (manifest work)))
  (work </> "CacheEntry.hs") [work]

preparedNames :: PreparedPipelineResult -> [String]
preparedNames = map (moduleNameString . moduleName . pmModule) . pprModules

requireReused :: String -> PreparedPipelineResult -> IO ()
requireReused label result = unless
  (Set.fromList (map candidateModule (pprAcceptedCandidates result))
      == Set.fromList ["CacheEven", "CacheOdd"]
    && all (`notElem` preparedNames result) ["CacheEven", "CacheOdd"]
    && "CacheEntry" `elem` preparedNames result
    && dependencyCacheSafe (pprDependencies result)
    && dependencySelectionComplete (pprDependencies result)) $
  fail (label ++ " did not reuse exact SOURCE SCC: accepted="
    ++ show (map candidateModule (pprAcceptedCandidates result))
    ++ " prepared=" ++ show (preparedNames result))

requireRefused :: String -> PreparedPipelineResult -> IO ()
requireRefused label result = unless (null (pprAcceptedCandidates result)) $
  fail (label ++ " retained a stale SOURCE SCC")

verifyHydration :: FilePath -> PreparedPipelineResult -> IO ()
verifyHydration work cold = do
  let producer = prHscEnv (pprPipelineResult cold)
      names = map mkModuleName ["CacheEven", "CacheOdd"]
      graph = hsc_mod_graph producer
      ordinary = [summary | ModuleNode _ summary <- mgModSummaries' graph
        , ms_hsc_src summary == HsSrcFile, ms_mod_name summary `elem` names]
      boots = [summary | ModuleNode _ summary <- mgModSummaries' graph
        , ms_hsc_src summary == HsBootFile, ms_mod_name summary `elem` names]
  candidates <- readModuleCandidates (manifest work) >>= either fail pure
  interfaces <- forM ["CacheEven", "CacheOdd"] $ \name -> do
    candidate <- case [value | value <- candidates, candidateModule value == name] of
      [value] -> pure value
      _ -> fail "SOURCE hydration lost its genuine candidate interface"
    iface <- maybe (fail "missing original source iface") pure
      (Map.lookup (mkModuleName name) (pprProductInterfaces cold))
    pure (ExactIfaceArtifact (candidateUnit candidate) name (candidateInterface candidate)
      (candidateInterfaceSha256 candidate) (candidateInterfaceRequirements candidate), iface)
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    fresh <- liftIO (freshExactState producer)
    let current = fresh {hsc_mod_graph = graph}
    setSession current
    restored <- hydrateCandidateHomeProducts current graph interfaces ordinary boots
    case restored of
      Left reason -> liftIO (fail ("fresh SOURCE hydration refused: " ++ reason))
      Right _ -> pure ()

writeManifestFor :: [String] -> FilePath -> PreparedPipelineResult -> IO ()
writeManifestFor names work cold = do
  candidates <- forM names $ \name -> do
    let key = mkModuleName name
        nodes = [node | node <- dependencyModules (pprDependencies cold)
          , dependencyModuleName node == name, not (dependencyModuleBoot node)]
    node <- case nodes of
      [value] -> pure value
      _ -> fail "SOURCE module lacks one compiler graph witness"
    iface <- maybe (fail "SOURCE module lacks a skinny interface") pure
      (Map.lookup key (pprProductInterfaces cold))
    let hi = work </> (name ++ ".candidate.hi")
    writeBinIface (targetProfile (hsc_dflags (prHscEnv (pprPipelineResult cold))))
      QuietBinIFace NormalCompression hi iface
    bytes <- BS.readFile hi
    source <- sourceEvidence (dependencyModuleSource node)
    let artifact = ExactIfaceArtifact (dependencyModuleUnit node) name hi (digest bytes) []
        packages = encodePackageImports artifact
          (Map.findWithDefault emptyPackageImports key (pprPackageImports cold))
        packagePath = hi ++ ".packages"
        text = encodeString . T.pack
        imports = dependencyModuleImports node
        productPath = hi ++ ".descriptor-only.tpmod"
    -- These fixtures exercise source/interface admission without publishing
    -- native products. Runtime promotion separately requires a framed TPMOD.
    BS.writeFile productPath BS.empty
    BS.writeFile packagePath packages
    pure $ encodeListLen 14
      <> foldMap text [dependencyModuleUnit node, name, dependencyModuleSource node
        , dependencySourceSha256 source, hi, digest bytes]
      <> foldMap text [replicate 64 '0', digest BS.empty, replicate 64 '0']
      <> encodeListLen (fromIntegral (length imports))
      <> foldMap (\imported -> encodeListLen 4
        <> text (renderDependencyQualifier (dependencyImportQualifier imported)) <> text (dependencyImportName imported)
        <> encodeBool (dependencyImportBoot imported)
        <> text (maybe "" id (dependencyImportSelected imported))) imports
      <> encodeListLen 0 <> text packagePath <> text (digest packages) <> text productPath
  BS.writeFile (manifest work) (toStrictByteString
    (encodeListLen 6 <> encodeString "TPMCAN" <> encodeString "8"
      <> encodeListLen 0 <> encodeListLen 0
      <> encodeListLen (fromIntegral (length candidates)) <> mconcat candidates
      <> encodeListLen 2 <> encodeListLen 0 <> encodeListLen 0))

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
