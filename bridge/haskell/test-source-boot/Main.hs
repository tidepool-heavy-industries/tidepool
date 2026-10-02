module Main (main) where

import Codec.CBOR.Encoding (encodeBool, encodeListLen, encodeString)
import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Data.ByteString.Lazy qualified as BSL
import Control.Exception (SomeException, bracket, evaluate, finally, try)
import Control.Monad (foldM, forM, unless, void)
import GHC.Clock (getMonotonicTimeNSec)
import Data.Word (Word64)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Char8 qualified as BSC
import Data.List (isInfixOf, isPrefixOf, sortOn)
import Data.Maybe (isJust, isNothing)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC (runGhc, setSession, ms_mod_name, ms_hsc_src, parseModule, typecheckModule, Target(..))
import GHC.Core qualified as Core
import GHC.Builtin.Types (boolTy, intTy, charTy, stringTy)
import GHC.Types.Id (idName, setIdName)
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
import GHC.Unit.Module.Location (ml_hi_file)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Tc.Types (tcg_imports)
import GHC.Unit.Module.Deps (imp_mods)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries', mkModuleGraph)
import GHC.Types.SourceFile (HscSource(..))
import Control.Monad.IO.Class (liftIO)
import GHC.Driver.Session (targetProfile)
import GHC.Driver.Hooks (hscCompileCoreExprHook)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Unit.Module.ModIface (set_mi_module, mi_module, mi_exports)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module (Module, mkModule, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString, stringToUnit, GenWithIsBoot(..))
import Numeric (showHex)
import System.Directory
  ( copyFile, createDirectory, createDirectoryIfMissing, getTemporaryDirectory, removeDirectoryRecursive
  , removeFile, renameFile, listDirectory, doesFileExist, getPermissions, setPermissions, executable )
import System.Environment (getArgs, getExecutablePath, setEnv, lookupEnv, unsetEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>), takeDirectory)
import System.IO (hClose, hFlush, hPutStrLn, hSeek, SeekMode(AbsoluteSeek), openTempFile, stderr)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.Process (readProcessWithExitCode)
import System.Timeout (timeout)
import Tidepool.CertifiedProducts (encodeCertifiedProducts)
import Tidepool.ExecutionEncode (encodeModuleProducts)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), projectPreparedModuleGroups
  , projectPreparedModuleProducts, projectOriginalHomeModuleProducts, preparedModuleProductOutcomes, topBinders
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
import Tidepool.PreparedFacts (PreparedFacts(..))
import GHC.Types.Name (nameModule_maybe)
import GHC.Types.Var (varName)
import Tidepool.CompileInput (writeCompileInputProof)
import Tidepool.ExecutionSchema
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencyImport(..)
  , DependencyResolution(..), ProductAvailability(..), DependencySource(..), sourceEvidence
  , selectedHomeRequirements, renderDependencyEvidence )
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), freshExactState, noCheckedValueImports, installExactLexicalGraph
  , readCheckedValueImportAuthority, readExactIfaceArtifacts, hydrateExactScope
  , readVerifiedExactIfaceClosure, readVerifiedExactIfaceClosureWithCheckedValues
  , selectVerifiedExactInterfaces, selectVerifiedValueInterfaces, checkedValueImportAuthorityFromVerified )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.HomeProducts (hydrateCandidateHomeProducts)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), PipelineResult(..), CheckedEnvironmentResult(..)
  , renderType, generatedScaffoldRecipe
  , CompilePurpose(..), runPipelineSelected, runPipelineSessionSelected, withResidentPipelineSelected )
import Tidepool.ModuleCandidates (ModuleCandidate(..), CandidateGroup(..), CandidateGlobal(..)
  , readModuleCandidates, candidateExecutionSources, candidateOriginalIdentity)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..), encodePackageImports, emptyPackageImports, readPackageImports)
import Tidepool.PreparedStg (PreparedModule(..), PreparedCoverage(..))
import Tidepool.FatIface (readExactInterface)
import Tidepool.Session (SessionScope(..), emptySessionScope)
import Tidepool.SessionArtifacts (mkBoundBinders, parseValModule)
import Tidepool.Session (sessionHiPath)
import Tidepool.ExactScope (ExactScope(..), ExactProduct(..), CheckedCellAdmission(..), readExactScope, extendExactExecutionSources, extendExactExecutionSourcesWithinBudget, scopeExecutionNativeOwners)
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.Binders (BoundBinder(..))
import Tidepool.ExecutionSource
  ( ExecutionSourceIdentity(..), ExecutionSourceOwner(..), ExecutionSourceRef(..), ExecutionSourceGraph(..), ExecutionSourceNode(..)
  , executionSourceClosure, executionSourceOriginalNode, executionSourceOriginalClosure, executionIdentityKey
  , ExecutionSourceRecipe(..), issueExecutionSourceRecipe, executionSourceProspectiveReferences )

main :: IO ()
main = getArgs >>= \case
  ["--original-package-projection"] -> originalPackageProjection
  ["--original-package-cohort", coreRoot, output] -> originalPackageCohort coreRoot output
  ["--original-projection-products"] -> originalProjectionProducts
  ["--candidate-manifest-products", path] -> candidateManifestProducts path
  ["--candidate-compact-inventory"] -> candidateCompactInventory
  ["--candidate-ghc-load"] -> candidateGhcLoad
  ["--generated-scaffold-imports"] -> generatedScaffoldImports
  ["--generated-scaffold-retained",scope,seal] -> generatedScaffoldRetained scope seal
  ["--hydrated-site-siblings"] -> hydratedSiteSiblings
  ["--fresh-execution-recipe"] -> freshExecutionRecipeTest
  ["--candidate-execution-sources"] -> candidateExecutionSourcesTest
  ["--candidate-execution-wire", path] -> candidateExecutionWire path
  ["--checked-value-type-closure", effects] -> checkedValueTypeClosure effects
  ["--execution-source-wire", path] -> executionSourceWire path
  ["--exact-retained-quoter"] -> exactRetainedQuoter
  ["--exact-reexport-quoter"] -> exactReexportQuoter
  ["--exact-execution-hidden-instance"] -> exactExecutionHiddenInstance
  ["--exact-execution-values"] -> exactExecutionValues
  ["--exact-to-ordinary"] -> exactToOrdinary
  ["--checked-value-imports"] -> checkedValueImports
  ["--exact-loaded-metadata"] -> exactLoadedMetadata
  ["--exact-bash-metadata", effects] -> exactBashMetadata effects
  ["--package-inputs"] -> packageInputs
  ["--home-instance-edges"] -> selectedHomeInstanceEdges
  ["--fresh", work] -> reuseFresh work >>= requireReused "fresh worker"
  ["--mixed-fresh", work, count] -> reuseFresh work >>= requireMixed (read count)
  ["--mixed"] -> do
    setEnv "TIDEPOOL_TIMING" "1"
    forM_ [1, 10, 100] (mixedGraph False)
    mixedGraph True 10
  [] -> withScratch $ \work -> do
    selectedHomeInstanceEdges
    forM_ ["CacheEven.hs", "CacheEven.hs-boot", "CacheOdd.hs", "CacheEntry.hs"] $ \file ->
      copyFile ("test-source-boot/fixtures" </> file) (work </> file)
    cold <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "CacheEntry.hs") [work] (Just (work </> "build-products"))
    unless (all (`elem` preparedNames cold) ["CacheEven", "CacheOdd", "CacheEntry"]
        && null (pprAcceptedCandidates cold)) $
      fail "cold SOURCE graph omitted original defining products"
    unless (dependencyCacheSafe (pprDependencies cold)
        && dependencySelectionComplete (pprDependencies cold)) $
      fail "cold SOURCE graph lacks final source/package evidence"
    writeManifest work cold
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
      let originalProduct = work </> "CacheEven.candidate.hi.descriptor-only.tpmod"
          reuse = compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
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
    setEnv "TIDEPOOL_TIMING" "1"
    mixedGraph False 1
    mixedGraph True 10
  _ -> fail "unexpected SOURCE boot test arguments"

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

executionSourceWire :: FilePath -> IO ()
executionSourceWire path = do
  scope <- readExactScope path >>= either fail pure
  unless (length (scopeExecutionGraphs scope) == 1 && length (scopeExecutionOwners scope) == 1
      && scopeCheckedCell scope == Nothing && scopeCheckedItem scope == Nothing
      && scopeCheckedDisplay scope == Nothing && scopeIncludePaths scope == Nothing) $
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
  let refuse label action = do
        action
        result <- readExactScope path
        BS.writeFile graphPath originalBytes
        unless (case result of Left _ -> True; Right _ -> False) $
          fail ("scope6 accepted " ++ label ++ " before execution")
  refuse "missing graph" (removeFile graphPath)
  refuse "truncated graph" (BS.writeFile graphPath (BS.take (BS.length originalBytes - 1) originalBytes))
  refuse "tampered graph" (BS.writeFile graphPath (BS.cons 0 (BS.drop 1 originalBytes)))
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
  putStrLn "Rust execution wire: scope6 closure, independent budgets, wrong native/missing/truncated/tampered/swapped refusals passed (7 checks)"

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
        , scopeCheckedCell = Just (CheckedCellAdmission (replicate 64 '0') (replicate 64 '0')
            (replicate 64 '0') [] ["Tidepool.Session.Val.G7"] [] [value] Nothing) }
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
        let capture = admitted {scopeCheckedCell = fmap
              (\admission -> admission {checkedValueInterfaces=[input]}) (scopeCheckedCell admitted)}
        checked <- compile CheckedEnvironment Set.empty (CellProgramCompile purpose capture)
          (Just scope) consumerPath [work,"lib",effects] Nothing
        unless (fmap renderType (crResultType checked) == Just "Command") $
          fail "dependency-ordered command value injection changed its captured type"
  putStrLn "checked command value: complete type closure, delayed injection and missing/changed-owner refusal passed"

generatedScaffoldImports :: IO ()
generatedScaffoldImports = withTiming $ withScratch $ \work -> do
  let supportDirectory = work </> "Tidepool/Internal"
      supportPath = supportDirectory </> "Resume.hs"
      target = work </> "Expr.hs"
      hiddenPath = work </> "hidden-scaffold.cbor"
      hidden = emptySessionScope {ssRoot=work,ssExactScope=Just hiddenPath}
      includes = [work]
  createDirectoryIfMissing True supportDirectory
  copyFile "lib/Tidepool/Internal/Resume.hs" supportPath
  copyFile "test-source-boot/fixtures/GeneratedScaffoldExpr.hs" target
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing supportPath includes Nothing
  writeExecutionScope hiddenPath work original []
  protected <- readFile target
  recipe <- generatedScaffoldRecipe protected protected target "Expr" >>= either fail pure
  let purpose = GeneratedScaffoldCompile recipe (CheckedItemCompile [] Nothing [])
      requireRejected label action = do
        result <- try (void action) :: IO (Either SomeException ())
        case result of
          Left reason -> do
            let detail = show reason
            unless (label /= "same target under general purpose" ||
                "source graph imports unadmitted home implementation" `isInfixOf` detail) $
              fail "baseline scaffold fixture did not reproduce the actual graph refusal"
            putStrLn ("scaffold refused " ++ label ++ ": " ++ take 512 detail)
          Right _ -> fail ("scaffold authority accepted " ++ label)
  withResidentPipelineSelected includes $ \compile -> do
    requireRejected "same target under general purpose" $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just hidden) target [] Nothing
    admitted <- compile (PreparedProducts Nothing) Set.empty purpose (Just hidden) target [] Nothing
    unless (hasIntResultLiteral 42 (prBinds (pprPipelineResult admitted))) $
      fail "generated scaffold lost its actual settled result"
    let extra = unlines (take 5 (lines protected) ++ ["import Tidepool.Internal.Resume"] ++ drop 5 (lines protected))
    writeFile target extra
    duplicate <- generatedScaffoldRecipe protected extra target "Expr" >>= either fail pure
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
    helperRecipe <- generatedScaffoldRecipe protected helperTarget target "Expr" >>= either fail pure
    requireRejected "fresh helper importing hidden support" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile helperRecipe GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target ("{-# LINE 100 \"authored.hs\" #-}\n" ++ protected)
    lineRecipe <- generatedScaffoldRecipe protected ("{-# LINE 100 \"authored.hs\" #-}\n" ++ protected) target "Expr" >>= either fail pure
    requireRejected "logical LINE import location differs from protected occurrence" $
      compile (PreparedProducts Nothing) Set.empty (GeneratedScaffoldCompile lineRecipe GeneralCompile)
        (Just hidden) target [] Nothing
    writeFile target protected
    admittedScope <- readExactScope hiddenPath >>= either fail pure
    forM_ ["Bind","Display"] $ \name -> do
      let rendered = T.unpack (T.replace "module Expr where" (T.pack ("module " ++ name ++ " where")) (T.pack protected))
          generatedPath = work </> (name ++ ".hs")
      writeFile generatedPath rendered
      generated <- generatedScaffoldRecipe protected rendered generatedPath name >>= either fail pure
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
      Nothing supportPath [] Nothing
    let missingExportPath = work </> "missing-export.cbor"
    writeExecutionScope missingExportPath work missingExport []
    requireRejected "missing actual resumeLifted export" $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just missingExportPath}) target [] Nothing
    forM_ ["ExecutionClass.hs","ExecutionHiddenOrphan.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
    let withOrphan = unlines [if line == "import Prelude" then
          "import Prelude\nimport ExecutionHiddenOrphan ()" else line | line <- lines supportText]
    writeFile supportPath withOrphan
    hiddenNeighbor <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing supportPath [] Nothing
    let neighborPath = work </> "hidden-neighbor.cbor"
    writeExecutionScope neighborPath work hiddenNeighbor []
    requireRejected "hidden orphan neighbor through scaffold support" $
      compile (PreparedProducts Nothing) Set.empty purpose
        (Just hidden {ssExactScope=Just neighborPath}) target [] Nothing
    copyFile "test-source-boot/fixtures/MetadataHiddenFamily.hs" (work </> "MetadataHiddenFamily.hs")
    let withFamily = unlines [if line == "import Prelude" then
          "import Prelude\nimport MetadataHiddenFamily ()" else line | line <- lines supportText]
    writeFile supportPath withFamily
    hiddenFamily <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing supportPath [] Nothing
    let familyPath = work </> "hidden-family.cbor"
    writeExecutionScope familyPath work hiddenFamily []
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
  recipe <- generatedScaffoldRecipe source source target "Expr" >>= either fail pure
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
  withResidentPipelineSelected [work] $ \compile -> do
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
    let helperPath = work </> "MetadataQuoteSupport.hs"
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
    let cancelMarker = work </> "cancel-marker"
        quoterPath = work </> "MetadataQuoter.hs"
    cancellingSource <- readFile "test-source-boot/fixtures/ExecutionCancellingQuoter.hs"
    writeFile quoterPath (T.unpack (T.replace "\"EXECUTION_CANCEL_MARKER\"" (T.pack (show cancelMarker)) (T.pack cancellingSource)))
    cancelled <- try (timeout 1500000 (compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
      (work </> "MetadataQuotedTarget.hs") [work] Nothing)) :: IO (Either SomeException (Maybe CheckedEnvironmentResult))
    beganExecution <- doesFileExist cancelMarker
    unless (beganExecution && case cancelled of Right (Just _) -> False; _ -> True) $
      fail "execution cancellation did not reach the scoped splice linker"
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
      Left reason -> "source graph imports unadmitted home implementation" `isInfixOf` show reason
        && not ("tidepool-timing phase=ghc_load" `isInfixOf` diagnostics)
      _ -> False) $
      fail "execution recipe granted a fresh provider a hidden lexical import"
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
      resolution row = TList [text (dependencyResolutionQualifier row),text (dependencyResolutionModule row),TBool (dependencyResolutionBoot row)
        ,optional (dependencyResolutionSelected row),list text (dependencyResolutionCandidates row)]
      imported row = TList [text (dependencyImportQualifier row),text (dependencyImportName row),TBool (dependencyImportBoot row),optional (dependencyImportSelected row)]
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
    unless ("tidepool-count name=exact_execution_original_load_owners count=2" `elem` lines diagnostics) $
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
    unless (case hiddenQuote of Left _ -> "tidepool-count name=exact_execution_original_load_owners count=0" `elem` lines hiddenDiagnostics; _ -> False) $
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
    unless (case classHidden of Left _ -> "tidepool-count name=exact_execution_original_load_owners count=0" `elem` lines classDiagnostics; _ -> False) $
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
  withResidentPipelineSelected [work] $ \compile -> do
    _ <- compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionSealedTarget.hs") [work] Nothing
    ordinary <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile Nothing
      (work </> "ExecutionClassQuoter.hs") [work] Nothing
    unless ("ExecutionClassQuoter" `elem` preparedNames ordinary) $
      fail "ordinary certification after an exact request omitted its source owner"
    let ordinaryQuote = do
          result <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
            (work </> "ExecutionClassQuoteTarget.hs") [work] Nothing
          assertSingleDiagnosticCollector (prHscEnv (pprPipelineResult result))
          unless (hasIntResultLiteral 43 (prBinds (pprPipelineResult result))
              && not (isJust (hscCompileCoreExprHook (hsc_hooks (prHscEnv (pprPipelineResult result)))))
              && not (isJust (lookupHpt (hsc_HPT (prHscEnv (pprPipelineResult result))) (mkModuleName "ExecutionHiddenOrphan")))
              && not (isJust (lookupHpt (hsc_HPT (prHscEnv (pprPipelineResult result))) (mkModuleName "Tidepool.Session.Val.G2")))) $
            fail "ordinary request inherited an exact execution environment"
    ordinaryQuote
    ordinaryQuote
    _ <- compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
      (work </> "ExecutionSealedTarget.hs") [work] Nothing
    afterExactLegacy <- compile CheckedEnvironment Set.empty GeneralCompile
      (Just emptySessionScope {ssRoot=work,ssValIfaces=[valueModule]}) (work </> "CheckedValueConsumer.hs") [work] Nothing
    unless (fmap renderType (crResultType afterExactLegacy) == Just "Int") $
      fail "legacy value request inherited the preceding exact graph"
    ordinaryQuote
    refused <- try (compile CheckedEnvironment Set.empty GeneralCompile
      (Just scope {ssExactScope=Just hiddenPath}) (work </> "ExecutionSealedTarget.hs") [work] Nothing)
      :: IO (Either SomeException CheckedEnvironmentResult)
    unless (case refused of Left reason -> "unadmitted home implementation" `isInfixOf` show reason; _ -> False) $
      fail "hidden original unexpectedly became a fresh lexical import"
    ordinaryQuote
    cancelling <- readFile "test-source-boot/fixtures/ExecutionCancellingQuoter.hs"
    let marker = work </> "cancel-marker"
    writeFile (work </> "MetadataQuoter.hs") (T.unpack (T.replace "EXECUTION_CANCEL_MARKER" (T.pack marker) (T.pack cancelling)))
    cancelled <- timeout 1500000 (compile CheckedEnvironment Set.empty GeneralCompile
      (Just scope {ssExactScope=Just helperPath}) (work </> "MetadataQuotedTarget.hs") [work] Nothing)
    started <- doesFileExist marker
    unless (isNothing cancelled && started) (fail "exact cancellation did not reach the real quoter")
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
    ordinaryQuote
    provisional <- compile (PreparedProducts (Just (work </> "missing-candidates.cbor"))) Set.empty GeneralCompile Nothing
      (work </> "ExecutionClassQuoteTarget.hs") [work] Nothing
    unless (null (pprAcceptedCandidates provisional)) (fail "missing manifest unexpectedly admitted a candidate")
    ordinaryQuote
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
  let admitted = base {scopeCheckedCell=Just (CheckedCellAdmission (replicate 64 '0') (replicate 64 '0')
        (replicate 64 '0') [] ["Tidepool.Session.Val.G8"] [] [value] Nothing)}
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
           , (constructor, _) <- preparedConstructors (pmFacts prepared)
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
           , (constructor, _) <- preparedConstructors (pmFacts prepared)
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
  encodeCertifiedProducts env [] Nothing fresh targets evidence bytes
    (BSC.pack (renderDependencyEvidence evidence))

certificateOwners :: BS.ByteString -> IO [Term]
certificateOwners bytes = do
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
  case term of
    TList [TString "TPCERT", TInt 4, _, _, _, TList rows] -> forM rows $ \case
      TList [_,_,_,_,owner] -> pure owner
      _ -> fail "original certificate global lacks its exact owner"
    _ -> fail "original product lacks its canonical ownership certificate"

verifyOriginalOnlyPackageRefusal :: FilePath -> PreparedPipelineResult
  -> [(Module, Either ProjectionError [ProjectedGroup])] -> BS.ByteString -> IO ()
verifyOriginalOnlyPackageRefusal work original outcomes certified = do
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict certified))
  (unit,name,path) <- case term of
    TList [_,_,_,TList [],TList (TList [TString unit,TString name,TString path,_]:_),_] ->
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

originalProjectionProducts :: IO ()
originalProjectionProducts = withScratch $ \work -> do
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
    compactRow inventory (TList fields) | length fields == 14 = case drop 10 fields of
      TList groups:_ -> do
        (next,compact) <- mapFixtureInventory compactGroup inventory groups
        pure (next,TList (take 10 fields ++ [TList compact] ++ drop 11 fields))
      _ -> Left "fixture candidate lacks groups"
    compactRow _ _ = Left "fixture candidate must have fourteen fields"
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
        [TString "TPMCAN",TString "8",symbols,globalTable,TList rows,emptyParcel]
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
        ++ [TList [],TList groups,TString "/fixture/packages",sha,TString "/fixture/products.tpmod"])
      where sha = TString (T.replicate 64 "0")
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
  writeExactMetadataScope scopePath []
  original <- runPipelineSelected (PreparedProducts Nothing) (work </> "MetadataQuoter.hs") [work]
  writeManifestFor ["MetadataQuoteSupport"] work original
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
          && ("tidepool-count name=candidate_source_load_required count=" ++ show required) `elem` lines diagnostics
          && hasIntResultLiteral expected (prBinds result)) $
        fail ("native candidate reuse skipped GHC execution or retained an old quoted helper body: "
          ++ show (expected, accepted, fresh) ++ "\n" ++ diagnostics ++ "\n"
          ++ showSDocUnsafe (ppr (prBinds result)))
      case lookupHpt (hsc_HPT (prHscEnv result)) helper of
        Just hmi | let linkable = hm_linkable hmi
                 , isJust (homeMod_bytecode linkable) || isJust (homeMod_object linkable) -> pure ()
        _ -> fail "native candidate retention discarded its actual GHC executable"
  putStrLn "candidate GHC load: original native reuse, actual quoter execution and source A/B/A passed"

-- Pure issuer and scope-budget controls; the runtime suite separately drives
-- cold parser -> whole checked program -> per-item original certification.
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
  let reference = ExecutionSourceRef support (executionGraphSha256 graph)
  selected <- either (fail . show) pure (executionSourceProspectiveReferences [graph] [] [reference])
  unless (selected == [reference]) $ fail "fresh supported recipe did not issue exact original"
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
        [(ExactIfaceArtifact "main" "Support" "" sha [],"",sha)] [] [original]
        [] [] Nothing Nothing Nothing Nothing
      oversized = graph {executionGraphBytes=BS.replicate (4*1024*1024+1) 0}
  bounded <- either (fail . show) pure
    (extendExactExecutionSourcesWithinBudget [oversized] [reference] scope)
  unless (isNothing bounded && case extendExactExecutionSources [oversized] [reference] scope of
      Left _ -> True; _ -> False) $ fail "optional/advertised aggregate budget policies diverged"
  unless (case extendExactExecutionSourcesWithinBudget [oversized]
      [reference {executionRefGraph=replicate 64 'b'}] scope of Left _ -> True; _ -> False) $
    fail "aggregate budget withholding hid corrupt advertised graph"
  putStrLn "fresh execution recipe: issuer, original lineage and strict/optional budget controls passed"

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
  copyFile "test-source-boot/fixtures/MetadataQuoteSupportChanged.hs" (work </> "MetadataQuoteSupport.hs")
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
  case executionSourceOriginalClosure (alternateGraph:mixedGraphs)
      [quoterRef,quoterRef {executionRefGraph=executionGraphSha256 alternateGraph}] of
    Left _ -> pure ()
    Right _ -> fail "shared source dedup discarded another recipe's negative-resolution constraints"
  -- Each level shares both later levels. Revalidating settled recipes per
  -- incoming path expands this bounded source inventory exponentially.
  let dagNames = ["SharedRecipe" ++ show index | index <- [0::Int ..35]]
      dagIdentity name = (executionRefIdentity helperReference) {executionModule=name}
      dagPath name = work </> name ++ ".hs"
      dagModules = [DependencyModule "main" name False (dagPath name)
          [DependencyImport "none" child False (Just (dagPath child))
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
        TList fields -> TList [if index == 7 then filteredParcel else field | (index,field) <- zip [0::Int ..] fields]
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
  putStrLn "candidate execution sources: actual native/source admission, source drift refusal, accepted-only promotion, no lexical widening, thin reexport execution and identity/digest/duplicate/producer refusals passed"
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
  forM_ ["MetadataOwner.hs", "MetadataTarget.hs", "MetadataLoadedFamily.hs"
    , "MetadataFamilyTarget.hs", "MetadataHiddenFamily.hs", "MetadataUntracked.hs"
    , "MetadataUntrackedTarget.hs", "MetadataQuoter.hs", "MetadataQuotedTarget.hs", "MetadataQuoteSupport.hs"] install
  writeExactMetadataScope scopePath []
  withResidentPipelineSelected [work] $ \compile -> do
    let checked name = compile CheckedEnvironment Set.empty GeneralCompile (Just scope)
          (work </> name) [work] Nothing
    ordinary <- compile CheckedEnvironment Set.empty GeneralCompile Nothing
      (work </> "MetadataTarget.hs") [work] Nothing
    (exact, diagnostics) <- captureDiagnostics (checked "MetadataTarget.hs")
    unless (resultType exact == Just "Int" && resultType ordinary == resultType exact
        && length (filter (== loadedOwner) (lines diagnostics)) == 1
        && checkedOwner `notElem` lines diagnostics
        && "tidepool-checked module=MetadataTarget target=True" `elem` lines diagnostics) $
      fail "exact metadata repeated the loaded source frontend or changed its instance result"
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
        && "tidepool-checked-dependency-executable module=MetadataQuoter bytecode=True object=False" `elem` lines quoteDiagnostics
        && "tidepool-checked-loaded-source module=MetadataQuoter" `elem` lines quoteDiagnostics) $
      fail "exact metadata discarded the loaded quoter's executable linkable"
    quoterProducer <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "MetadataQuoter.hs") [work] Nothing
    -- The helper has no TH extension or quotation itself. GHC's graph still
    -- requires its bytecode when the quoter executes in the target.
    writeManifestFor ["MetadataQuoteSupport"] work quoterProducer
    (quotedCandidate, candidateQuoteDiagnostics) <- captureDiagnostics $
      compile (CheckedEnvironmentProducts (manifest work)) Set.empty GeneralCompile (Just scope)
        (work </> "MetadataQuotedTarget.hs") [work] Nothing
    unless (resultType quotedCandidate == Just "Int"
        && "tidepool-checked-loaded-source module=MetadataQuoter" `elem` lines candidateQuoteDiagnostics
        && "tidepool-checked-dependency-executable module=MetadataQuoteSupport bytecode=True object=False" `elem` lines candidateQuoteDiagnostics
        && "tidepool-count name=candidate_source_load_required count=1" `elem` lines candidateQuoteDiagnostics) $
      fail "source candidate discarded a GHC-required quoter executable"
    hidden <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "MetadataHiddenFamily.hs") [work] Nothing
    iface <- maybe (fail "hidden family owner omitted its interface") pure
      (Map.lookup (mkModuleName "MetadataHiddenFamily") (pprProductInterfaces hidden))
    let hi = work </> "hidden-family.hi"
        packagesPath = hi ++ ".packages"
    writeBinIface (targetProfile (hsc_dflags (prHscEnv (pprPipelineResult hidden))))
      QuietBinIFace NormalCompression hi iface
    bytes <- BS.readFile hi
    let artifact = ExactIfaceArtifact "main" "MetadataHiddenFamily" hi (digest bytes) []
        packages = encodePackageImports artifact
          (Map.findWithDefault emptyPackageImports (mkModuleName "MetadataHiddenFamily") (pprPackageImports hidden))
    BS.writeFile packagesPath packages
    writeExactMetadataScope scopePath [(artifact, packagesPath, digest packages)]
    ordinaryProducts <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
      (work </> "MetadataTarget.hs") [work] Nothing
    writeManifestFor ["MetadataOwner"] work ordinaryProducts
    disjoint <- compile (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile (Just scope)
      (work </> "MetadataTarget.hs") [work] Nothing
    unless (map candidateModule (pprAcceptedCandidates disjoint) == ["MetadataOwner"]
        && fmap renderType (prResultType (pprPipelineResult disjoint)) == Just "Int"
        && "MetadataHiddenFamily" `notElem`
          [moduleNameString (ms_mod_name summary)
          | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph (prHscEnv (pprPipelineResult disjoint)))]) $
      fail "disjoint source candidate lost exact hidden-owner graph isolation"
    family <- try (checked "MetadataFamilyTarget.hs") :: IO (Either SomeException CheckedEnvironmentResult)
    case family of
      Left failure | "retained family consistency" `isInfixOf` show failure -> pure ()
      _ -> fail "loaded metadata lost the hidden original family conflict"
    writeExactMetadataScope scopePath []
    (_, untrackedDiagnostics) <- captureDiagnostics (checked "MetadataUntrackedTarget.hs")
    unless ("tidepool-checked module=MetadataUntracked target=False" `elem` lines untrackedDiagnostics
        && "tidepool-checked-loaded-source module=MetadataUntracked" `notElem` lines untrackedDiagnostics) $
      fail "untracked compile-time input was promoted to loaded metadata evidence"
    receipts <- listDirectory (work </> ".exact-compilations")
    evidence <- forM receipts $ \entry -> do
      bytes' <- BS.readFile (work </> ".exact-compilations" </> entry </> "receipt.cbor")
      pure $ case deserialiseFromBytes decodeTerm (BSL.fromStrict bytes') of
        Right (_, TList [_, _, _, _, TString source, _, _, TString facts, _])
          | source == T.pack (work </> "MetadataUntrackedTarget.hs") -> T.unpack facts
        _ -> ""
    unless (any (isInfixOf "\"cache_safe\":false") evidence) $
      fail "untracked dependency was certified as cache safe"
  putStrLn "exact loaded metadata: parity, source drift, quoter bytecode, hidden family and untracked input passed"

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
    writeManifestFor ["OptionalAnchor"] work warmer
    warm <- root (PreparedProducts (Just (manifest work)))
    unless (map candidateModule (pprAcceptedCandidates warm) == ["OptionalAnchor"]) $
      fail "warm input fixture did not admit its authenticated anchor candidate"
    unless ("OptionalSupport" `elem` preparedNames warm) $
      fail "warm input fixture did not retain the executable support product"
    coldBody <- proof work "cold" cold
    warmBody <- proof work "warm" warm
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
    writeManifestFor ["OptionalAnchor", "OptionalSupport"] work warm
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
    writeManifestFor ["OptionalWiredSupport"] work wired
    verifyPackageSidecar work
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

-- Mutated fixtures retain their bytes; only the owning sidecar reader decides
-- whether old, cross-owner or unknown compiler facts can be admitted.
verifyPackageSidecar :: FilePath -> IO ()
verifyPackageSidecar work = do
  let path = work </> "OptionalWiredSupport.candidate.hi"
      packages = path ++ ".packages"
  ifaceBytes <- BS.readFile path
  original <- BS.readFile packages
  let artifact = ExactIfaceArtifact "main" "OptionalWiredSupport" path (digest ifaceBytes) []
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
        retained <- BS.readFile changedPath
        unless (retained == bytes) (fail "sidecar refusal changed retained input bytes")
      readPackageImports packages (digest original) (artifact { exactModule = "WrongOwner" }) >>= \case
        Left _ -> pure ()
        Right _ -> fail "sidecar admitted another checked interface owner"
      readPackageImports packages (digest original) (artifact { exactSha256 = replicate 64 '0' }) >>= \case
        Left _ -> pure ()
        Right _ -> fail "sidecar admitted another checked interface digest"
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
      encode globals = encodeCertifiedProducts producer [] Nothing []
        [("target", program globals)] evidence "" ""
  withTiming $ do
    let repeated = replicate 1000 (global package 0)
          ++ [global (package { symbolOccurrence = "id" }) 0]
        certify = do
          started <- getMonotonicTimeNSec
          certified <- captureDiagnostics (encode repeated >>= either fail pure)
          finished <- getMonotonicTimeNSec
          putStrLn ("package certification requests=1001 wall_ms="
            ++ show ((finished - started) `div` 1000000))
          pure certified
    (first, firstLog) <- certify
    (second, secondLog) <- certify
    unless (first == second) $ fail "repeated package certification changed its wire evidence"
    forM_ [firstLog, secondLog] $ \diagnostics -> do
      unless (count "certified_package_global_requests" diagnostics == 1001
          && count "certified_package_owner_loads" diagnostics == 1
          && count "certified_package_owner_revalidations" diagnostics == 1) $
        fail "package certification reread one owner for repeated or distinct symbols"
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
  term <- either (fail . show) (pure . snd) (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes))
  owners <- case term of
    TList [TString "TPCERT", TInt 4, _, _, _, TList rows] ->
      forM rows $ \case
        TList [_, _, _, _, owner] -> pure owner
        _ -> fail "certified global row lacks exact owner"
    _ -> fail "retained package producer used another ownership format"
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
      encodeProgram target = encodeCertifiedProducts producer [] Nothing []
        [("target", target)] evidence "" ""
      decode bytes' = either (fail . show) (pure . snd)
        (deserialiseFromBytes decodeTerm (BSL.fromStrict bytes'))
  local <- encodeProgram (localProgram [constructor, synthetic]) >>= either fail decode
  case local of
    TList [TString "TPCERT", TInt 4, _, _, TList [TList
      [TString "ghc-internal", TString "GHC.Internal.Stack.Types", TString _, TString sha]], TList []]
      | T.length sha == 64 -> pure ()
    _ -> fail "local package constructor without incoming globals lacks exact interface evidence"
  internal <- encodeProgram (localProgram [synthetic]) >>= either fail decode
  case internal of
    TList [TString "TPCERT", TInt 4, _, _, TList [], TList []] -> pure ()
    _ -> fail "noncanonical internal package helper supplied external interface authority"
  encodeProgram ((localProgram [constructor, synthetic])
    { programGlobals = [global synthetic 0] }) >>= \case
      Left _ -> pure ()
      Right _ -> fail "witnessed package module authorized a noncanonical retained sibling"
  putStrLn "local package exports: canonical constructor without globals, internal helper and synthetic demand refusal passed"
  putStrLn "retained package witnesses: authenticated map/gen0, home/gen7 and missing package refusal passed"

  where
    count :: String -> String -> Integer
    count name diagnostics = sum
      [read (drop (length prefix) line) | line <- lines diagnostics, prefix `isPrefixOf` line]
      where prefix = "tidepool-count name=" ++ name ++ " count="

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
      encode = encodeCertifiedProducts environment [] Nothing []
        [("target", program [global identity 0, global identity 0])] evidence "" ""
  first <- encode >>= either fail pure
  BS.writeFile path "invalid interface"
  encode >>= \case
    Left _ -> pure ()
    Right _ -> fail "package certification reused an owner after its interface changed"
  BS.writeFile path original
  restored <- encode >>= either fail pure
  unless (restored == first) $ fail "restored package interface did not recover certification"
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
  writeManifestFor expected work cold
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
      loadCounts = [line | line <- lines errors
        , "tidepool-count name=home_products_source_load_owners " `isPrefixOf` line]
      expectedLine = "tidepool-count name=home_products_source_load_owners count=" ++ show expectedLoad
  unless (loadCounts == [expectedLine]) $
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

forM_ :: [a] -> (a -> IO b) -> IO ()
forM_ values action = mapM_ action values

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
  interfaces <- forM ["CacheEven", "CacheOdd"] $ \name -> do
    let hi = work </> (name ++ ".candidate.hi")
    bytes <- BS.readFile hi
    iface <- maybe (fail "missing original source iface") pure
      (Map.lookup (mkModuleName name) (pprProductInterfaces cold))
    pure (ExactIfaceArtifact "main" name hi (digest bytes) [], iface)
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    fresh <- liftIO (freshExactState producer)
    let current = fresh {hsc_mod_graph = graph}
    setSession current
    restored <- hydrateCandidateHomeProducts current graph interfaces ordinary boots
    case restored of
      Left reason -> liftIO (fail ("fresh SOURCE hydration refused: " ++ reason))
      Right _ -> pure ()

writeManifest :: FilePath -> PreparedPipelineResult -> IO ()
writeManifest = writeManifestFor ["CacheEven", "CacheOdd"]

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
        <> text (dependencyImportQualifier imported) <> text (dependencyImportName imported)
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
