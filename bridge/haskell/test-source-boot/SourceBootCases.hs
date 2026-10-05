module SourceBootCases where

import CandidateExecutionSourcesTest (executionScopeDescriptorChecks)

import ExactScopeV9Test (exactScopeV9Checks, nativeOriginChecks, candidateCanonicalChecks)

import SourceBootFixtureSupport

import CandidateGraphDescriptorTest (candidateGraphDescriptorChecks)
import CodecFixtureSupport
import Tidepool.Test.GenuineCandidate
  ( writeGenuineCandidateManifestFor, writeGenuineMetadataScope, writeGenuineEmptyMetadataScope
  , writeGenuineCandidateNativeScope, writeGenuineCandidateLexicalScope, writeGenuineAuthoredDeclarationScope
  , writeGenuineExecutionScope )

import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Data.ByteString.Lazy qualified as BSL
import Control.Exception (SomeException, IOException, AsyncException(ThreadKilled), bracket, evaluate, finally, try, fromException, onException, mask, catches, Handler(..), throwIO)
import Control.Concurrent (MVar, forkIO, killThread, myThreadId, throwTo, threadDelay, newEmptyMVar, putMVar, takeMVar)
import Data.IntMap.Strict qualified as IntMap
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
import GHC (runGhc, getSession, setSession, SafeHaskellMode(Sf_None), ms_mod_name, ms_hsc_src, ms_hspp_buf, ms_hspp_file, ms_hspp_opts, parseModule, typecheckModule, TypecheckedModule(..), ParsedModule(..))
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
import GHC.Types.SourceError (SourceError)
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
  ( copyFile, createDirectory, createDirectoryIfMissing, removeDirectoryRecursive
  , removeFile, renameFile, listDirectory, doesFileExist, getPermissions, setPermissions, executable
  , getModificationTime, setModificationTime, withCurrentDirectory, getCurrentDirectory, canonicalizePath )
import System.Environment (setEnv, lookupEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>), takeBaseName, takeDirectory, normalise, replaceExtension, addTrailingPathSeparator)
import System.IO (hPutStrLn, hSetFileSize, withBinaryFile, IOMode(WriteMode), stderr)
import System.IO.Error (isDoesNotExistError, ioeGetFileName)
import System.Process (readProcessWithExitCode)
import System.Timeout (timeout)
import Tidepool.CompilerProducts
  ( writeCertifiedProductsKeeping, retainedOriginalInterfaces, certifiedSourceOriginals, certifiedFinalizedArtifacts )
import Tidepool.CertifiedProducts (encodeCertifiedProducts, resolvePackageGlobal)
import Tidepool.FinalizedModuleArtifacts (captureFinalizedModuleArtifacts, emptyFinalizedModuleArtifacts, finalizedLocalAdmissions, localFinalizedInterface)
import Tidepool.FinalizedModule (finalizedHomeModInfo, homeInterfaceUsageOwners)
import Tidepool.ExecutionEncode (encodeModuleProducts)

import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), projectPreparedModuleGroups
  , projectPreparedModuleProducts, projectOriginalHomeModuleProducts, preparedModuleProductOutcomes, closeUnavailableOriginalGroups, closeUnavailableOriginalModules, topBinders
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
import Tidepool.DiagJson (InputRejection(..), DependencyLoadFailure(..), Diag(..), DiagSeverity(..), diagsFromSourceError)
import Tidepool.ExecutionSchema
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencyImport(..)
  , DependencyResolution(..), ProductAvailability(..), DependencySource(..), sourceEvidence
  , DependencyQualifier(..), renderDependencyQualifier
  , selectedFreshHomeRequirements, renderDependencyEvidence )
import Tidepool.ExactHydration
  ( CheckedTemplateInterface(..), newOriginalInterfaceArtifacts, originalInterfaceBytes, ExactIfaceArtifact(..), freshExactState, noCheckedValueImports, installExactLexicalGraph
  , readCheckedValueImportAuthority, readExactIfaceArtifacts, hydrateExactScope
  , readVerifiedExactIfaceClosure, readVerifiedExactIfaceClosureWithCheckedValues
  , selectVerifiedExactInterfaces, selectVerifiedValueInterfaces, checkedValueImportAuthorityFromVerified )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.HomeProducts (hydrateCandidateHomeProducts, CandidateCoreFailure(..))
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), PipelineResult(..), CheckedEnvironmentResult(..)
  , preparedFreshDependencies, preparedExactCompilation, preparedHomeRequirements
  , finalizedTidyGuts, FinalizedExecutionFailure(..)
  , retainedCompilerArtifactClosure, retainedCompilerInterface
  , renderType, generatedScaffoldRecipe, activationPreviewInputType, withSourceImportIntents
  , CompilePurpose(..), runPipelineSelected, runPipelineSessionSelected, withResidentPipelineSelected
  , withResidentPipelineSelectedRequests )
import Tidepool.ModuleCandidates (ModuleCandidate(..), CandidateGroup(..), CandidateGlobal(..)
  , readModuleCandidates, readModuleCandidatesWithGraphs, candidateExecutionSources, candidateOriginalIdentity
  , candidateCoreDescriptor, captureCandidateManifest, readCapturedModuleCandidatesWithGraphs)
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
  , scopeModuleInterfaceProofs, canonicalCoreArtifact, canonicalCorePath, canonicalCoreSha256
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalRequirements
  , ExactCompilation(..), ExactInterfaceEvidence(..), SourceSelectedOriginals(..)
  , extendSourceSelectedOriginals, validateCanonicalInterfaceProof, canonicalSourceImports
  , originalGroupFromCandidate
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget, scopeExecutionNativeOwners )
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
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

exactCompilationCacheSafety :: FilePath -> FilePath -> IO (Maybe Bool)
exactCompilationCacheSafety expectedSource receipt = do
  facts <- readReceiptCodecFacts (takeDirectory receipt) receipt
  pure (if codecReceiptSource facts == expectedSource then Just (codecReceiptCacheSafe facts) else Nothing)

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
  unless (dependencyCacheSafe (preparedFreshDependencies cold)
      && dependencySelectionComplete (preparedFreshDependencies cold)) $
    fail "cold SOURCE graph lacks final source/package evidence"
  coldFixture <- capturePreparedFixture work cold
  nativeScope <- writeGenuineCandidateNativeScope ["CacheEven", "CacheOdd"]
    ["NativeScopeBase", "NativeScopeOwner"] work coldFixture
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
  createDirectoryIfMissing True (takeDirectory authoredSource)
  copyFile "test-source-boot/fixtures/AuthoredScopeG1.hs" authoredSource
  authoredScope <- writeGenuineAuthoredDeclarationScope nativeOwner [work] authoredSource work
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
  executable <- lookupEnv "TIDEPOOL_TEST_SOURCE_BOOT_CHILD" >>= maybe
    (fail "missing declared source-boot child executable") pure
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
      owners = Set.fromList ["NativeScopeBase", "NativeScopeOwner"]
  BS.appendFile source (BSC.pack ("\n--" ++ replicate (4*1024*1024) ' ' ++ "\n"))
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile
    Nothing source [work] (Just (work </> "build-products"))
  originalFixture <- capturePreparedFixture work original
  scopePath <- writeGenuineCandidateNativeScope (Set.toAscList owners) (Set.toAscList owners)
    work originalFixture
  candidateGraphDescriptorChecks scopePath (manifest work)
  executionScopeDescriptorChecks scopePath
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
writeGenuineEmptyScopeFields work = do
  path <- writeGenuineEmptyMetadataScope work
  _ <- readExactScope path >>= either fail pure
  bytes <- BS.readFile path
  either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes)) >>= \case
      TList values | length values == 9 -> pure values
      _ -> fail "genuine exact scope has another envelope layout"

exactScopeBinders :: IO ()
exactScopeBinders = withScratch $ \work -> do
  fields <- writeGenuineEmptyScopeFields work
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

checkedValueTypeClosure :: FilePath -> IO ()
checkedValueTypeClosure effects = withScratch $ \work -> do
  let producerPath = work </> "MetadataBashTarget.hs"
      consumerPath = work </> "CheckedCommandConsumer.hs"
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
  let ownerNames = map moduleNameString (Map.keys (pprProductInterfaces prepared))
  preparedFixture <- capturePreparedFixture work prepared
  scopePath <- writeGenuineCandidateLexicalScope [] ownerNames work preparedFixture
  base <- readExactScope scopePath >>= either fail pure
  let originals = scopeInterfaces base
      value = ExactIfaceArtifact "main" "Tidepool.Session.Val.G7" valuePath (digest valueBytes) requirements
  let admitted = base
        { scopePurpose = ExactCellPurpose (CheckedCellAdmission (replicate 64 '0') (replicate 64 '0')
            (replicate 64 '0') [] ["Tidepool.Session.Val.G7"] [] [value] Nothing AuthoredCellCheck) [work,"lib",effects] }
      scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath, ssValIfaces = [valueOwner] }
  isolated <- readCheckedValueImportAuthority environment [value]
  unless (case isolated of Left "incomplete exact interface dependency closure" -> True; _ -> False) $
    fail "a checked value authorized its absent type owner"
  verified <- readVerifiedExactIfaceClosureWithCheckedValues environment
    [iface | (iface,_,_) <- originals] [value] >>= either fail pure
  _ <- either fail pure (checkedValueImportAuthorityFromVerified verified [value])
  _ <- either fail pure (selectVerifiedExactInterfaces verified [value])
  forM_ [value {exactRequirements=[]},value {exactSha256=replicate 64 '0'}] $ \changed ->
    unless (case selectVerifiedExactInterfaces verified [changed] of Left _ -> True; Right _ -> False) $
      fail "late checked value weakened or changed its captured type proof"
  let missing = [iface | (iface,_,_) <- originals, exactModule iface /= "Tidepool.Command.Types"]
  refused <- readVerifiedExactIfaceClosureWithCheckedValues environment missing [value]
  unless (case refused of Left _ -> True; Right _ -> False) $
    fail "a checked command accepted an absent captured type owner"
  let aliasPath = work </> "captured-command-copy.hi"
      alias = value {exactPath=aliasPath}
  copyFile valuePath aliasPath
  aliasClosure <- readVerifiedExactIfaceClosureWithCheckedValues environment
    [iface | (iface,_,_) <- originals] [alias] >>= either fail pure
  selected <- either fail pure (selectVerifiedValueInterfaces aliasClosure [alias])
  unless (map (exactRequirements . fst) selected == [requirements]) $
    fail "captured value alias lost its complete original type requirements"
  forM_ [alias {exactSha256=replicate 64 '0'},alias {exactPath=aliasPath ++ ".missing"}] $ \wrong -> do
    rejected <- readVerifiedExactIfaceClosureWithCheckedValues environment
      [iface | (iface,_,_) <- originals] [wrong]
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

canonicalCurrentSource :: IO ()
canonicalCurrentSource = withTiming $ withScratch $ \work -> do
  forM_ ["CanonicalSource.hs","CanonicalDependency.hs","CanonicalConsumer.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  let owner = work </> "CanonicalSource.hs"
      consumer = work </> "CanonicalConsumer.hs"
      includes = [work]
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing owner includes Nothing
  originalFixture <- capturePreparedFixture work original
  scopePath <- writeGenuineMetadataScope work ["CanonicalSource","CanonicalDependency"] originalFixture
  base <- readExactScope scopePath >>= either fail pure
  unless (null (scopeProducts base) && null (scopeExecutionOwners base) && null (scopeExecutionGraphs base)) $
    fail "canonical current-source fixture unexpectedly retained native execution authority"
  let admitted = base {scopePurpose=ExactCellPurpose (CheckedCellAdmission (replicate 64 '0')
        (replicate 64 '0') (replicate 64 '0') [] [] [] [] Nothing AuthoredCellCheck) includes}
      session = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
      check compile purpose = compile CheckedEnvironment Set.empty (CellProgramCompile purpose admitted)
        (Just session) consumer includes Nothing
  parsed <- analyzeCellWithFlags (hsc_dflags (prHscEnv (pprPipelineResult original))) ""
    "import CanonicalSource as Source (Answer)\nimport CanonicalSource (Answer)\n(1 :: Answer)"
    >>= either (fail . show) pure
  let purpose = withSourceImportIntents (cellPlanPrologue parsed) GeneralCompile
  originalBytes <- BS.readFile owner
  dependencyBytes <- BS.readFile (work </> "CanonicalDependency.hs")
  withResidentPipelineSelected includes $ \compile -> do
    _ <- check compile purpose
    receipts <- listDirectory (work </> ".exact-compilations")
    let receiptPath = work </> ".exact-compilations" </> head receipts </> "receipt.cbor"
    receiptFacts <- readReceiptCodecFacts work receiptPath
    unless (Set.fromList (codecReceiptSourceSelected receiptFacts)
        == Set.fromList [("main","CanonicalSource"),("main","CanonicalDependency")]) $
      fail "canonical source receipt lost its typed transitive source-selected closure"
    -- The next authored import must consume this cell's completed original
    -- capture through the same proof used after Rust persistence.
    forM_ ["CanonicalLocalSupport.hs","CanonicalLocalConsumer.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
    let localTarget = work </> "CanonicalLocalConsumer.hs"
        localOwner = ("main","CanonicalLocalSupport")
        localSource = work </> "CanonicalLocalSupport.hs"
        captureDirectory = work </> "local-original-capture"
    captured <- compile (PreparedProducts Nothing) Set.empty (CellProgramCompile GeneralCompile admitted)
      (Just session) localTarget includes Nothing
    createDirectory captureDirectory
    originals <- newOriginalInterfaceArtifacts (prHscEnv (pprPipelineResult captured))
      (pprFinalizedModules captured) (retainedOriginalInterfaces captured) captureDirectory
    emitted <- writeCertifiedProductsKeeping includes originals captureDirectory captured Nothing []
    localProof <- maybe (fail "fresh support did not emit a complete source-original proof") pure
      (Map.lookup localOwner (certifiedSourceOriginals emitted))
    localCompilation <- maybe (fail "fresh support did not retain exact compilation custody") pure
      (preparedExactCompilation captured)
    inherited <- either fail pure (extendSourceSelectedOriginals
      (compilationSourceSelection localCompilation) admitted)
    -- Fresh rows are in the completed capture rather than its input scope.
    let capturedRows = finalizedLocalAdmissions (certifiedFinalizedArtifacts emitted)
    localAdmission <- maybe (fail "fresh support lacks captured interface custody") pure
      (Map.lookup localOwner capturedRows)
    unless (maybe False (elem (DependencyUnqualified,"CanonicalDependency",False,Just "main"))
        (canonicalSourceImports localProof)) $
      fail "captured original lost its exact retained-home import receipt"
    let row = localFinalizedInterface localAdmission
        selectedScope = inherited
          { scopeInterfaces = scopeInterfaces inherited ++ [row]
          , scopeInterfaceEvidence = Map.insert localOwner (ModuleInterfaceEvidence localProof)
              (scopeInterfaceEvidence inherited)
          , scopeLexical = scopeLexical inherited ++ [(localOwner,[("main","CanonicalDependency")])] }
    parsedLocal <- analyzeCellWithFlags (hsc_dflags (prHscEnv (pprPipelineResult captured))) ""
      "import CanonicalLocalSupport\n(1 :: Answer)" >>= either (fail . show) pure
    let localPurpose = withSourceImportIntents (cellPlanPrologue parsedLocal) GeneralCompile
        checkLocal scope = compile (PreparedProducts Nothing) Set.empty
          (CellProgramCompile localPurpose scope) (Just session) localTarget includes Nothing
        recheckLocal = checkLocal selectedScope
    selectedLocal <- recheckLocal
    let selectedRows = maybe [] (maybe [] selectedOriginalRows . compilationSourceSelection)
          (preparedExactCompilation selectedLocal)
    unless (any (\(key,seal,_,_) -> key == localOwner
        && seal == canonicalCertificateSha256 localProof) selectedRows) $
      fail "later authored import did not select the captured canonical identity"
    let copiedCertificate = work </> "relocated-local-original.certificate.cbor"
    copyFile (canonicalCertificatePath localProof) copiedCertificate
    relocated <- validateCanonicalInterfaceProof selectedScope localOwner copiedCertificate
      (canonicalCertificateSha256 localProof) ((\core -> (canonicalCorePath core,canonicalCoreSha256 core))
        <$> canonicalCoreArtifact localProof) >>= either fail pure
    unless (canonicalCertificateSha256 relocated == canonicalCertificateSha256 localProof
        && canonicalSourceImports relocated == canonicalSourceImports localProof) $
      fail "canonical source identity depends on its capture/persistence locator"
    _ <- checkLocal selectedScope {scopeInterfaceEvidence = Map.insert localOwner
      (ModuleInterfaceEvidence relocated) (scopeInterfaceEvidence selectedScope)}
    let wrongOwner = ("main","CanonicalWrongOwner")
        (localIface,_,_) = row
        wrongOwnerScope = selectedScope {scopeInterfaces =
          [(if (exactUnit artifact,exactModule artifact) == localOwner
              then localIface {exactModule=snd wrongOwner} else artifact,packages,seal)
          | (artifact,packages,seal) <- scopeInterfaces selectedScope]}
    wrongOwnerProof <- validateCanonicalInterfaceProof wrongOwnerScope wrongOwner
      copiedCertificate (canonicalCertificateSha256 localProof)
      ((\core -> (canonicalCorePath core,canonicalCoreSha256 core)) <$> canonicalCoreArtifact localProof)
    unless (either (const True) (const False) wrongOwnerProof) $
      fail "captured source original authorized another exact module owner"
    copyFile "test-source-boot/fixtures/CanonicalDependencyChanged.hs" (work </> "CanonicalDependency.hs")
    requireOriginalSourceBytesChanged "captured source retained import closure"
      ("main","CanonicalDependency") (work </> "CanonicalDependency.hs") (digest dependencyBytes) recheckLocal
    BS.writeFile (work </> "CanonicalDependency.hs") dependencyBytes
    _ <- recheckLocal
    localBytes <- BS.readFile localSource
    BS.appendFile localSource "\n-- changed original source\n"
    requireOriginalSourceBytesChanged "captured source later drift"
      localOwner localSource (digest localBytes) recheckLocal
    BS.writeFile localSource localBytes
    _ <- recheckLocal
    copyFile "test-source-boot/fixtures/CanonicalDependencyChanged.hs" (work </> "CanonicalDependency.hs")
    requireOriginalSourceBytesChanged "canonical current-source admitted dependency"
      ("main","CanonicalDependency") (work </> "CanonicalDependency.hs") (digest dependencyBytes)
      (check compile purpose)
    BS.writeFile (work </> "CanonicalDependency.hs") dependencyBytes
    _ <- check compile purpose
    BS.writeFile owner "module CanonicalSource where\ntype Answer = Bool\n"
    requireOriginalSourceBytesChanged "canonical current-source drift"
      ("main","CanonicalSource") owner (digest originalBytes) (check compile purpose)
    removeFile owner
    requireOriginalSourceRejection "canonical current-source missing original"
      (ExecutionSourceUnavailable ("main","CanonicalSource")) (check compile purpose)
    BS.writeFile owner originalBytes
    _ <- check compile purpose
    pure ()
  putStrLn "canonical current source: captured and persisted interface-only proofs, exact imports, duplicate imports, dependency/source drift and missing refusal"

-- GHC owns whether an authored import contributes an interface obligation.
-- Inspect that evidence before projecting custody; import text is never used
-- to manufacture an absent type or native dependency.
canonicalSourceObligations :: IO ()
canonicalSourceObligations = withTiming $ withScratch $ \work -> do
  forM_ ["CanonicalUnusedSource.hs","CanonicalUnusedDependency.hs","CanonicalUnusedConsumer.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  let owner = work </> "CanonicalUnusedSource.hs"
      dependency = work </> "CanonicalUnusedDependency.hs"
      consumer = work </> "CanonicalUnusedConsumer.hs"
      includes = [work]
      ownerName = mkModuleName "CanonicalUnusedSource"
      dependencyKey = ("main","CanonicalUnusedDependency")
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing owner includes Nothing
  finalized <- maybe (fail "unused import fixture lacks its original finalized owner") pure
    (Map.lookup ownerName (pprFinalizedModules original))
  let environment = prHscEnv (pprPipelineResult original)
      usageOwners = homeInterfaceUsageOwners environment (hm_iface (finalizedHomeModInfo finalized))
      required = dependencyKey `elem` usageOwners
      retained = "CanonicalUnusedSource" : ["CanonicalUnusedDependency" | required]
      imported = [edge | node <- dependencyModules (preparedFreshDependencies original)
        , dependencyModuleName node == "CanonicalUnusedSource", edge <- dependencyModuleImports node
        , dependencyImportName edge == "CanonicalUnusedDependency"]
  unless (length imported == 1 && all ((== Just dependency) . dependencyImportSelected) imported) $
    fail "unused import fixture lacks its genuine parsed home edge"
  originalFixture <- capturePreparedFixture work original
  scopePath <- writeGenuineMetadataScope work retained originalFixture
  base <- readExactScope scopePath >>= either fail pure
  ownerProof <- maybe (fail "unused import fixture lacks canonical custody") pure
    (Map.lookup ("main","CanonicalUnusedSource") (scopeModuleInterfaceProofs base))
  unless (Map.keys (canonicalRequirements ownerProof) == usageOwners
      && Map.member dependencyKey (scopeModuleInterfaceProofs base) == required
      && null (scopeProducts base) && null (scopeExecutionGraphs base)) $
    fail "unused import scope invented an interface or native obligation"
  parsed <- analyzeCellWithFlags (hsc_dflags environment) ""
    "import CanonicalUnusedSource (Answer)\n(1 :: Answer)" >>= either (fail . show) pure
  let purpose = withSourceImportIntents (cellPlanPrologue parsed) GeneralCompile
      admitted = base {scopePurpose=ExactCellPurpose (CheckedCellAdmission (replicate 64 '0')
        (replicate 64 '0') (replicate 64 '0') [] [] [] [] Nothing AuthoredCellCheck) includes}
      session = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
      check compile = compile CheckedEnvironment Set.empty (CellProgramCompile purpose admitted)
        (Just session) consumer includes Nothing
  originalBytes <- BS.readFile dependency
  withResidentPipelineSelected includes $ \compile -> do
    _ <- check compile
    copyFile "test-source-boot/fixtures/CanonicalUnusedDependencyChanged.hs" dependency
    if required
      then requireOriginalSourceBytesChanged "current source retained obligation"
        dependencyKey dependency (digest originalBytes) (check compile)
      else void (check compile)
    ordinary <- compile (PreparedProducts Nothing) Set.empty GeneralCompile
      Nothing consumer includes Nothing
    changedSource <- sourceEvidence dependency
    unless (Map.member (mkModuleName "CanonicalUnusedDependency") (pprFinalizedModules ordinary)
        && any (\source -> dependencySourcePath source == dependencySourcePath changedSource
          && dependencySourceSha256 source == dependencySourceSha256 changedSource)
          (dependencySources (preparedFreshDependencies ordinary))) $
      fail "ordinary fresh source refresh inherited an exact retained obligation"
    BS.writeFile dependency originalBytes
    void (check compile)
  putStrLn ("canonical source obligations: actual GHC usage=" ++ show required
    ++ ", ordinary fresh refresh accepted, source import shape preserved, " ++ if required
      then "held dependency drift refused and recovered"
      else "unretained dependency remained fresh after drift")

generatedScaffoldImports :: IO ()
generatedScaffoldImports = withTiming $ withScratch $ \work -> do
  let supportDirectory = work </> "Tidepool/Internal"
      supportPath = supportDirectory </> "Resume.hs"
      capturePath = work </> "GeneratedScaffoldCapture.hs"
      target = work </> "Expr.hs"
      includes = [work]
      originalOwners = filter (/= "GeneratedScaffoldCapture") . preparedNames
  createDirectoryIfMissing True supportDirectory
  copyFile "lib/Tidepool/Internal/Resume.hs" supportPath
  copyFile "test-source-boot/fixtures/GeneratedScaffoldCapture.hs" capturePath
  copyFile "test-source-boot/fixtures/GeneratedScaffoldExpr.hs" target
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing capturePath includes Nothing
  originalFixture <- capturePreparedFixture work original
  hiddenPath <- writeGenuineCandidateNativeScope [] (originalOwners original) work originalFixture
  let hidden = emptySessionScope {ssRoot=work,ssExactScope=Just hiddenPath}
  protected <- readFile target
  recipe <- generatedScaffoldRecipe [] protected protected target "Expr" >>= either fail pure
  let purpose = GeneratedScaffoldCompile recipe (CheckedItemCompile [] Nothing [])
      supportOwner = ("main","Tidepool.Internal.Resume")
      requireHiddenSource label = requireOriginalSourceRejection label (ExecutionSourceUnavailable supportOwner)
      neighborDiagnostic scopePath = do
        retained <- readExactScope scopePath >>= either fail pure
        artifact <- case [iface | (iface,_,_) <- scopeInterfaces retained
            , (exactUnit iface,exactModule iface) == supportOwner] of
          [iface] -> pure iface
          _ -> fail "scaffold neighbor fixture lost its support interface"
        pure $ if any (/= supportOwner) (exactRequirements artifact)
          then "generated scaffold support requires another home implementation owner"
          else "generated scaffold support imports home orphan or family witnesses"
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
    requireHiddenSource "additional authored hidden import" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile duplicate GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target (protected ++ "\ntampered = 0 :: Int\n")
    requireUserError "changed rendered target"
      "generated scaffold target differs from its protected recipe" $
      compile (PreparedProducts Nothing) Set.empty purpose (Just hidden) target [] Nothing
    writeFile target protected
    copyFile "test-source-boot/fixtures/GeneratedScaffoldHelper.hs" (work </> "GeneratedScaffoldHelper.hs")
    let helperTarget = unlines (take 5 (lines protected) ++ ["import GeneratedScaffoldHelper"] ++ drop 5 (lines protected))
    writeFile target helperTarget
    helperRecipe <- generatedScaffoldRecipe [] protected helperTarget target "Expr" >>= either fail pure
    requireHiddenSource "fresh helper importing hidden support" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile helperRecipe GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target ("{-# LINE 100 \"authored.hs\" #-}\n" ++ protected)
    lineRecipe <- generatedScaffoldRecipe [] protected ("{-# LINE 100 \"authored.hs\" #-}\n" ++ protected) target "Expr" >>= either fail pure
    requireSourceSelectionInput "logical LINE import location differs from protected occurrence"
      "generated scaffold import occurrence differs from its protected recipe" $
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
    requireSourceSelectionInput "template interface cannot replace paired native owner"
      "generated scaffold lacks one paired original native owner" $
      compile (PreparedProducts Nothing) Set.empty
        (CellProgramCompile capturedPurpose admittedScope {scopeProducts=[]}) (Just hidden) target [] Nothing
    wrongSeal <- generatedScaffoldRecipe [templateInterface {templateInterfaceSha256=replicate 64 'f'}]
      withTemplate withTemplate target "Expr" >>= either fail pure
    requireSourceSelectionInput "changed initial template interface seal"
      "checked template graph interface seal changed" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile wrongSeal GeneralCompile)
        (Just hidden) target [] Nothing
    let secondImport = unlines (take 5 (lines withTemplate)
          ++ ["import qualified Tidepool.Internal.Resume as AuthoredSecond"] ++ drop 5 (lines withTemplate))
    writeFile target secondImport
    secondRecipe <- generatedScaffoldRecipe [templateInterface] withTemplate secondImport target "Expr"
      >>= either fail pure
    requireHiddenSource "authored second import beside permitted initial template import" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile secondRecipe GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target (withTemplate ++ "\ntamperedTemplateTarget = 0 :: Int\n")
    requireUserError "same owner with changed template target bytes"
      "generated scaffold target differs from its protected recipe" $
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
    requireSourceSelectionInput "missing paired original native owner"
      "generated scaffold lacks one paired original native owner" $
      compile (PreparedProducts Nothing) Set.empty
        (CellProgramCompile purpose admittedScope {scopeProducts=[]}) (Just hidden) target [] Nothing
    let alteredOwner product' = product' {originalIfaceSha256=replicate 64 'f'}
    -- A product with another paired interface cannot grant scaffold authority.
    let differentScope = admittedScope {scopeProducts=map alteredOwner (scopeProducts admittedScope)}
    requireSourceSelectionInput "wrong paired original interface identity"
      "generated scaffold lacks one paired original native owner" $
      compile (PreparedProducts Nothing) Set.empty (CellProgramCompile purpose differentScope)
        (Just hidden) target [] Nothing
    supportText <- BSC.unpack <$> BS.readFile supportPath
    let incompleteExports = unlines [if line == "  , resumeLifted" then "" else line | line <- lines supportText]
    writeFile supportPath incompleteExports
    missingExport <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing capturePath [] Nothing
    missingExportFixture <- capturePreparedFixture work missingExport
    missingExportPath <- writeGenuineCandidateNativeScope [] (originalOwners missingExport) work missingExportFixture
    requireSourceSelectionInput "missing actual resumeLifted export"
      "generated scaffold support has another export owner" $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just missingExportPath}) target [] Nothing
    forM_ ["ExecutionClass.hs","ExecutionHiddenOrphan.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
    let withOrphan = unlines [if line == "import Prelude" then
          "import Prelude\nimport ExecutionHiddenOrphan ()" else line | line <- lines supportText]
    writeFile supportPath withOrphan
    hiddenNeighbor <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing capturePath [] Nothing
    neighborFixture <- capturePreparedFixture work hiddenNeighbor
    neighborPath <- writeGenuineCandidateNativeScope [] (originalOwners hiddenNeighbor) work neighborFixture
    orphanDiagnostic <- neighborDiagnostic neighborPath
    requireSourceSelectionInput "hidden orphan neighbor through scaffold support" orphanDiagnostic $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just neighborPath}) target [] Nothing
    copyFile "test-source-boot/fixtures/MetadataHiddenFamily.hs" (work </> "MetadataHiddenFamily.hs")
    let withFamily = unlines [if line == "import Prelude" then
          "import Prelude\nimport MetadataHiddenFamily ()" else line | line <- lines supportText]
    writeFile supportPath withFamily
    hiddenFamily <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing capturePath [] Nothing
    familyFixture <- capturePreparedFixture work hiddenFamily
    familyPath <- writeGenuineCandidateNativeScope [] (originalOwners hiddenFamily) work familyFixture
    familyDiagnostic <- neighborDiagnostic familyPath
    requireSourceSelectionInput "hidden family neighbor through scaffold support" familyDiagnostic $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just familyPath}) target [] Nothing
    writeFile supportPath supportText
    let metadataPath = work </> "CellCheck.hs"
    copyFile "test-source-boot/fixtures/GeneratedScaffoldMetadata.hs" metadataPath
    metadata <- sourceFailureDiagnostics $
      compile CheckedEnvironment Set.empty GeneralCompile (Just hidden) metadataPath [] Nothing
    case metadata of
      Left diagnostics | any (\diagnostic -> sourceDiagnosticAt metadataPath "Not in scope" diagnostic
          && "TidepoolResume.settle" `isInfixOf` dMessage diagnostic) diagnostics -> pure ()
      Left diagnostics -> fail ("authored metadata alias had another source failure: " ++ show diagnostics)
      Right _ -> fail "authored metadata acquired the generated scaffold alias"
    -- A current source implementation uses ordinary source admission, not the
    -- generated edge exception. It must not require a retained exact product.
    cold <- compile (PreparedProducts Nothing) Set.empty purpose Nothing target [] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult cold))) $
      fail "generated cold source scaffold failed ordinary support admission"
  putStrLn "generated scaffold: exact hidden support, settled result, ordinary/cold scope, bind/display CellProgram; duplicate/helper/source-drift/native/export/hidden-orphan/family/metadata refusals passed"

-- Native and lexical roots share one immutable compiler capture. The witness
-- is retained only as canonical interface/Core custody in the emitted scope.
captureRetainedCompilerFixture
  :: FilePath -> IO (PreparedPipelineResult, SessionScope, ExactScope, CapturedCompilerFixture)
captureRetainedCompilerFixture work =
  captureRetainedCompilerFixtureWith "MetadataRetainedWitness.hs" [work] work

captureRetainedCompilerFixtureWith
  :: FilePath -> [FilePath] -> FilePath -> IO (PreparedPipelineResult, SessionScope, ExactScope, CapturedCompilerFixture)
captureRetainedCompilerFixtureWith witnessFixture includes work = do
  forM_ ["MetadataQuoter.hs", "MetadataQuotedTarget.hs"
    , "MetadataCurrentSourceTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  copyFile ("test-source-boot/fixtures" </> witnessFixture) (work </> "MetadataRetainedWitness.hs")
  copyFile "test-source-boot/fixtures/MetadataQuoteSupportRetainedCore.hs"
    (work </> "MetadataQuoteSupport.hs")
  let source = work </> "MetadataQuoter.hs"
      helper = ("main", "MetadataQuoteSupport")
      witness = ("main", "MetadataRetainedWitness")
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing source includes Nothing
  originalFixture <- capturePreparedFixture work original
  scopePath <- writeGenuineExecutionScope [snd helper] [snd helper] work originalFixture
  let session = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
  exact <- readExactScope scopePath >>= either fail pure
  let proofs = scopeModuleInterfaceProofs exact
  unless (map originalModule (scopeProducts exact) == [snd helper]
      && Map.member witness proofs
      && isJust (Map.lookup witness proofs >>= canonicalCoreArtifact)
      && maybe False (Map.member witness . canonicalRequirements) (Map.lookup helper proofs)) $
    fail "retained fixture lost its interface/Core-only compiler dependency or acquired its native product"
  pure (original, session, exact, originalFixture)

retainedExecutionThCounter :: IO ()
retainedExecutionThCounter = withTiming $ withScratch $ \work -> do
  library <- canonicalizePath "lib"
  ((_, session, exact, _), initialDiagnostics) <- captureDiagnostics $
    captureRetainedCompilerFixtureWith "MetadataRetainedAuditedWitness.hs" [work,library] work
  let target = work </> "MetadataQuotedTarget.hs"
      proofs = scopeModuleInterfaceProofs exact
      witness = ("main", "MetadataRetainedWitness")
      retained = Map.keys proofs
      freshNames = Set.fromList (map mkModuleName ["MetadataQuoter", "MetadataQuotedTarget"])
      metaCount :: (String, String) -> String -> Int
      metaCount owner diagnostics = length
        [() | line <- lines diagnostics, "tidepool-meta-execution " `isPrefixOf` line
          , let fields = words line
          , ("owner_sha256=" ++ digest (TE.encodeUtf8 (T.pack (show owner)))) `elem` fields
          , ("unit=" ++ show (fst owner)) `elem` fields
          , ("module=" ++ show (snd owner)) `elem` fields
          , "owner_truncated=False" `elem` fields]
  unless (metaCount witness initialDiagnostics > 0
      && all (`Map.member` proofs)
        [("main", "Tidepool.QQ.Validate"), ("main", "Tidepool.Data.Text")]) $
    fail "audited retained original omitted its actual splice observation or genuine library closure"
  removeRetainedCompilerSources work exact
  withResidentPipelineSelected [work] $ \compile -> do
    (prepared, diagnostics) <- captureDiagnostics $ compile
      (PreparedProducts Nothing) Set.empty GeneralCompile (Just session) target [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult prepared))
        && counterValues "exact_execution_original_load_owners" diagnostics == [fromIntegral (length retained)]
        && counterValues "exact_execution_fresh_provider_compiles" diagnostics == [1]
        && Map.keysSet (pprFinalizedModules prepared) == freshNames
        && all (\(_,owner) -> Map.notMember (mkModuleName owner) (pprProductInterfaces prepared)) retained
        && all (\(_,owner) -> ("tidepool-canonical-frontend module=" ++ owner)
          `notElem` lines diagnostics) retained
        && all (\owner -> metaCount owner diagnostics == 0) retained
        && metaCount ("main", "MetadataQuotedTarget") diagnostics > 0
        && isNothing (runMetaHook (hsc_hooks (prHscEnv (pprPipelineResult prepared))))) $
      fail "retained audited original replayed TH, lost original42, or skipped the fresh target observation"
    putStrLn ("retained TH observations: original=" ++ show (metaCount witness initialDiagnostics)
      ++ " retained=" ++ show (sum [metaCount owner diagnostics | owner <- retained])
      ++ " fresh_target=" ++ show (metaCount ("main", "MetadataQuotedTarget") diagnostics))

removeRetainedCompilerSources :: FilePath -> ExactScope -> IO ()
removeRetainedCompilerSources work exact =
  forM_ ["MetadataQuoteSupport", "MetadataRetainedWitness"] $ \owner ->
    forM_ ["hs", "hi", "o", "dyn_hi", "dyn_o", "hie"] $ \extension -> do
      let path = work </> owner ++ "." ++ extension
      unless (path `notElem` [exactPath artifact | (artifact,_,_) <- scopeInterfaces exact]) $
        fail "ordinary compiler output is also an admitted immutable interface"
      exists <- doesFileExist path
      when exists (removeFile path)

requireRetainedCompilerResult :: PreparedPipelineResult -> String -> IO ()
requireRetainedCompilerResult prepared diagnostics = do
  let retained = map mkModuleName ["MetadataQuoteSupport", "MetadataRetainedWitness"]
      fresh = Set.fromList (map mkModuleName ["MetadataQuoter", "MetadataQuotedTarget"])
  unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult prepared))
      && counterValues "exact_execution_original_load_owners" diagnostics == [2]
      && counterValues "exact_execution_fresh_provider_compiles" diagnostics == [1]
      && Map.keysSet (pprFinalizedModules prepared) == fresh
      && all (`Map.notMember` pprProductInterfaces prepared) retained
      && all (\owner -> ("tidepool-canonical-frontend module=" ++ moduleNameString owner)
          `notElem` lines diagnostics) retained) $
    fail "retained compiler load changed original42, ran a retained frontend or republished a native owner"

retainedExecutionPublication :: IO ()
retainedExecutionPublication = withTiming $ withScratch $ \work -> do
  (original, session, exact, _) <- captureRetainedCompilerFixture work
  let helper = mkModuleName "MetadataQuoteSupport"
      helperPath = work </> "MetadataQuoteSupport.hs"
      targetPath = work </> "MetadataQuotedTarget.hs"
  scopePath <- maybe (fail "retained fixture omitted issued scope") pure (ssExactScope session)
  let originalEnvironment = prHscEnv (pprPipelineResult original)
      helperFlags = [ms_hspp_opts summary | ModuleNode _ summary <-
        mgModSummaries' (hsc_mod_graph originalEnvironment), ms_mod_name summary == helper]
  unless (case helperFlags of
      [flags] -> xopt LangExt.TypeFamilies flags
        && not (xopt LangExt.TypeFamilies (hsc_dflags originalEnvironment))
      _ -> False) $
    fail "retained support must require module flags absent from the session defaults"
  originalBytes <- BS.readFile scopePath
  deferredOriginalModuleFlags work session
  removeRetainedCompilerSources work exact
  withResidentPipelineSelected [work] $ \compile -> do
    (prepared, diagnostics) <- captureDiagnostics $ compile (PreparedProducts Nothing) Set.empty GeneralCompile
      (Just session) targetPath [work] Nothing
    requireRetainedCompilerResult prepared diagnostics
    let environment = prHscEnv (pprPipelineResult prepared)
    case lookupHpt (hsc_HPT environment) helper of
      Just hmi | isJust (homeMod_bytecode (hm_linkable hmi)) -> pure ()
      _ -> fail "retained original lost its authorized GHC bytecode"
    let captured = work </> "fresh-publication"
    createDirectory captured
    originals <- newOriginalInterfaceArtifacts environment (pprFinalizedModules prepared)
      [artifact | (artifact,_,_) <- scopeInterfaces exact] captured
    _ <- captureFinalizedModuleArtifacts originals environment (pprFinalizedModules prepared)
      (pprPackageImports prepared) (preparedFreshDependencies prepared) captured
    after <- BS.readFile scopePath
    unless (after == originalBytes) $ fail "execution mutated its original immutable admission"
    copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" helperPath
    fresh <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing helperPath [work] Nothing
    unless (Map.member helper (pprFinalizedModules fresh)
        && Map.member helper (pprProductInterfaces fresh)) $
      fail "ordinary source refresh inherited retained execution ownership"
    ordinary <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing targetPath [work] Nothing
    unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult ordinary))) $
      fail "ordinary source refresh did not execute the changed current helper"
    (capturedAgain, againDiagnostics) <- captureDiagnostics $ compile
      (PreparedProducts Nothing) Set.empty GeneralCompile (Just session) targetPath [work] Nothing
    requireRetainedCompilerResult capturedAgain againDiagnostics
    afterChangedSource <- BS.readFile scopePath
    unless (afterChangedSource == originalBytes) $
      fail "capture-only execution changed its immutable admission after disk source drift"
  putStrLn "retained execution publication: cold source-less original42, compiler dependency without native product, no retained frontend/native publication, current source43 separate"

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
  (original, scope, exact, originalFixture) <- captureRetainedCompilerFixture work
  let target = work </> "MetadataQuotedTarget.hs"
      helperPath = work </> "MetadataQuoteSupport.hs"
      quoterPath = work </> "MetadataQuoter.hs"
      cancelMarker = work </> "cancel-marker"
      helperKey = ("main", "MetadataQuoteSupport")
      witnessKey = ("main", "MetadataRetainedWitness")
      proofs = scopeModuleInterfaceProofs exact
  helperProof <- maybe (fail "captured helper lacks its canonical proof") pure (Map.lookup helperKey proofs)
  witnessProof <- maybe (fail "captured compiler dependency lacks its canonical proof") pure (Map.lookup witnessKey proofs)
  core <- maybe (fail "captured helper lacks its executable Core") pure (canonicalCoreArtifact helperProof)
  scratchRoot <- addTrailingPathSeparator <$> canonicalizePath work
  corePath <- canonicalizePath (canonicalCorePath core)
  unless (scratchRoot `isPrefixOf` corePath) $
    fail "retained Core mutation must remain inside this fixture's scratch directory"
  coreBytes <- BS.readFile (canonicalCorePath core)
  scopePath <- maybe (fail "retained fixture omitted issued scope") pure (ssExactScope scope)
  originalBytes <- BS.readFile scopePath
  hiddenPath <- writeGenuineExecutionScope [snd helperKey] [] work originalFixture
  let hiddenScope = scope {ssExactScope=Just hiddenPath}
  removeRetainedCompilerSources work exact
  withResidentPipelineSelectedRequests [work] $ \runRequest -> do
    runRequest (pure ()) $ \compile -> do
      checked <- compile CheckedEnvironment Set.empty GeneralCompile (Just scope) target [work] Nothing
      unless (fmap renderType (crResultType checked) == Just "Int") $
        fail "cold retained compiler execution changed its result type"
      (native,diagnostics) <- captureDiagnostics $
        compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [work] Nothing
      requireRetainedCompilerResult native diagnostics
    let missing = canonicalCorePath core ++ ".removed"
    backupExists <- doesFileExist missing
    when backupExists (fail "retained Core removal backup already exists")
    bracket (renameFile (canonicalCorePath core) missing)
      (const (renameFile missing (canonicalCorePath core))) $ \_ ->
        runRequest (pure ()) $ \compile -> do
          refused <- try (void (compile CheckedEnvironment Set.empty GeneralCompile
            (Just scope) target [work] Nothing)) :: IO (Either IOException ())
          unless (case refused of
            Left reason -> isDoesNotExistError reason && ioeGetFileName reason == Just (canonicalCorePath core)
            _ -> False) $
            fail "retained compiler execution did not refuse its exact missing Core path"
    bracket (BS.writeFile (canonicalCorePath core) (BSC.pack "corrupt retained Core"))
      (const (BS.writeFile (canonicalCorePath core) coreBytes)) $ \_ ->
        runRequest (pure ()) $ \compile -> do
          refused <- try (void (compile CheckedEnvironment Set.empty GeneralCompile
            (Just scope) target [work] Nothing)) :: IO (Either CandidateCoreFailure ())
          unless (refused == Left CandidateCoreBytesMismatch) $
            fail "retained compiler execution did not refuse corrupted authenticated Core bytes"
    -- Both certificate descriptors are genuine; only this negative association
    -- places the dependency's certificate under the helper's exact owner.
    let wrongOwner (TList fields)
          | take 2 fields == map (TString . T.pack) [fst helperKey,snd helperKey]
          , length fields == 8
          , TList role <- fields !! 7
          , length role == 5 = TList (take 7 fields ++ [TList
              [head role,TString (T.pack (canonicalCertificatePath witnessProof))
              ,TString (T.pack (canonicalCertificateSha256 witnessProof)),role !! 3,role !! 4]])
        wrongOwner row = row
    originalTerm <- case deserialiseFromBytes decodeTerm (BSL.fromStrict originalBytes) of
      Right (remaining, decoded) | BSL.null remaining -> pure decoded
      Right _ -> fail "genuine retained scope has trailing CBOR bytes"
      Left failure -> fail (show failure)
    wrongOwnerTerm <- case originalTerm of
      TList fields | length fields == 9 -> case fields !! 4 of
        TList rows -> pure (TList [if index == 4 then TList (map wrongOwner rows) else field
          | (index,field) <- zip [0::Int ..] fields])
        _ -> fail "genuine retained interface inventory changed framing"
      _ -> fail "genuine retained scope changed framing"
    _ <- readExactScope scopePath >>= either fail pure
    (do
      BS.writeFile scopePath (toStrictByteString (encodeTerm wrongOwnerTerm))
      wrongOwnerResult <- readExactScope scopePath
      case wrongOwnerResult of
        Left reason | "canonical module certificate differs from exact owner or payload" `isInfixOf` reason -> pure ()
        Left reason -> fail ("wrong-owner certificate failed for another reason: " ++ reason)
        Right _ -> fail "retained compiler proof accepted another genuine certificate under the wrong owner"
      ) `finally` BS.writeFile scopePath originalBytes
    _ <- readExactScope scopePath >>= either fail pure
    restoredScopeBytes <- BS.readFile scopePath
    unless (restoredScopeBytes == originalBytes) (fail "wrong-owner certificate control changed its genuine scope")
    runRequest (pure ()) $ \compile -> do
      copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" helperPath
      (captured,diagnostics) <- captureDiagnostics $
        compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [work] Nothing
      requireRetainedCompilerResult captured diagnostics
      -- This is a real authored current-source demand, separate from capture.
      -- This fixture has no checked include authority and must refuse before
      -- considering changed current source; it does not qualify drift checks.
      parsed <- analyzeCellWithFlags (hsc_dflags (prHscEnv (pprPipelineResult original))) ""
        "import MetadataQuoteSupport (answerValue)\nanswerValue" >>= either (fail . show) pure
      let current = withSourceImportIntents (cellPlanPrologue parsed) GeneralCompile
      refused <- try (void (compile CheckedEnvironment Set.empty current (Just scope)
        (work </> "MetadataCurrentSourceTarget.hs") [work] Nothing)) :: IO (Either InputRejection ())
      case refused of
        Left (OriginalSourceSelectionRejected (ExecutionSourceUnavailable key)) | key == helperKey -> pure ()
        Left reason -> fail ("current import had another authority refusal: " ++ show reason)
        Right () -> fail "capture-only scope acquired checked current-source authority"
    runRequest (pure ()) $ \compile -> do
      let preprocessor = work </> "unadmitted-preprocessor"
          marker = work </> "preprocess-marker"
      writeFile preprocessor ("#!/bin/sh\n: > " ++ show marker ++ "\nexit 1\n")
      permissions <- getPermissions preprocessor
      setPermissions preprocessor permissions {executable=True}
      source <- readFile "test-source-boot/fixtures/ExecutionChangedPreprocessor.hs"
      writeFile helperPath (T.unpack (T.replace "EXECUTION_PREPROCESSOR" (T.pack preprocessor) (T.pack source)))
      (captured,diagnostics) <- captureDiagnostics $
        compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [work] Nothing
      requireRetainedCompilerResult captured diagnostics
      ran <- doesFileExist marker
      when ran (fail "retained Core execution replayed changed original preprocessing")
    runRequest (pure ()) $ \compile -> do
      source <- readFile "test-source-boot/fixtures/ExecutionCancellingQuoter.hs"
      writeFile quoterPath (T.unpack (T.replace "\"EXECUTION_CANCEL_MARKER\"" (T.pack (show cancelMarker)) (T.pack source)))
      caller <- myThreadId
      let waitForSplice = do
            started <- doesFileExist cancelMarker
            unless started (threadDelay 10000 >> waitForSplice)
          cancelAtSplice = do
            started <- timeout 60000000 waitForSplice
            case started of
              Just () -> throwTo caller ThreadKilled
              Nothing -> throwTo caller (userError "execution cancellation splice did not start")
      -- The watchdog bounds failure to reach the splice. Cancellation itself
      -- follows its real marker, independently of compiler startup duration.
      cancelled <- bracket (forkIO cancelAtSplice) killThread $ \_ ->
        try (void (compile CheckedEnvironment Set.empty GeneralCompile
          (Just scope) target [work] Nothing)) :: IO (Either SomeException ())
      began <- doesFileExist cancelMarker
      unless (began && case cancelled of
          Left reason -> fromException reason == Just ThreadKilled
          Right () -> False) $
        fail "execution cancellation did not reach the scoped splice linker"
      requireFailedCompilerTransaction "retained exact quoter cancellation" $
        timeout 1000000 (compile CheckedEnvironment Set.empty GeneralCompile
          (Just scope) target [work] Nothing)
    runRequest (pure ()) $ \compile -> do
      copyFile "test-source-boot/fixtures/MetadataQuoter.hs" quoterPath
      (recovered,diagnostics) <- captureDiagnostics $
        compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [work] Nothing
      requireRetainedCompilerResult recovered diagnostics
      when (isJust (hscCompileCoreExprHook (hsc_hooks (prHscEnv (pprPipelineResult recovered))))) $
        fail "cancelled retained execution leaked its linker hook into the next request"
      hidden <- try (void (compile CheckedEnvironment Set.empty GeneralCompile
        (Just hiddenScope) target [work] Nothing)) :: IO (Either InputRejection ())
      case hidden of
        Left (OriginalSourceSelectionRejected (ExecutionSourceUnavailable key)) | key == helperKey -> pure ()
        Left reason -> fail ("hidden import had another authority refusal: " ++ show reason)
        Right () -> fail "native/Core custody exposed its unadmitted lexical owner"
  restored <- BS.readFile (canonicalCorePath core)
  after <- BS.readFile scopePath
  unless (restored == coreBytes && after == originalBytes) $
    fail "retained compiler refusal/recovery changed immutable admission bytes"
  putStrLn "exact retained quoter: cold source-less original42, Core-only compiler dependency, missing/corrupt/wrong-owner Core refusal, capture-only source drift, no preprocessing/frontend replay, explicit current/hidden authority refusal, cancellation/recovery"

-- Execution custody comes from actual captured originals and the Rust issuer.
exactReexportQuoter :: IO ()
exactReexportQuoter = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs","MetadataQuoter.hs","ExecutionReexportFacade.hs"
    ,"ExecutionReexportTarget.hs","MetadataCurrentSourceTarget.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ExecutionReexportFacade.hs") [work] Nothing
  originalFixture <- capturePreparedFixture work original
  scopePath <- writeExecutionScope work originalFixture ["ExecutionReexportFacade"]
  let scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
  withResidentPipelineSelected [work] $ \compile -> do
    (result,diagnostics) <- captureDiagnostics (compile (PreparedProducts Nothing) Set.empty GeneralCompile
      (Just scope) (work </> "ExecutionReexportTarget.hs") [work] Nothing)
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult result))) $
      fail "retained facade did not execute its original defining quoter"
    unless (counterValues "exact_execution_original_load_owners" diagnostics == [2]) $
      fail "reexport fixture did not select exactly the defining quoter and pure helper"
    let environment = prHscEnv (pprPipelineResult result)
        retainedBytecode owner = case lookupHpt (hsc_HPT environment) (mkModuleName owner) of
          Just hmi -> mi_module (hm_iface hmi) == mkModule (stringToUnit "main") (mkModuleName owner)
            && isJust (homeMod_bytecode (hm_linkable hmi))
            && isNothing (homeMod_object (hm_linkable hmi))
          Nothing -> False
        facadeLinkable = hm_linkable <$> lookupHpt (hsc_HPT environment)
          (mkModuleName "ExecutionReexportFacade")
    unless (all retainedBytecode ["MetadataQuoter","MetadataQuoteSupport"]
        && maybe False (\linkable -> isNothing (homeMod_bytecode linkable)
          && isNothing (homeMod_object linkable)) facadeLinkable
        && isNothing (hscCompileCoreExprHook (hsc_hooks environment))) $
      fail "reexport execution lost original bytecode, loaded the facade, or retained a linker hook"
    copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
    changed <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionReexportTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult changed))) $
      fail "captured reexport changed meaning with its former helper source"
    parsed <- analyzeCellWithFlags (hsc_dflags environment) ""
      "import MetadataQuoteSupport (answerValue)\nanswerValue" >>= either (fail . show) pure
    let current = withSourceImportIntents (cellPlanPrologue parsed) GeneralCompile
    currentRefusal <- try (void (compile CheckedEnvironment Set.empty current (Just scope)
      (work </> "MetadataCurrentSourceTarget.hs") [work] Nothing)) :: IO (Either InputRejection ())
    case currentRefusal of
      Left (OriginalSourceSelectionRejected (ExecutionSourceUnavailable ("main","MetadataQuoteSupport"))) -> pure ()
      Left reason -> fail ("current-source demand had another authority refusal: " ++ show reason)
      Right () -> fail "captured reexport acquired unadmitted current-source authority"
    copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
    recovered <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionReexportTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult recovered))) $
      fail "failed reexport admission leaked its execution targets into the next cycle"
  putStrLn "execution reexport: thin facade selects only quoter and helper, executes, and restores load targets"

data ExecutionInstanceFixture = ExecutionInstanceFixture
  { instanceFixtureRoot :: FilePath
  , instanceSealedCapture :: CapturedCompilerFixture
  , instanceClassCapture :: CapturedCompilerFixture
  }

acquireExecutionInstanceFixture :: IO ExecutionInstanceFixture
acquireExecutionInstanceFixture = mask $ \restore -> do
  root <- acquireFixtureScratch
  (restore $ withScratchFailureEvidence root $ do
    forM_ ["ExecutionClass.hs","ExecutionHiddenOrphan.hs","ExecutionSealedQuoter.hs"
      ,"ExecutionClassQuoter.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (root </> name)
    sealed <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing (root </> "ExecutionSealedQuoter.hs") [root] Nothing
    sealedCapture <- capturePreparedFixture root sealed
    classOriginal <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing (root </> "ExecutionClassQuoter.hs") [root] Nothing
    classCapture <- capturePreparedFixture root classOriginal
    pure (ExecutionInstanceFixture root sealedCapture classCapture))
    `onException` releaseFixtureScratchAfterFailure root

releaseExecutionInstanceFixture :: ExecutionInstanceFixture -> IO ()
releaseExecutionInstanceFixture = releaseFixtureScratch . instanceFixtureRoot

withExecutionInstanceFixture
  :: IO ExecutionInstanceFixture -> (FilePath -> SessionScope -> SessionScope -> IO a) -> IO a
withExecutionInstanceFixture getFixture action = do
  fixture <- getFixture
  withScratchFailureEvidence (instanceFixtureRoot fixture) $ withTiming $ withScratch $ \work -> do
    forM_ ["ExecutionFreshQuoter.hs","ExecutionSealedTarget.hs","ExecutionFreshTarget.hs"
      ,"ExecutionQualifiedTarget.hs","ExecutionHiddenQuoteTarget.hs"
      ,"ExecutionClassQuoteTarget.hs","ExecutionClassQuoteHidden.hs"
      ,"ExecutionUnrelatedQuoter.hs"
      ,"ExecutionExplicitOrphanQuoter.hs","ExecutionExplicitOrphanTarget.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
    scopePath <- writeExecutionScope work (instanceSealedCapture fixture) ["ExecutionSealedQuoter"]
    classScopePath <- writeExecutionScope work (instanceClassCapture fixture) ["ExecutionClassQuoter"]
    let scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
        classScope = scope {ssExactScope=Just classScopePath}
    admitted <- readExactScope scopePath >>= either fail pure
    let imports owner = Map.findWithDefault [] ("main",owner) (Map.fromList (scopeLexical admitted))
    unless (("main","ExecutionHiddenOrphan") `elem` imports "ExecutionSealedQuoter"
        && null (imports "ExecutionClass")) $
      fail "instance fixture lost its genuine transitive orphan edge or added an orphan below the class"
    action work scope classScope

exactExecutionSealedInstance :: IO ExecutionInstanceFixture -> IO ()
exactExecutionSealedInstance getFixture = withExecutionInstanceFixture getFixture $ \work scope _ ->
  withResidentPipelineSelected [work] $ \compile -> do
    forM_ ["ExecutionSealedTarget.hs","ExecutionQualifiedTarget.hs"] $ \target -> do
      sealed <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) (work </> target) [work] Nothing
      let owner = takeBaseName target
      requireExactProviderImport owner "ExecutionSealedQuoter" sealed
      requirements <- either fail pure (preparedHomeRequirements sealed "main" owner)
      unless (requirements == [("main","ExecutionSealedQuoter")]) $
        fail "sealed or qualified target acquired another home requirement"
      unless (fmap renderType (prResultType (pprPipelineResult sealed)) == Just "Int"
          && hasIntResultLiteral 42 (prBinds (pprPipelineResult sealed))) $
        fail "sealed or qualified original quoter lost its authenticated orphan dictionary"
    let target = work </> "ExecutionHiddenQuoteTarget.hs"
    (hidden,diagnostics) <- captureDiagnostics (try (compile CheckedEnvironment Set.empty GeneralCompile
      (Just scope) target [work] Nothing) :: IO (Either SourceError CheckedEnvironmentResult))
    case hidden of
      Left reason | any (sourceDiagnosticAt target "Not in scope") (diagsFromSourceError reason)
        , counterValues "exact_execution_original_load_owners" diagnostics == [0] -> pure ()
      Left reason -> fail ("hidden quoter failed for another reason: " ++ show reason)
      Right _ -> fail "hidden qualified export acquired an execution recipe"

exactExecutionTransitiveInstance :: IO ExecutionInstanceFixture -> IO ()
exactExecutionTransitiveInstance getFixture = withExecutionInstanceFixture getFixture $ \work scope _ -> do
  result <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
    (work </> "ExecutionFreshTarget.hs") [work] Nothing
  requireInstanceQuoterResult "transitive orphan" result
  requireExactProviderImport "ExecutionFreshQuoter" "ExecutionSealedQuoter" result

exactExecutionUnrelatedInstance :: IO ExecutionInstanceFixture -> IO ()
exactExecutionUnrelatedInstance getFixture = withExecutionInstanceFixture getFixture $ \work scope _ ->
  withResidentPipelineSelected [work] $ \compile -> do
    -- Ordinary GHC supplies the independent source-visibility oracle. Its
    -- inputs live apart from retained artifacts and exact hydration.
    withScratch $ \oracle -> do
      forM_ ["ExecutionClass.hs","ExecutionHiddenOrphan.hs","ExecutionSealedQuoter.hs"
        ,"ExecutionExplicitOrphanQuoter.hs","ExecutionExplicitOrphanTarget.hs"] $ \name ->
        copyFile ("test-source-boot/fixtures" </> name) (oracle </> name)
      let oracleTarget = oracle </> "ExecutionExplicitOrphanTarget.hs"
          oracleProvider = oracle </> "ExecutionExplicitOrphanQuoter.hs"
      ordinary <- runPipelineSelected (PreparedProducts Nothing) oracleTarget [oracle]
      requireInstanceQuoterResult "ordinary explicit orphan import" ordinary
      writeUnrelatedProvider work oracleProvider
      refused <- sourceFailureDiagnostics (runPipelineSelected (PreparedProducts Nothing) oracleTarget [oracle])
      case refused of
        Left diagnostics | any (sourceDiagnosticAt oracleProvider "No instance for") diagnostics -> pure ()
        Left diagnostics -> fail ("ordinary GHC failed for another source reason: " ++ show diagnostics)
        Right _ -> fail "ordinary GHC made a lexically unrelated orphan visible to the provider"
    -- The explicit-import control has the same class and dictionary demand.
    -- Removing only that provider's orphan edge must prevent resolution even
    -- while the target demands bytecode from the sealed original quoter.
    explicit <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionExplicitOrphanTarget.hs") [work] Nothing
    requireInstanceQuoterResult "exact explicit orphan import" explicit
    mixed <- either fail pure (preparedHomeRequirements explicit "main" "ExecutionExplicitOrphanTarget")
    fresh <- either fail pure (selectedFreshHomeRequirements (preparedFreshDependencies explicit)
      "main" "ExecutionExplicitOrphanTarget")
    unless (fresh == [("main","ExecutionExplicitOrphanQuoter")]
        && sort mixed == [("main","ExecutionExplicitOrphanQuoter"),("main","ExecutionSealedQuoter")]) $
      fail "mixed source and exact imports lost their separate views or complete requirements"
    requireExactProviderImport "ExecutionExplicitOrphanQuoter" "ExecutionHiddenOrphan" explicit
    -- Use the same provider module in both observations. This makes the
    -- missing import the sole source difference and exercises warm isolation.
    writeUnrelatedProvider work (work </> "ExecutionExplicitOrphanQuoter.hs")
    let provider = work </> "ExecutionExplicitOrphanQuoter.hs"
        target = work </> "ExecutionExplicitOrphanTarget.hs"
    exact <- maybe (fail "instance fixture omitted its issued scope") readExactScope (ssExactScope scope)
      >>= either fail pure
    let sealedOwner = ("main","ExecutionSealedQuoter")
        expectedOwners = Set.fromList [("main",owner)
          | owner <- ["ExecutionClass","ExecutionHiddenOrphan","ExecutionSealedQuoter"]]
    -- The consumed target retains canonical Core authority, but has no optional
    -- source replay recipe. Bytecode demand follows canonical interface seals.
    case executionSourceClosure (scopeExecutionGraphs exact) (scopeExecutionOwners exact)
        (scopeExecutionNativeOwners exact) [sealedOwner] of
      Left (ExecutionSourceMissing owner) | owner == sealedOwner -> pure ()
      Left reason -> fail ("sealed target source replay refused for another reason: " ++ show reason)
      Right _ -> fail "consumed sealed target unexpectedly received a source replay recipe"
    expectedOriginals <- either (fail . show) pure (retainedCompilerArtifactClosure exact
      [artifact | (artifact,_,_) <- scopeInterfaces exact] [sealedOwner])
    let actualOwners = Set.fromList [(exactUnit artifact,exactModule artifact)
          | selected <- expectedOriginals, let artifact = retainedCompilerInterface selected]
        proofs = scopeModuleInterfaceProofs exact
        hasCore owner = isJust (Map.lookup owner proofs >>= canonicalCoreArtifact)
        environment = prHscEnv (pprPipelineResult explicit)
        hasBytecode (unit,owner) = case lookupHpt (hsc_HPT environment) (mkModuleName owner) of
          Just home -> mi_module (hm_iface home) == mkModule (stringToUnit unit) (mkModuleName owner)
            && isJust (homeMod_bytecode (hm_linkable home))
          Nothing -> False
    unless (actualOwners == expectedOwners && all hasCore (Set.toList actualOwners)
        && all hasBytecode (Set.toList expectedOwners)) $
      fail ("sealed canonical execution closure lost its admitted owners, Core or loaded bytecode: "
        ++ show actualOwners)
    let orphanOwner = ("main","ExecutionHiddenOrphan")
        withoutOrphan = [artifact | (artifact,_,_) <- scopeInterfaces exact
          , (exactUnit artifact,exactModule artifact) /= orphanOwner]
    case retainedCompilerArtifactClosure exact withoutOrphan [sealedOwner] of
      Left (FinalizedExecutionDependencyMismatch owner dependency)
        | owner == sealedOwner, dependency == orphanOwner -> pure ()
      Left reason -> fail ("missing canonical orphan refused for another reason: " ++ show reason)
      Right _ -> fail "canonical bytecode selection bypassed its exact orphan dependency seal"
    sealedArtifact <- case [artifact | (artifact,_,_) <- scopeInterfaces exact
        , (exactUnit artifact,exactModule artifact) == sealedOwner] of
      [artifact] -> pure artifact
      _ -> fail "sealed canonical execution owner is missing or duplicated"
    let artifacts = [artifact | (artifact,_,_) <- scopeInterfaces exact]
        wrongRootSeal artifact
          | (exactUnit artifact,exactModule artifact) == sealedOwner = artifact {exactSha256=replicate 64 '0'}
          | otherwise = artifact
    case retainedCompilerArtifactClosure exact (map wrongRootSeal artifacts) [sealedOwner] of
      Left (FinalizedExecutionInterfaceMismatch owner) | owner == sealedOwner -> pure ()
      Left reason -> fail ("changed canonical root seal refused for another reason: " ++ show reason)
      Right _ -> fail "canonical bytecode selection admitted another root interface seal"
    case retainedCompilerArtifactClosure exact (sealedArtifact : artifacts) [sealedOwner] of
      Left (FinalizedExecutionDuplicateOwner owner) | owner == sealedOwner -> pure ()
      Left reason -> fail ("duplicate canonical owner refused for another reason: " ++ show reason)
      Right _ -> fail "canonical bytecode selection silently selected a duplicate owner"
    (rejected,diagnostics) <- captureDiagnostics (sourceFailureDiagnostics
      (compile CheckedEnvironment Set.empty GeneralCompile (Just scope) target [work] Nothing))
    case rejected of
      Left errors | any (sourceDiagnosticAt provider "No instance for") errors
        , counterValues "exact_execution_original_load_owners" diagnostics == [toInteger (length expectedOriginals)] -> pure ()
      Left errors -> fail ("unrelated provider failed for another source reason: " ++ show errors ++ "\n" ++ diagnostics)
      Right _ -> fail "unrelated provider borrowed an orphan from the execution linker graph"
    restored <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionSealedTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult restored))) $
      fail "unrelated-provider refusal leaked its execution environment"
    copyFile "test-source-boot/fixtures/ExecutionExplicitOrphanQuoter.hs" provider
    recovered <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [work] Nothing
    requireInstanceQuoterResult "restored explicit orphan import" recovered
    requireExactProviderImport "ExecutionExplicitOrphanQuoter" "ExecutionHiddenOrphan" recovered

-- ExactCompilation owns imports to retained interfaces. Its separate fresh
-- dependency graph contains source-lookup edges after those imports are removed.
requireExactProviderImport :: String -> String -> PreparedPipelineResult -> IO ()
requireExactProviderImport provider imported prepared = do
  compilation <- maybe (fail "instance provider lost its exact compilation evidence") pure
    (preparedExactCompilation prepared)
  requirements <- either fail pure (preparedHomeRequirements prepared "main" provider)
  unless (("main",imported) `elem` requirements) $
    fail ("complete provider requirements omitted its authored exact import: " ++ provider ++ " -> " ++ imported)
  let expected = (DependencyUnqualified,imported,False,"main")
      exactRows = [edges | (("main",owner,False),edges) <- compilationExactImports compilation
        , owner == provider]
      freshRows = [node | node <- dependencyModules (preparedFreshDependencies prepared)
        , dependencyModuleUnit node == "main", dependencyModuleName node == provider
        , not (dependencyModuleBoot node)]
  unless (case (exactRows,freshRows) of
      ([edges],[node]) -> expected `elem` edges
        && all ((/= imported) . dependencyImportName) (dependencyModuleImports node)
      _ -> False) $
    fail ("instance provider lost its authored exact import or confused fresh source lookup"
      ++ ": provider=" ++ provider ++ " expected=" ++ show expected
      ++ " exact=" ++ show exactRows
      ++ " fresh=" ++ show [map dependencyImportName (dependencyModuleImports node) | node <- freshRows])

-- These provider fixtures check the actual dictionary result inside quoteExp.
-- GHC must execute that check to produce the Int expression; floated Core
-- bindings and target optimization do not define the source-visibility oracle.
requireInstanceQuoterResult :: String -> PreparedPipelineResult -> IO ()
requireInstanceQuoterResult label prepared = do
  let result = pprPipelineResult prepared
  unless (maybe False (`eqType` intTy) (prResultType result)) $
    fail (label ++ ": checked quoter expression did not retain its GHC Int type")

exactExecutionClassInstance :: IO ExecutionInstanceFixture -> IO ()
exactExecutionClassInstance getFixture = withExecutionInstanceFixture getFixture $ \work _ scope ->
  withResidentPipelineSelected [work] $ \compile -> do
    result <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionClassQuoteTarget.hs") [work] Nothing
    unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult result))) $
      fail "class parent wildcard import lost its exported quoter method"
    let target = work </> "ExecutionClassQuoteHidden.hs"
    (hidden,diagnostics) <- captureDiagnostics (try (compile CheckedEnvironment Set.empty GeneralCompile
      (Just scope) target [work] Nothing) :: IO (Either SourceError CheckedEnvironmentResult))
    case hidden of
      Left reason | any (sourceDiagnosticAt target "Not in scope") (diagsFromSourceError reason)
        , counterValues "exact_execution_original_load_owners" diagnostics == [0] -> pure ()
      Left reason -> fail ("hidden class child failed for another reason: " ++ show reason)
      Right _ -> fail "hiding a class parent acquired its child quoter execution capability"

writeUnrelatedProvider :: FilePath -> FilePath -> IO ()
writeUnrelatedProvider work destination = do
  unrelated <- readFile (work </> "ExecutionUnrelatedQuoter.hs")
  writeFile destination (T.unpack (T.replace "ExecutionUnrelatedQuoter" "ExecutionExplicitOrphanQuoter" (T.pack unrelated)))

-- GHC can report a dependency source error through its logger and return
-- Failed, or throw SourceError directly. Worker and filesystem failures escape.
sourceFailureDiagnostics :: IO a -> IO (Either [Diag] a)
sourceFailureDiagnostics action = (Right <$> action) `catches`
  [ Handler (pure . Left . diagsFromSourceError)
  , Handler dependencyFailure
  ]
  where
    dependencyFailure (DependencySourceFailure diagnostics) = pure (Left diagnostics)
    dependencyFailure failure = throwIO failure

sourceDiagnosticAt :: FilePath -> String -> Diag -> Bool
sourceDiagnosticAt path fragment diagnostic = dSeverity diagnostic == DiagError
  && fragment `isInfixOf` dMessage diagnostic
  && case dFile diagnostic of
    Just (actual,_,_,_,_) -> normalise actual == normalise path
    Nothing -> False

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
  sealedFixture <- capturePreparedFixture work sealed
  helper <- runPipelineSelected (PreparedProducts Nothing) (work </> "MetadataQuoteSupport.hs") [work]
  helperFixture <- capturePreparedFixture work helper
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
  scopePath <- writeExecutionScope work sealedFixture ["ExecutionSealedQuoter"]
  hiddenPath <- writeExecutionScope work sealedFixture []
  helperPath <- writeExecutionScope work helperFixture ["MetadataQuoteSupport"]
  let scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
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
      warmExact <- compile CheckedEnvironment Set.empty GeneralCompile
        (Just scope {ssExactScope=Just helperPath}) (work </> "MetadataQuotedTarget.hs") [work] Nothing
      unless (maybe False (`eqType` intTy) (crResultType warmExact)) $
        fail "exact cancellation fixture did not first establish its reusable environment"
      cancelling <- readFile "test-source-boot/fixtures/ExecutionCancellingQuoter.hs"
      let marker = work </> "cancel-marker"
      writeFile (work </> "MetadataQuoter.hs") (T.unpack (T.replace "EXECUTION_CANCEL_MARKER" (T.pack marker) (T.pack cancelling)))
      cancelled <- timeout 1500000 (compile CheckedEnvironment Set.empty GeneralCompile
        (Just scope {ssExactScope=Just helperPath}) (work </> "MetadataQuotedTarget.hs") [work] Nothing)
      started <- doesFileExist marker
      unless (isNothing cancelled && started) (fail "exact cancellation did not reach the real quoter")
      requireFailedCompilerTransaction "exact to ordinary cancellation" $
        timeout 1000000 (compile CheckedEnvironment Set.empty GeneralCompile
          (Just scope {ssExactScope=Just helperPath}) (work </> "MetadataQuotedTarget.hs") [work] Nothing)
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
  let marker = work </> "checked-value-quoter-executed"
      target = work </> "CheckedValueQuoterTarget.hs"
      quoter = work </> "MetadataQuoter.hs"
      ordinaryTarget = work </> "CheckedValueQuoterControl.hs"
  source <- TE.decodeUtf8 <$> BS.readFile quoter
  writeFile quoter (T.unpack (T.replace
    "quoteExp = \\_ -> pure"
    (T.pack ("quoteExp = \\_ -> runIO (writeFile " ++ show marker ++ " \"executed\") >> pure"))
    (T.replace "import MetadataQuoteSupport" "import Language.Haskell.TH.Syntax (runIO)\nimport MetadataQuoteSupport" source)))
  writeFile ordinaryTarget $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module CheckedValueQuoterControl where"
    , "import MetadataQuoter (answer)"
    , "__result :: Int"
    , "__result = [answer|quoted|]"
    ]
  produced <- runPipelineSelected (PreparedProducts Nothing) (work </> "CheckedValueQuoterProducer.hs") [work]
  let result = pprPipelineResult produced
  valueOwner <- maybe (fail "invalid checked value fixture owner") pure (parseValModule "Tidepool.Session.Val.G8")
  _ <- mkBoundBinders ["answer"] 8 work result
  let valuePath = sessionHiPath work valueOwner
  bytes <- BS.readFile valuePath
  let value = ExactIfaceArtifact "main" "Tidepool.Session.Val.G8" valuePath (digest bytes) []
  scopePath <- writeGenuineEmptyMetadataScope work
  base <- readExactScope scopePath >>= either fail pure
  let admitted = base {scopePurpose=ExactCellPurpose (CheckedCellAdmission (replicate 64 '0') (replicate 64 '0')
        (replicate 64 '0') [] ["Tidepool.Session.Val.G8"] [] [value] Nothing AuthoredCellCheck) [work]}
      scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath,ssValIfaces=[valueOwner]}
  withResidentPipelineSelected [work] $ \compile -> do
    positive <- compile CheckedEnvironment Set.empty GeneralCompile Nothing ordinaryTarget [work] Nothing
    case crResultType positive of
      Just inferred | eqType inferred intTy -> pure ()
      _ -> fail "ordinary positive control lost the real quoter's result type"
    positiveRan <- doesFileExist marker
    unless positiveRan (fail "ordinary positive control did not execute the instrumented quoter")
    removeFile marker
    refused <- try (compile CheckedEnvironment Set.empty
      (CellProgramCompile GeneralCompile admitted) (Just scope) target [work] Nothing)
      :: IO (Either SomeException CheckedEnvironmentResult)
    case refused of
      Left reason | fromException reason == Just (FinalizedExecutionOwnerMissing ("main", "Tidepool.Session.Val.G8")) -> pure ()
      Left reason -> fail ("checked value execution lost its typed owner refusal: " ++ show reason)
      Right _ -> fail "checked value quoter entered GHC execution without a canonical original owner"
    ran <- doesFileExist marker
    when ran (fail "checked value refusal ran the real quoter")
    writeFile target $ unlines
      [ "{-# LANGUAGE QuasiQuotes #-}"
      , "module CheckedValueQuoterTarget where"
      , "import Tidepool.Session.Val.G8 (answer)"
      , "__result :: Int"
      , "__result = 7"
      ]
    recovered <- compile CheckedEnvironment Set.empty
      (CellProgramCompile GeneralCompile admitted) (Just scope) target [work] Nothing
    case crResultType recovered of
      Just inferred | eqType inferred intTy -> pure ()
      _ -> fail "checked value execution refusal prevented an extension-only metadata retry"
    ranAfterRetry <- doesFileExist marker
    when ranAfterRetry (fail "extension-only metadata retry executed its unused quoter")
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
      sourceProducts = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env (pprProductInterfaces original) retained mempty modules)
      coldProducts = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env (pprProductInterfaces original) context mempty modules)
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
        homeProducts = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env (pprProductInterfaces original) live mempty modules)
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
        (preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env map' retained mempty modules))
  unless (fallback (Map.delete supportName paired) == lookup supportOwner executable
      && fallback (Map.insert supportName
        (set_mi_module (mkModule (stringToUnit "wrong-home-unit") supportName) supportInterface) paired)
          == lookup supportOwner executable) $
    fail "missing or wrong-unit native interface granted original product purpose"
  let incomplete = [prepared {pmCoverage=ExactBodySubset} | prepared <- modules]
      conservative = preparedModuleProductOutcomes (projectOriginalHomeModuleProducts env paired retained mempty incomplete)
  unless (any (isJust . globalRequiredGeneration) (globals conservative)) $
    fail "incomplete prepared coverage acquired generation-free original package requirements"
  target <- either (fail . show) pure (projectPrepared retained modules)
  coldCertificate <- certifyProjectedProducts work "cold" original coldProducts [] env >>= either fail pure
  originalOnly <- certifyProjectedProducts work "original-only" original sourceProducts [] env >>= either fail pure
  mixed <- certifyProjectedProducts work "mixed" original sourceProducts [("target",target)] env >>= either fail pure
  let packageOwner = \case CodecPackageOwner {} -> True; _ -> False
      retainedOwner = \case CodecRetainedPackageOwner {} -> True; _ -> False
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
  let target = output </> "OriginalPackageCohort.hs"
  copyFile "test-source-boot/fixtures/OriginalPackageCohort.hs" target
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing target [output,coreRoot,"lib"] Nothing
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
        (projectOriginalHomeModuleProducts env (pprProductInterfaces original) retained mempty modules)
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
      evidence = preparedFreshDependencies original
  unless (dependencyCacheSafe evidence && dependencySelectionComplete evidence) $
    fail "actual original source cohort lacks complete tracked evidence"
  forM_ names $ \name -> do
    required <- either fail pure (selectedFreshHomeRequirements evidence "main" name)
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
  moduleResults <- forM outcomes $ \(owner,projected) -> do
    let name = moduleNameString (moduleName owner)
        path = work </> (label ++ "-" ++ name ++ ".hi")
        key = (unitString (moduleUnit owner), name)
        interface = Map.lookup (moduleName owner) (pprProductInterfaces original)
          >>= \iface -> if mi_module iface == owner then Just iface else Nothing
    case interface of
      Nothing -> pure (key, ProductMissingInterface, Nothing)
      Just _ | Left _ <- projected -> pure (key, ProductProjectionRejected, Nothing)
      Just iface -> do
        groups <- either (fail . show) pure projected
        writeBinIface (targetProfile (hsc_dflags env)) QuietBinIFace NormalCompression path iface
        bytes <- BS.readFile path
        pure (key, ProductReady, Just (T.pack (fst key), T.pack name, bytes, groups))
  let fresh = [product | (_, ProductReady, Just product) <- moduleResults]
      availability = Map.fromList [(key, status) | (key, status, _) <- moduleResults]
      evidence = (preparedFreshDependencies original) {dependencyModules =
        [node {dependencyModuleProduct = Map.findWithDefault
          (dependencyModuleProduct node)
          (dependencyModuleUnit node, dependencyModuleName node) availability}
        | node <- dependencyModules (preparedFreshDependencies original)]}
      bytes = encodeModuleProducts fresh
  originals <- newOriginalInterfaceArtifacts env (pprFinalizedModules original) [] work
  finalized <- captureFinalizedModuleArtifacts originals env (pprFinalizedModules original)
    (pprPackageImports original) evidence work
  encodeCertifiedProducts env OrdinaryExecutionSource (pprProductInterfaces original) finalized [] Nothing fresh targets evidence bytes
    (BSC.pack (renderDependencyEvidence evidence))

certificateFacts :: BS.ByteString -> IO CertificateCodecFacts
certificateFacts bytes = withScratch $ \work -> do
  let path = work </> "compiler-products.cbor"
  BS.writeFile path bytes
  readCertificateCodecFacts work path

certificateOwners :: BS.ByteString -> IO [CodecImportOwner]
certificateOwners bytes = codecCertificateOwners <$> certificateFacts bytes

verifyOriginalOnlyPackageRefusal :: FilePath -> PreparedPipelineResult
  -> [(Module, Either ProjectionError [ProjectedGroup])] -> BS.ByteString -> IO ()
verifyOriginalOnlyPackageRefusal work original outcomes certified = do
  facts <- certificateFacts certified
  (unit,name,path) <- case codecCertificatePackages facts of
    (unit,name,path,_):_ | null (codecCertificateTargets facts) -> pure (unit,name,path)
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
  facts <- certificateFacts bytes
  unless (codecCertificateModules facts == [(candidateUnit candidate,name,[91,3])]
      && null (codecCertificateTargets facts) && null (codecCertificatePackages facts)
      && null (codecCertificateOwners facts)) $
    fail "certified module grouping changed nonmonotone order or acquired another inventory"
  let absent = candidate {candidateModule="MissingOriginalInterface"}
  encodeCertifiedProducts env OrdinaryExecutionSource Map.empty (emptyFinalizedModuleArtifacts env) [absent] Nothing [] [] emptyEvidence BS.empty BS.empty >>= \case
    Left _ -> pure ()
    Right _ -> fail "cached original without its exact interface acquired a certificate"

-- Synthetic descriptors exercise producer group framing only. This decoder
-- does not grant them a durable certificate or compiler admission.
structuralCandidate :: FilePath -> IO ModuleCandidate
structuralCandidate work = do
  path <- writeCandidateCodecFixture work SingleGroupCandidateInventory
  readModuleCandidates path >>= \case
    Right [candidate] -> pure candidate
    Left reason -> fail ("production structural candidate codec refused: " ++ reason)
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
      external = identity "main" "Unavailable" "missing"
      stranded = identity "main" "G" "stranded"
      strandedDependent = identity "main" "H" "strandedDependent"
      siblingGood = identity "main" "A" "siblingGood"
      siblingConsumer = identity "main" "I" "siblingConsumer"
      owners = Map.fromList
        [(unavailable, ("A", 0)), (middle, ("B", 3)),
         (terminal, ("C", 7)), (unrelated, ("D", 2)),
         (cycleA, ("E", 1)), (cycleB, ("F", 4)),
         (stranded, ("G", 5)), (strandedDependent, ("H", 6)),
         (siblingGood, ("A", 1)), (siblingConsumer, ("I", 8))]
      dependencies = Map.fromList
        [ (("B", 3), Set.singleton unavailable)
        , (("C", 7), Set.singleton middle)
        , (("E", 1), Set.singleton cycleB)
        , (("F", 4), Set.singleton cycleA)
        , (("G", 5), Set.singleton external)
        , (("H", 6), Set.singleton stranded)
        , (("A", 1), Set.empty)
        , (("I", 8), Set.singleton siblingGood)
        , (("D", 2), Set.empty) ]
      blocked = closeUnavailableOriginalGroups dependencies owners
        (Set.fromList [unavailable, cycleA, external])
      rejectedModules = closeUnavailableOriginalModules dependencies owners blocked
  unless (blocked == Set.fromList
      [("A", 0), ("B", 3), ("C", 7), ("E", 1), ("F", 4), ("G", 5), ("H", 6)]) $
    fail "original product closure did not handle cross-module chains, cycles and unrelated groups"
  unless (rejectedModules == Set.fromList ["A", "B", "C", "E", "F", "G", "H", "I"]) $
    fail "module-level rejection did not cover sibling binders and their dependants"
  writeFile (work </> "ProjectionUnavailableProvider.hs") $ unlines
    ["{-# OPTIONS_GHC -O0 #-}", "module ProjectionUnavailableProvider (missing) where", "{-# NOINLINE missing #-}", "missing :: Int", "missing = 7"]
  writeFile (work </> "ProjectionOwner.hs") $ unlines
    [ "{-# OPTIONS_GHC -O0 #-}", "module ProjectionOwner (bad, good) where"
    , "import ProjectionUnavailableProvider"
    , "{-# NOINLINE bad #-}", "bad :: Int", "bad = missing"
    , "{-# NOINLINE good #-}", "good :: Int", "good = 42" ]
  writeFile (work </> "ProjectionIndependent.hs") $ unlines
    ["{-# OPTIONS_GHC -O0 #-}", "module ProjectionIndependent (safe) where", "safe :: Int", "safe = 1"]
  writeFile (work </> "ProjectionConsumer.hs") $ unlines
    [ "{-# OPTIONS_GHC -O0 -Wno-unused-imports #-}"
    , "module ProjectionConsumer (usesGood) where"
    , "import ProjectionOwner", "import ProjectionIndependent"
    , "usesGood = good + 1" ]
  paired <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "ProjectionConsumer.hs") [work] Nothing
  let findPrepared name = case [prepared | prepared <- pprModules paired
        , moduleNameString (moduleName (pmModule prepared)) == name] of
          [prepared] -> pure prepared
          _ -> fail ("cross-module projection fixture lacks " ++ name)
      findBinder prepared occurrence = case [identity | (binding, _) <- pmBindings prepared
          , binder <- topBinders binding, let identity = preparedRootIdentity binder
          , symbolOccurrence identity == T.pack occurrence] of
        [identity] -> pure identity
        _ -> fail ("cross-module projection fixture lacks binder " ++ occurrence)
  consumer <- findPrepared "ProjectionConsumer"
  ownerModule <- findPrepared "ProjectionOwner"
  provider <- findPrepared "ProjectionUnavailableProvider"
  independent <- findPrepared "ProjectionIndependent"
  known <- findBinder ownerModule "good"
  missing <- findBinder provider "missing"
  safe <- findBinder independent "safe"
  let pairedEnv = prHscEnv (pprPipelineResult paired)
      pairedInterfaces = pprProductInterfaces paired
      pairedContext = ProjectionContext "test" "matched"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        known [] Nothing Nothing Nothing Nothing
      consumerOutcome externalBinders = lookup (pmModule consumer)
        (preparedModuleProductOutcomes (projectOriginalHomeModuleProducts pairedEnv
          pairedInterfaces pairedContext externalBinders [consumer]))
  case consumerOutcome mempty of
    Just (Left (UnavailableOriginalHomeDependencies [missing])) | missing == known -> pure ()
    other -> fail ("unowned home dependency did not fail closed: " ++ show other)
  forM_ [("exact", Set.singleton known), ("candidate", Set.singleton known)] $
      \(ownerKind, ownerBinders) ->
        case consumerOutcome ownerBinders of
          Just (Right groups) | not (null groups) -> pure ()
          other -> fail (ownerKind ++ " owner did not preserve its dependent product: " ++ show other)
  let incompleteModules = filter ((/= pmModule provider) . pmModule) (pprModules paired)
      incompleteProducts = preparedModuleProductOutcomes
        (projectOriginalHomeModuleProducts pairedEnv pairedInterfaces pairedContext mempty incompleteModules)
      moduleOutcome prepared = lookup (pmModule prepared) incompleteProducts
      rejectedByMissing prepared = case moduleOutcome prepared of
        Just (Left (UnavailableOriginalHomeDependencies identities)) -> missing `elem` identities
        _ -> False
      safeOutcome = moduleOutcome independent
  unless (rejectedByMissing ownerModule && rejectedByMissing consumer
      && case safeOutcome of Just (Right groups) -> not (null groups); _ -> False) $
    fail "unavailable module sibling did not reject its consumers while preserving an independent owner"
  let safeContext = pairedContext { projectionEntry = safe }
  safeProgram <- either (fail . show) pure (projectPrepared safeContext [independent])
  _ <- certifyProjectedProducts work "independent-after-product-rejection" paired incompleteProducts
    [("safe", safeProgram)] pairedEnv >>= either fail pure
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

candidateSitedSiblings :: IO ()
candidateSitedSiblings = withScratch candidateSitedSiblingsAt

candidateSitedSiblingsAt :: FilePath -> IO ()
candidateSitedSiblingsAt work = do
  let unfoldDir = work </> "Tidepool" </> "Actors"
      replyDir = work </> "Tidepool" </> "Agent" </> "Reply"
      owner = unfoldDir </> "Unfold.hs"
      target = work </> "HydratedChildTarget.hs"
      candidates = ["Tidepool.Internal.RequestSite", "Tidepool.Agent.Reply.Internal"]
      owners = candidates ++ ["Tidepool.Actors.Unfold"]
  createDirectoryIfMissing True unfoldDir
  createDirectoryIfMissing True replyDir
  createDirectoryIfMissing True (work </> "Tidepool/Internal")
  copyFile "lib/Tidepool/Internal/RequestSite.hs" (work </> "Tidepool/Internal/RequestSite.hs")
  copyFile "test-source-boot/fixtures/HydratedChildOwner.hs" owner
  copyFile "test-source-boot/fixtures/HydratedReplyOwner.hs" (replyDir </> "Internal.hs")
  copyFile "test-source-boot/fixtures/HydratedChildTarget.hs" target
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing owner [work] Nothing
  originals <- newOriginalInterfaceArtifacts (prHscEnv (pprPipelineResult original))
    (pprFinalizedModules original) [] work
  originalFixture <- capturePreparedFixture work original
  capturedPath <- writeGenuineCandidateLexicalScope candidates owners work originalFixture
  scopePath <- writeGenuineEmptyMetadataScope work
  let scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath }
  -- Native candidates retain their original certified interfaces; Unfold is
  -- compiled from source here and retained only in the canonical lexical scope.
  reused <- runPipelineSessionSelected (PreparedProducts (Just (manifest work)))
    Set.empty GeneralCompile (Just scope) target [work] Nothing
  unless (Set.fromList (map candidateModule (pprAcceptedCandidates reused)) == Set.fromList candidates
      && any ((== "Tidepool.Actors.Unfold") . dependencyModuleName)
        (dependencyModules (preparedFreshDependencies reused))) $
    fail "typed sibling regression did not admit the requested originals"
  unless (Set.fromList (preparedNames reused) == Set.fromList ["Tidepool.Actors.Unfold","HydratedChildTarget"]
      && Set.fromList (map moduleNameString (Map.keys (pprFinalizedModules reused)))
        == Set.fromList ["Tidepool.Actors.Unfold","HydratedChildTarget"]) $
    fail "typed sibling regression did not compile its deliberately fresh Unfold and target"
  forM_ (pprAcceptedCandidates reused) $ \candidate -> do
    bytes <- BS.readFile (candidateInterface candidate)
    captured <- originalInterfaceBytes originals
      (mkModule (stringToUnit (candidateUnit candidate)) (mkModuleName (candidateModule candidate)))
      >>= maybe (fail "typed sibling candidate lacks its captured original interface") pure
    unless (candidateUnit candidate == "main"
        && bytes == captured && candidateInterfaceSha256 candidate == digest captured) $
      fail "typed sibling candidate changed its exact original interface custody"
  case filter ((== "HydratedChildTarget") . moduleNameString . moduleName . pmModule) (pprModules reused) of
    [prepared] | null (pmSiteRejections prepared), length (pmYieldSites prepared) == 1 -> pure ()
    _ -> fail "hydrated child surface lost its exact typed sibling or site identity"
  retainedScope <- readExactScope capturedPath >>= either fail pure
  unless (Set.fromList (map (snd . fst) (scopeLexical retainedScope)) == Set.fromList owners
      && null (scopeProducts retainedScope) && null (scopeExecutionOwners retainedScope)) $
    fail "typed sibling metadata closure lost source authority or acquired native execution"
  removeFile owner
  removeFile (work </> "Tidepool/Internal/RequestSite.hs")
  removeFile (replyDir </> "Internal.hs")
  captured <- runPipelineSessionSelected (PreparedProducts Nothing)
    Set.empty GeneralCompile (Just (scope {ssExactScope=Just capturedPath})) target [work] Nothing
  unless (null (pprAcceptedCandidates captured)
      && preparedNames captured == ["HydratedChildTarget"]
      && all ((`notElem` owners) . dependencyModuleName) (dependencyModules (preparedFreshDependencies captured))) $
    fail "typed sibling regression did not exclude captured originals from downsweep"
  case pprModules captured of
    [prepared] | null (pmSiteRejections prepared), length (pmYieldSites prepared) == 1 -> pure ()
    _ -> fail "source-free exact child surface lost its typed sibling or site identity"
  unless (map pmYieldSites (pprModules captured) == map pmYieldSites
      (filter ((== "HydratedChildTarget") . moduleNameString . moduleName . pmModule) (pprModules reused))) $
    fail "source-free sibling hydration changed the original typed suspension site"
  putStrLn "candidate typed siblings: native candidates plus fresh Unfold and source-free canonical owners retain childSited and typed sites"

-- The production inventory preserves every identity field and global
-- requirement; malformed controls mutate only the issued packet.
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
  emptyPath <- writeCandidateCodecFixture work EmptyCandidateInventory
  emptyCandidates <- readModuleCandidates emptyPath >>= either fail pure
  unless (case emptyCandidates of [candidate] -> null (candidateGroups candidate); _ -> False) $
    fail "production empty inventory codec added a structural group"
  issuedPath <- writeCandidateCodecFixture work CompactCandidateInventory
  issued <- readCodecTerm issuedPath
  (symbols,globalTable,rows,fields) <- case issued of
    TList values@[TString "TPMCAN",TString "10",symbols,globalTable,TList rows,_,_] ->
      pure (symbols,globalTable,rows,values)
    _ -> fail "production compact fixture has another candidate envelope"
  let envelope symbolTable globals' rows' = TList (replace 2 symbolTable
        (replace 3 globals' (replace 4 (TList rows') fields)))
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
  case (symbols,globalTable) of
    (TList symbolRows,TList globalRows) | length symbolRows == length identities
      && length globalRows == length globals -> pure ()
    _ -> fail "production inventory merged complete identities or global requirements"
  decoded <- readModuleCandidates issuedPath >>= either fail pure
  capturedOffer <- captureCandidateManifest issuedPath >>= either fail pure
  issuedBytes <- BS.readFile issuedPath
  BS.writeFile issuedPath (BSC.pack "changed after capture")
  capturedDecoded <- readCapturedModuleCandidatesWithGraphs [] capturedOffer >>= either fail pure
  unless (capturedDecoded == decoded) $
    fail "candidate admission did not consume its captured envelope"
  readModuleCandidates issuedPath >>= \case
    Left _ -> pure ()
    Right _ -> fail "candidate envelope drift was accepted by a new capture"
  BS.writeFile issuedPath issuedBytes
  unless (map candidateGroups decoded == [groups,reverse groups]) $
    fail "compact inventory changed exact values, order or ordinal"
  firstRow <- case rows of
    TList row:_ | length row == 16 -> pure row
    _ -> fail "production candidate fixture has another row layout"
  let badGroup binders globalRefs = [TList (replace 10
        (TList [TList [TInt 91,TList binders,TList globalRefs]]) firstRow)]
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
  expandedPath <- writeCandidateCodecFixture work BoundedExpandedCandidateInventory
  _ <- readModuleCandidates expandedPath >>= either fail pure
  expanded <- readCodecTerm expandedPath
  expandedAggregate <- case expanded of
    TList values@[_,_,_,_,TList [TList row],_,_] -> pure (TList
      (replace 4 (TList [TList row,TList (replace 1 (TString "Other") row)]) values))
    _ -> fail "production expanded-bound fixture has another candidate envelope"
  refuse "expanded-aggregate" "expanded candidate inventory exceeds" expandedAggregate
  -- Historical decoder refusals retain only their unsupported framing.
  refuse "unsupported6" "unsupported" (TList [TString "TPMCAN",TString "6",TList rows])
  refuse "unsupported7" "unsupported" (TList [TString "TPMCAN",TString "7",TList rows,fields !! 5])
  putStrLn "candidate compact inventory: production codec values/order/ordinals, complete interning, unavailable/out-of-range indices, dangling globals, duplicate owners, table/expanded bounds and unsupported6/7 passed"
  where
    replace index value fields = [if ordinal == index then value else field
      | (ordinal,field) <- zip [0::Int ..] fields]

candidateGhcLoad :: IO ()
candidateGhcLoad = withTiming $ withScratch $ \work -> do
  forM_ ["MetadataQuoteSupport.hs", "MetadataQuoter.hs", "MetadataQuotedTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  let helper = mkModuleName "MetadataQuoteSupport"
      restore = copyFile "test-source-boot/fixtures/MetadataQuoteSupport.hs" (work </> "MetadataQuoteSupport.hs")
      changed = copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
  scopePath <- writeGenuineEmptyMetadataScope work
  let scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath }
  original <- runPipelineSelected (PreparedProducts Nothing) (work </> "MetadataQuoter.hs") [work]
  originalFixture <- capturePreparedFixture work original
  writeGenuineCandidateManifestFor ["MetadataQuoteSupport"] work originalFixture
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
  inputTemplate <- readFile "test-source-boot/fixtures/HostActivationInput.hs"
  originalSource <- either fail pure (replaceTemplateMarker "{{CHECKED_TYPE}}" "Int" inputTemplate)
  writeFile sourcePath originalSource
  original <- runPipelineSessionSelected CheckedEnvironment Set.empty GeneralCompile Nothing sourcePath [work] Nothing
  inputType <- maybe (fail "host input fixture has no inferred type") pure (crResultType original)
  capturedSignature <- captureCheckedSignature (crHscEnv original) "__tidepool_cell_pin_0_sessionInput" inputType
  let signatureBytes = toStrictByteString (encodeCheckedSignature capturedSignature)
  issuedFields <- writeGenuineEmptyScopeFields work
  authorization <- readPurposeCodecFixture work [work] (CodecHostActivationInput signatureBytes) >>= \case
    TList values -> pure values
    _ -> fail "production host-input purpose encoder returned another record"
  let sha = TString (T.replicate 64 "a")
      empty = TList []
      text = TString . T.pack
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
  authoredPurpose <- readPurposeCodecFixture work [work] CodecItemPurpose
  authored <- decodeManifest (TList (replace 8 authoredPurpose issuedFields)) >>= either fail pure
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
  let requestBytes = toStrictByteString (encodeRequestTypeSignatures (RequestTypeSignatures replySignature Nothing))
  requestTerm <- either (fail . show) (pure . snd)
    (deserialiseFromBytes decodeTerm (BSL.fromStrict requestBytes))
  let nativeAuthorization recipe inner = [text "request-types2", requestTerm, text recipe, inner]
  forM_ [NoRequestHelpers,ActorReplyHelpers] $ \recipe -> do
    wrapper <- readRequestTypesCodecFixture work requestBytes recipe Nothing
    requestScope <- decodeManifest (TList (replace 8 wrapper issuedFields)) >>= either fail pure
    unless (scopeRequestTypes requestScope == Just (recipe,RequestTypeSignatures replySignature Nothing)
        && scopePurpose requestScope == NoCheckedPurpose
        && isNothing (scopeIncludePaths requestScope))
      (fail "native request wrapper lost recipe or granted an inner purpose")
    graphFree <- decodeManifest (TList (replace 7 TNull (replace 8 wrapper issuedFields))) >>= either fail pure
    unless (scopeRequestTypes graphFree == scopeRequestTypes requestScope
        && isNothing (scopeCheckedItem graphFree)
        && null (scopeExecutionGraphs graphFree) && null (scopeExecutionOwners graphFree))
      (fail "graph-free request wrapper changed native recipe authority")
    legacy <- decodeManifest (TList [text "TPEXACTSCOPE",text "4",sha,sha,empty,empty,empty,wrapper])
    unless (case legacy of Left reason -> "unsupported exact scope" `isInfixOf` reason; Right _ -> False)
      (fail "native request wrapper admitted a legacy scope envelope")
    wrapped <- readRequestTypesCodecFixture work requestBytes recipe
      (Just (toStrictByteString (encodeTerm (TList authorization))))
    wrappedHost <- decodeManifest (TList (replace 8 wrapped issuedFields)) >>= either fail pure
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
  initialAuthorization <- readPurposeCodecFixture work [work]
    (CodecHostInputCheck (toStrictByteString (encodeCheckedSignature initialSignature))) >>= \case
      TList values -> pure values
      _ -> fail "production initial host-input purpose encoder returned another record"
  let initialSource = "module HostActivationInput where\n__result :: TidepoolActivationInput\n__result = undefined\n"
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

exactTransactionReuse :: IO ()
exactTransactionReuse = withTiming $ withScratch $ \work -> do
  let fixture name = "test-source-boot/fixtures" </> name
      target = work </> "MetadataTarget.hs"
      installOwner = copyFile (fixture "MetadataOwner.hs") (work </> "MetadataOwner.hs")
      frontends diagnostics = length
        [line | line <- lines diagnostics
        , line == "tidepool-canonical-frontend module=MetadataOwner"
          || line == "tidepool-checked module=MetadataOwner target=False"]
      isInt checked = maybe False (`eqType` intTy) (crResultType checked)
  installOwner
  copyFile (fixture "MetadataTarget.hs") target
  scopePath <- writeGenuineEmptyMetadataScope work
  let relocatedPath = work </> "same-empty-scope.cbor"
      scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath
        ,ssIncarnation=Just "exact-transaction-reuse"}
      relocated = scope {ssExactScope=Just relocatedPath}
  copyFile scopePath relocatedPath
  withResidentPipelineSelectedRequests [work] $ \runRequest -> do
    runRequest (pure ()) $ \compile -> do
      let check session = compile CheckedEnvironment Set.empty GeneralCompile
            (Just session) target [work] Nothing
      (cold,coldDiagnostics) <- captureDiagnostics (check scope)
      unless (isInt cold && frontends coldDiagnostics == 1) $
        fail "cold exact transaction did not compile its real source dependency"
      (warm,warmDiagnostics) <- captureDiagnostics (check relocated)
      unless (isInt warm && frontends warmDiagnostics == 0
          && counterValues "transaction_reused_source_products" warmDiagnostics == [1]) $
        fail "identical empty exact scopes repeated a dependency frontend"
      (native,nativeDiagnostics) <- captureDiagnostics $
        compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
          target [work] Nothing
      unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult native))
          && frontends nativeDiagnostics == 0
          && counterValues "transaction_reused_source_products" nativeDiagnostics == [1]
          && Map.member (mkModuleName "MetadataOwner") (pprFinalizedModules native)) $
        fail "check to native preparation replayed its dependency or lost original42"
      copyFile (fixture "MetadataOwnerWithoutInstance.hs") (work </> "MetadataOwner.hs")
      changed <- sourceFailureDiagnostics (check scope)
      case changed of
        Left diagnostics | any (\diagnostic -> sourceDiagnosticAt target
            "No instance for" diagnostic && "Available Int" `isInfixOf` dMessage diagnostic) diagnostics -> pure ()
        Left diagnostics -> fail ("changed dependency failed for another reason: " ++ show diagnostics)
        Right _ -> fail "warm exact transaction borrowed a removed instance"
      installOwner
      (recovered,recoveryDiagnostics) <- captureDiagnostics (check scope)
      unless (isInt recovered && frontends recoveryDiagnostics == 1) $
        fail "synchronous refusal retained partial compiler products"
    runRequest (pure ()) $ \compile -> do
      (next,nextDiagnostics) <- captureDiagnostics $
        compile CheckedEnvironment Set.empty GeneralCompile (Just relocated)
          target [work] Nothing
      unless (isInt next && frontends nextDiagnostics == 1
          && counterValues "transaction_reused_source_products" nextDiagnostics == [0]) $
        fail "source compiler products survived their transaction"
  putStrLn "exact transaction reuse: check/check and check/native skip dependency work; instance drift, refusal recovery and transaction close passed"

exactLegacyValueIsolation :: IO ()
exactLegacyValueIsolation = withTiming $ withScratch $ \work -> do
  valueOwner <- maybe (fail "invalid legacy isolation value owner") pure
    (parseValModule "Tidepool.Session.Val.G2")
  let fixture name = "test-source-boot/fixtures" </> name
      alternate = work </> "alternate-values"
      consumer = work </> "CheckedValueConsumer.hs"
      target = work </> "LegacyValueNativeTarget.hs"
      produce root valueFixture consumerFixture = do
        let source = root </> "Tidepool/Session/Val/G2.hs"
        createDirectoryIfMissing True (takeDirectory source)
        copyFile (fixture valueFixture) source
        copyFile (fixture consumerFixture) (root </> "CheckedValueConsumer.hs")
        produced <- runPipelineSelected (PreparedProducts Nothing)
          (root </> "CheckedValueConsumer.hs") [root]
        iface <- maybe (fail "legacy value producer omitted its real interface") pure
          (Map.lookup (mkModuleName "Tidepool.Session.Val.G2") (pprProductInterfaces produced))
        writeBinIface (targetProfile (hsc_dflags (prHscEnv (pprPipelineResult produced))))
          QuietBinIFace NormalCompression (sessionHiPath root valueOwner) iface
        renameFile source (replaceExtension source "retained-source")
        BS.readFile (sessionHiPath root valueOwner)
  intBytes <- produce work "CheckedValueG2.hs" "CheckedValueConsumer.hs"
  boolBytes <- produce alternate "CheckedValueG2Bool.hs" "CheckedValueBoolConsumer.hs"
  copyFile (fixture "LegacyValueNativeTarget.hs") target
  scopePath <- writeGenuineEmptyMetadataScope work
  let scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath
        ,ssValIfaces=[valueOwner],ssIncarnation=Just "legacy-exact-isolation"}
  base <- readExactScope scopePath >>= either fail pure
  let value = ExactIfaceArtifact "main" "Tidepool.Session.Val.G2"
        (sessionHiPath work valueOwner) (digest intBytes) []
      admitted = base {scopePurpose=ExactCellPurpose
        (CheckedCellAdmission (replicate 64 '0') (replicate 64 '0') (replicate 64 '0')
          [] ["Tidepool.Session.Val.G2"] [] [value] Nothing AuthoredCellCheck) [work]}
  withResidentPipelineSelected [work] $ \compile -> do
    let native session = compile (PreparedProducts Nothing) Set.empty GeneralCompile
          (Just session) target [work] Nothing
        seed = do
          (result,diagnostics) <- captureDiagnostics (native scope)
          unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult result))
              && Map.member (mkModuleName "CheckedValueConsumer") (pprFinalizedModules result)
              && counterValues "transaction_reused_source_products" diagnostics == [0]) $
            fail "legacy isolation did not compile and finalize its real native dependency"
        refuse session = sourceFailureDiagnostics (native session) >>= \case
          Left diagnostics | any (\diagnostic -> sourceDiagnosticAt consumer "Bool" diagnostic
              && "Int" `isInfixOf` dMessage diagnostic) diagnostics -> pure ()
          Left diagnostics -> fail ("changed legacy value failed for another reason: " ++ show diagnostics)
          Right _ -> fail "exact reuse borrowed old Int dependency Core after legacy Bool injection"
    seed
    BS.writeFile (sessionHiPath work valueOwner) boolBytes
    refuse scope
    BS.writeFile (sessionHiPath work valueOwner) intBytes
    seed
    refuse scope {ssRoot=alternate}
    seed
    _ <- compile (PreparedProducts Nothing) Set.empty (CellProgramCompile GeneralCompile admitted)
      (Just scope) target [work] Nothing
    (authenticated,authenticatedDiagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty (CellProgramCompile GeneralCompile admitted)
        (Just scope {ssRoot=alternate}) target [work] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult authenticated))
        && counterValues "transaction_reused_source_products" authenticatedDiagnostics == [1]) $
      fail "authenticated checked values lost reuse or consumed the unsealed replacement root"
  putStrLn "exact legacy value isolation: native dependency seeds, same-root interface drift, root substitution and recovery passed"

exactLoadedMetadata :: IO ()
exactLoadedMetadata = withTiming $ withScratch $ \work -> do
  let fixture name = "test-source-boot/fixtures" </> name
      install name = copyFile (fixture name) (work </> name)
      resultType = fmap renderType . crResultType
      loadedOwner = "tidepool-checked-loaded-source module=MetadataOwner"
      checkedOwner = "tidepool-checked module=MetadataOwner target=False"
      frontendCount name diagnostics = length (filter (==
          "tidepool-canonical-frontend module=" ++ name) (lines diagnostics))
        + length (filter (== "tidepool-checked module=" ++ name ++ " target=False") (lines diagnostics))
  forM_ ["MetadataOwner.hs", "MetadataTarget.hs", "MetadataExtensionOnlyTarget.hs", "MetadataLoadedFamily.hs"
    , "MetadataFamilyTarget.hs", "MetadataHiddenFamily.hs", "MetadataUntracked.hs"
    , "MetadataUntrackedTarget.hs", "MetadataQuoter.hs", "MetadataQuotedTarget.hs", "MetadataQuoteSupport.hs"] install
  scopePath <- writeGenuineEmptyMetadataScope work
  let scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath
        , ssIncarnation = Just "loaded-metadata-test" }
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
    changed <- sourceFailureDiagnostics (checked "MetadataTarget.hs")
    case changed of
      Left diagnostics | any (\diagnostic -> sourceDiagnosticAt (work </> "MetadataTarget.hs")
          "No instance for" diagnostic && "Available Int" `isInfixOf` dMessage diagnostic) diagnostics -> pure ()
      Left diagnostics -> fail ("instance removal had another source failure: " ++ show diagnostics)
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
    quoterFixture <- capturePreparedFixture work quoterProducer
    writeGenuineCandidateManifestFor ["MetadataQuoteSupport"] work quoterFixture
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
    hiddenFixture <- capturePreparedFixture work hidden
    hiddenScopePath <- writeGenuineMetadataScope work ["MetadataHiddenFamily"] hiddenFixture
    let hiddenScope = scope {ssExactScope=Just hiddenScopePath}
    ordinaryProducts <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "MetadataTarget.hs") [work] Nothing
    ordinaryFixture <- capturePreparedFixture work ordinaryProducts
    writeGenuineCandidateManifestFor ["MetadataOwner"] work ordinaryFixture
    disjoint <- compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile (Just hiddenScope)
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
        compile (CheckedEnvironmentProducts (manifest work)) Set.empty GeneralCompile (Just hiddenScope)
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
    copyFile (fixture "MetadataLoadedFamilyCompatible.hs") (work </> "MetadataLoadedFamily.hs")
    compatibleFamily <- compile CheckedEnvironment Set.empty GeneralCompile (Just hiddenScope)
      (work </> "MetadataFamilyTarget.hs") [work] Nothing
    unless (resultType compatibleFamily == Just "Int") $
      fail "compatible loaded and hidden exact family equations did not establish warm state"
    install "MetadataLoadedFamily.hs"
    family <- try (compile CheckedEnvironment Set.empty GeneralCompile (Just hiddenScope)
      (work </> "MetadataFamilyTarget.hs") [work] Nothing) :: IO (Either SomeException CheckedEnvironmentResult)
    case family of
      Left failure | "retained family consistency" `isInfixOf` show failure -> pure ()
      _ -> fail "loaded metadata lost the hidden original family conflict"
    copyFile (fixture "MetadataLoadedFamilyCompatible.hs") (work </> "MetadataLoadedFamily.hs")
    recoveredFamily <- compile CheckedEnvironment Set.empty GeneralCompile (Just hiddenScope)
      (work </> "MetadataFamilyTarget.hs") [work] Nothing
    unless (resultType recoveredFamily == Just "Int") $
      fail "family conflict retained stale source equations in the next exact preflight"
    (_, untrackedDiagnostics) <- captureDiagnostics (checked "MetadataUntrackedTarget.hs")
    unless (frontendCount "MetadataUntracked" untrackedDiagnostics == 1
        && "tidepool-checked-loaded-source module=MetadataUntracked" `elem` lines untrackedDiagnostics) $
      fail "untracked compile-time input repeated its native frontend"
    receipts <- listDirectory (work </> ".exact-compilations")
    receiptSafety <- fmap catMaybes $ forM receipts $ \entry -> do
      exactCompilationCacheSafety (work </> "MetadataUntrackedTarget.hs")
        (work </> ".exact-compilations" </> entry </> "receipt.cbor")
    case receiptSafety of
      [cacheSafe] -> do
        putStrLn ("untracked target receipt cache_safe=" ++ show cacheSafe)
        unless (not cacheSafe) $
          fail "untracked dependency receipt marked dependency evidence cache_safe=true"
      [] -> fail "no exact compilation receipt matched the untracked target source"
      _ -> fail ("multiple exact compilation receipts matched the untracked target: "
        ++ show (length receiptSafety))
  putStrLn "exact loaded metadata: parity, extension-only execution elision, source drift, quoter bytecode, hidden family and untracked input passed"

-- Native candidates and exact owners bypass fresh preparation. Their defining
-- interfaces must still supply typed site siblings without widening imports.
hydratedSiteSiblings :: IO ()
hydratedSiteSiblings = withScratch $ \work -> do
  let unfoldName = "Tidepool.Actors.Unfold"
      replyName = "Tidepool.Agent.Reply.Internal"
      names = ["Tidepool.Internal.RequestSite",replyName,unfoldName]
      target = work </> "HydratedSiteExpr.hs"
      unfoldPath = work </> "Tidepool/Actors/Unfold.hs"
      replyPath = work </> "Tidepool/Agent/Reply/Internal.hs"
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
        unless (any (\(TopBinding identity _) -> identity == root)
              (concatMap tops (programBindings program))
            && any ((== sibling) . globalIdentity) (programGlobals program)) $
          fail "hydrated sibling lost its capture root or original defining global"
        unless (all (\global -> let identity = globalIdentity global in
            symbolModule identity /= "Tidepool.Internal.RequestSite"
              || symbolOccurrence identity /= "RequestSite") (programGlobals program)) $
          fail "post-tidy site issuance retained a newtype constructor worker"
        case pmYieldSites target' of
          [site] | ysOrigin site == "HydratedSiteExpr.__result"
            , stType (ysAnswer site) == "Bool"
            , map stType (ysInputs site) == ["Char"] -> pure site
          actual -> fail ("hydrated sibling changed the lexical site/root/input arity: " ++ show actual)
  createDirectoryIfMissing True (work </> "Tidepool/Actors")
  createDirectoryIfMissing True (work </> "Tidepool/Agent/Reply")
  createDirectoryIfMissing True (work </> "Tidepool/Internal")
  copyFile "lib/Tidepool/Internal/RequestSite.hs" (work </> "Tidepool/Internal/RequestSite.hs")
  copyFile "test-source-boot/fixtures/HydratedSiteUnfold.hs" unfoldPath
  copyFile "test-source-boot/fixtures/HydratedSiteReply.hs" replyPath
  copyFile "test-source-boot/fixtures/HydratedSiteExpr.hs" target
  cold <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing target [work] Nothing
  originalSite <- evidence cold
  coldFixture <- capturePreparedFixture work cold
  writeGenuineCandidateManifestFor names work coldFixture
  warm <- compile (PreparedProducts (Just (manifest work))) Nothing
  unless (sortOn id (map candidateModule (pprAcceptedCandidates warm)) == sortOn id names
      && all (`notElem` preparedNames warm) names) $
    fail "hydrated sibling regression did not take native-candidate reuse"
  warmSite <- evidence warm
  unless (warmSite == originalSite) (fail "native-candidate hydration changed exact child-site identity")
  let env = prHscEnv (pprPipelineResult cold)
  scopePath <- writeGenuineCandidateLexicalScope names names work coldFixture
  let scope = emptySessionScope {ssRoot=work,ssExactScope=Just scopePath}
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
  writeFile unfoldPath (T.unpack (T.replace "RequestSite '[input] result" "Bool" (T.pack source)))
  incompatible <- compile (PreparedProducts Nothing) Nothing >>= targetModule
  unless (any (isInfixOf "RequestSite input or reply index" . srMessage) (pmSiteRejections incompatible)) $
    fail "incompatible typed sibling did not remain a source rejection"
  -- A captured owner may retain the exact nominal name and indices while
  -- changing the private value field. Reject it before synthesizing Core.
  writeFile unfoldPath source
  carrierSource <- readFile "lib/Tidepool/Internal/RequestSite.hs"
  writeFile (work </> "Tidepool/Internal/RequestSite.hs")
    (T.unpack (T.replace "RequestSite Int" "RequestSite Bool"
      (T.replace "requestSiteIdentity (RequestSite identity) = identity"
        "requestSiteIdentity _ = 0" (T.pack carrierSource))))
  malformed <- compile (PreparedProducts Nothing) Nothing >>= targetModule
  unless (null (pmYieldSites malformed)
      && any (isInfixOf "RequestSite constructor ABI must accept exactly one Int field" . srMessage)
        (pmSiteRejections malformed)) $
    fail "a malformed captured carrier field was admitted for Int injection"
  putStrLn "hydrated site siblings: 11 checks passed (native/exact, private native/exact, wrong interface owner, foreign surface unit, two missing owners, missing sibling, incompatible sibling, malformed carrier ABI)"

exactBashMetadata :: FilePath -> IO ()
exactBashMetadata effects = withTiming $ withScratch $ \work -> do
  let target = work </> "MetadataBashTarget.hs"
  copyFile "test-source-boot/fixtures/MetadataBashTarget.hs" target
  scopePath <- writeGenuineEmptyMetadataScope work
  let scope = emptySessionScope { ssRoot = work, ssExactScope = Just scopePath }
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
        && dependencyCacheSafe (preparedFreshDependencies native)) $
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
    warmerFixture <- capturePreparedFixture work warmer
    writeGenuineCandidateManifestFor ["OptionalAnchor"] work warmerFixture
    warm <- root (PreparedProducts (Just (manifest work)))
    unless (map candidateModule (pprAcceptedCandidates warm) == ["OptionalAnchor"]) $
      fail "warm input fixture did not admit its authenticated anchor candidate"
    unless ("OptionalSupport" `elem` preparedNames warm) $
      fail "warm input fixture did not retain the executable support product"
    coldBody <- proof work "cold" cold
    warmBody <- proof work "warm" warm
    verifyCollectivePackageProof work cold
    case fst coldBody of
      CodecCheckedInputs owners closure -> do
        let direct = Set.fromList (concatMap snd owners)
            complete = Set.fromList closure
        unless (Set.size complete > Set.size direct) $
          fail "input fixture did not exercise transitive installed interface dependencies"
      _ -> fail "ordinary checked input fixture lacks a complete package proof"
    unless (normalized cold == normalized warm && coldBody == warmBody) $
      fail "optional native availability changed checked compilation inputs"
    putStrLn ("package-input-products cold=" ++ show (preparedNames cold)
      ++ " candidate=" ++ show (preparedNames warm)
      ++ " checked=" ++ show (length (dependencyModules (preparedFreshDependencies cold))))
    -- Both offers share the complete fresh warmer capture. This fixture
    -- checks input identity; mixed fresh/retained reissuance has separate owners.
    writeGenuineCandidateManifestFor ["OptionalAnchor", "OptionalSupport"] work warmerFixture
    reused <- root (PreparedProducts (Just (manifest work)))
    unless (map candidateModule (pprAcceptedCandidates reused) == ["OptionalAnchor", "OptionalSupport"]) $
      fail "input fixture did not exercise authenticated candidate hydration"
    reusedBody <- proof work "candidate" reused
    unless (normalized cold == normalized reused && coldBody == reusedBody) $
      fail "accepted candidate lost its checked direct package inputs"
    let incomplete = reused { pprPackageImports = Map.delete (mkModuleName "OptionalSupport")
          (pprPackageImports reused) }
    requireUserError "compiler input missing candidate package owner"
      "compiler input proof lacks complete checked package-import owners"
      (proof work "missing-owner" incomplete)
    let wiredRoot selection purpose = compile selection Set.empty purpose Nothing
          (work </> "OptionalWiredRoot.hs") [] Nothing
    wired <- wiredRoot (PreparedProducts Nothing) CertifyHomeProductsCompile
    wiredBody <- proof work "wired-fresh" wired
    case fst wiredBody of
      CodecUnsupportedWired "main" "OptionalWiredSupport" unit name
        | unit == unitString (moduleUnit gHC_PRIM)
        , name == moduleNameString (moduleName gHC_PRIM) -> pure ()
      _ -> do
        observed <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
          (work </> "OptionalWiredSupport.hs") [] Nothing
        let imported = Map.keys (imp_mods (tcg_imports (prTargetTcGblEnv (pprPipelineResult observed))))
        fail ("direct compiler-provided input lacked its typed unsupported category: body="
          ++ take 512 (show wiredBody) ++ " checked-imports="
          ++ show [(dependencyModuleName node, map dependencyImportName (dependencyModuleImports node))
            | node <- dependencyModules (preparedFreshDependencies wired)]
          ++ " resolved-imports=" ++ show [(unitString (moduleUnit owner),
              moduleNameString (moduleName owner), owner == gHC_PRIM) | owner <- imported])
    wiredFixture <- capturePreparedFixture work wired
    writeGenuineCandidateManifestFor ["OptionalWiredSupport"] work wiredFixture
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
      (CodecCheckedInputs _ _,_) -> pure ()
      _ -> fail "real primitive-extension interface did not retain a complete input proof"
  putStrLn "package inputs: cold/warm native divergence, identical checked closure, candidate roots, omission refusal and wired fresh/candidate refusal passed"
  where
    normalized result = renderDependencyEvidence ((preparedFreshDependencies result)
      { dependencyModules = sortOn (\node -> (dependencyModuleUnit node, dependencyModuleName node))
          [node { dependencyModuleProduct = ProductInterfaceOnly }
          | node <- dependencyModules (preparedFreshDependencies result)] })
    proof work label result = do
      let directory = work </> ("input-proof-" ++ label)
          evidence = preparedFreshDependencies result
      createDirectory directory
      writeFile (directory </> "dependencies.json") (renderDependencyEvidence evidence)
      writeCompileInputProof directory (prHscEnv (pprPipelineResult result)) evidence
        (pprPackageImports result)
      let inputPath = directory </> "compiler-inputs.cbor"
      readCompilerInputCodecFacts directory inputPath (directory </> "dependencies.json")

sessionNativeBodyDemand :: IO ()
sessionNativeBodyDemand = withTiming $ withScratch $ \work -> do
  forM_ ["OptionalRoot", "OptionalSupport", "OptionalAnchor", "OptionalWarmer"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name ++ ".hs") (work </> name ++ ".hs")
  writeFile (work </> "OptionalRoot.hs") $ unlines
    [ "module OptionalRoot where"
    , "import OptionalSupport ()"
    , "import OptionalAnchor ()"
    , "import Tidepool.Session.Val.G2 (x)"
    , "result :: Int"
    , "result = x - 41"
    ]
  writeFile (work </> "OptionalWarmer.hs") $ unlines
    [ "module OptionalWarmer where"
    , "import OptionalSupport"
    , "import OptionalAnchor"
    , "import Tidepool.Session.Val.G2 (x)"
    , "result :: Int"
    , "result = if optional then anchor + x else 0"
    ]
  copyFile "test-source-boot/fixtures/CheckedValueConsumer.hs" (work </> "CheckedValueConsumer.hs")
  let valueSource = work </> "Tidepool/Session/Val/G2.hs"
  createDirectoryIfMissing True (takeDirectory valueSource)
  copyFile "test-source-boot/fixtures/CheckedValueG2.hs" valueSource
  valueProducer <- runPipelineSelected (PreparedProducts Nothing)
    (work </> "CheckedValueConsumer.hs") [work]
  valueOwner <- maybe (fail "session tier fixture omitted its value owner") pure
    (parseValModule "Tidepool.Session.Val.G2")
  valueIface <- maybe (fail "session tier fixture omitted its value interface") pure
    (Map.lookup (mkModuleName "Tidepool.Session.Val.G2") (pprProductInterfaces valueProducer))
  writeBinIface (targetProfile (hsc_dflags (prHscEnv (pprPipelineResult valueProducer))))
    QuietBinIFace NormalCompression (sessionHiPath work valueOwner) valueIface
  renameFile valueSource (valueSource ++ ".retained-source")
  let scope = emptySessionScope {ssRoot=work, ssValIfaces=[valueOwner]}
      compileTarget compile name = compile (PreparedProducts Nothing) Set.empty GeneralCompile
        (Just scope) (work </> name) [work] Nothing
      injectedValueOwner result = isJust (lookupHpt
        (hsc_HPT (prHscEnv (pprPipelineResult result))) (mkModuleName "Tidepool.Session.Val.G2"))
  withResidentPipelineSelected [work] $ \compile -> do
    first <- compileTarget compile "OptionalRoot.hs"
    unless (preparedNames first == ["OptionalRoot"]
        && injectedValueOwner first
        && fmap renderType (prResultType (pprPipelineResult first)) == Just "Int"
        && all (`Map.member` pprFinalizedModules first)
          (map mkModuleName ["OptionalSupport", "OptionalAnchor"])) $
      fail ("session request prepared unrelated native bodies or skipped source finalization: "
        ++ show (preparedNames first))
    later <- compileTarget compile "OptionalWarmer.hs"
    unless (all (`elem` preparedNames later) ["OptionalWarmer", "OptionalSupport", "OptionalAnchor"]
        && injectedValueOwner later
        && fmap renderType (prResultType (pprPipelineResult later)) == Just "Int"
        && Map.member (mkModuleName "OptionalSupport") (pprFinalizedModules later)) $
      fail ("later session request did not activate its newly demanded source bodies: "
        ++ show (preparedNames later))
    writeFile (work </> "OptionalSupport.hs") "module OptionalSupport where\noptional ::\n"
    refused <- try (compileTarget compile "OptionalRoot.hs")
      :: IO (Either SomeException PreparedPipelineResult)
    unless (case refused of Left _ -> True; Right _ -> False) $
      fail "session demand skipped invalid source in an unused imported module"
  putStrLn "session native bodies: unused imports stay validation-only, then prepare when a later target demands them"

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
  let evidence = preparedFreshDependencies cold
      producer = prHscEnv (pprPipelineResult cold)
      consumers = [node | node@(ModuleNode _ summary) <- mgModSummaries' (hsc_mod_graph producer)
        , ms_mod_name summary == mkModuleName "InstanceConsumer"]
  verifyRetainedPackageWitness producer evidence
  lexical <- forM ["InstanceOwner", "InstanceRelay"] $ \name -> do
    requirements <- either fail pure (preparedHomeRequirements cold "main" name)
    unless (preparedExactCompilation cold == Nothing
        && selectedFreshHomeRequirements evidence "main" name == Right requirements) $
      fail "source-only prepared requirements differ from their fresh dependency view"
    pure (ExactIfaceArtifact "main" name (work </> name ++ ".hi") "" requirements, requirements)
  unless (map snd lexical == [[], [("main", "InstanceOwner")]]) $
    fail "selected home receipt omitted the transitive instance owner or admitted a package import"
  let altered = evidence { dependencyModules =
        [node { dependencyModuleSource = "missing-owner.hs" }
        | node <- dependencyModules evidence] }
  case selectedFreshHomeRequirements altered "main" "InstanceRelay" of
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
        , programTypes = TypeGraph IntMap.empty IntMap.empty, programSites = [], programConstructorReplies = [], programJsonLayout = Nothing }
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
    repeatedFacts <- certificateFacts first
    case codecCertificateTargets repeatedFacts of
      [("target",references)]
        | length references == 1001
        , length (codecCertificateOwners repeatedFacts) == 2
        , length (codecCertificateGlobalSeals repeatedFacts) == 2
        , null (codecCertificateModules repeatedFacts) ->
            unless (sameRepeatedReferences references (codecCertificateOwners repeatedFacts)) $
              fail ("repeated package witnesses lost canonical deduplication or reference indices: "
                ++ show (take 3 references,drop 999 references,codecCertificateOwners repeatedFacts))
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
      CodecRetainedPackageOwner "ghc-internal" "GHC.Internal.Base" packageHash _ 0 -> length packageHash == 64
      _ -> False) owners
      && any (\case CodecRetainedOwner _ 7 -> True; _ -> False) owners) $
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
    forwardFacts <- certificateFacts forward
    backwardFacts <- certificateFacts backward
    restored <- case codecCertificateTargets backwardFacts of
      [(name,references)] | length references == 37 -> pure backwardFacts
        {codecCertificateTargets=[(name,reverse references)]}
      _ -> fail "package catalog fixture lacks its target reference inventory"
    unless (forwardFacts == restored) $
      fail "package lookup order changed canonical global seals, witnesses or reference order"
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
  local <- certificateFacts localBytes
  unless (case codecCertificatePackages local of
      [("ghc-internal","GHC.Internal.Stack.Types",_,sha)] -> length sha == 64
      _ -> False) $
    fail "local package constructor without incoming globals lacks exact interface evidence"
  unless (null (codecCertificateOwners local)) $
    fail "local package constructor added an incoming global witness"
  internal <- encodeProgram (localProgram [synthetic]) >>= either fail certificateFacts
  unless (null (codecCertificatePackages internal) && null (codecCertificateOwners internal)) $
    fail "noncanonical internal package helper supplied external interface authority"
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
    sameRepeatedReferences references owners = case references of
      firstIndex:rest | length owners == 2 -> all (== firstIndex) (take 999 rest)
        && case drop 999 rest of
          [secondIndex] -> secondIndex /= firstIndex
            && occurrenceAt firstIndex owners == Just "map"
            && occurrenceAt secondIndex owners == Just "id"
          _ -> False
      _ -> False
    occurrenceAt index owners
      | index >= 0 && index < length owners = Just (symbolOccurrence (case owners !! index of
          CodecSourceOwner _ _ _ _ binder -> binder
          CodecRetainedOwner binder _ -> binder
          CodecPackageOwner _ _ _ binder -> binder
          CodecRetainedPackageOwner _ _ _ binder _ -> binder))
      | otherwise = Nothing

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
    verify (preparedFreshDependencies cold)
    coldFixture <- capturePreparedFixture work cold
    writeManifestFor ["InstanceOwner"] work coldFixture
    withResidentPipelineSelected roots $ \compile -> do
      let reuse = compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
            Nothing target [] Nothing
      warm <- reuse
      verify (preparedFreshDependencies warm)
      unless (resolutionBytes (preparedFreshDependencies warm) == resolutionBytes (preparedFreshDependencies cold)) $
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
      verify (preparedFreshDependencies restored)
      unless (resolutionBytes (preparedFreshDependencies restored) == resolutionBytes (preparedFreshDependencies cold)) $
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
  coldFixture <- capturePreparedFixture work cold
  writeGenuineCandidateManifestFor expected work coldFixture
  withResidentPipelineSelected [work] $ \compile ->
    forM_ [1 .. 3 :: Int] $ \sample -> do
      hPutStrLn stderr ("mixed-source-start independent=" ++ show count
        ++ " required=" ++ show required ++ " sample=" ++ show sample)
      start <- getMonotonicTimeNSec
      result <- compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
        Nothing (work </> "CacheEntry.hs") [] Nothing
      end <- getMonotonicTimeNSec
      requireMixed count result
      let evidence = preparedFreshDependencies result
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
  executable <- lookupEnv "TIDEPOOL_TEST_SOURCE_BOOT_CHILD" >>= maybe
    (fail "missing declared source-boot child executable") pure
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
    && dependencyCacheSafe (preparedFreshDependencies result)
    && dependencySelectionComplete (preparedFreshDependencies result)) $
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
        && dependencyCacheSafe (preparedFreshDependencies result)
        && dependencySelectionComplete (preparedFreshDependencies result)) $
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


reuseFresh :: FilePath -> IO PreparedPipelineResult
reuseFresh work = runPipelineSelected (PreparedProducts (Just (manifest work)))
  (work </> "CacheEntry.hs") [work]

requireReused :: String -> PreparedPipelineResult -> IO ()
requireReused label result = unless
  (Set.fromList (map candidateModule (pprAcceptedCandidates result))
      == Set.fromList ["CacheEven", "CacheOdd"]
    && all (`notElem` preparedNames result) ["CacheEven", "CacheOdd"]
    && "CacheEntry" `elem` preparedNames result
    && dependencyCacheSafe (preparedFreshDependencies result)
    && dependencySelectionComplete (preparedFreshDependencies result)) $
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
