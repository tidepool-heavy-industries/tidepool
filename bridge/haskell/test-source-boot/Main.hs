module Main (main) where

import Codec.CBOR.Encoding (encodeBool, encodeListLen, encodeString)
import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm)
import Data.ByteString.Lazy qualified as BSL
import Control.Exception (SomeException, bracket, finally, try)
import Control.Monad (forM, unless)
import GHC.Clock (getMonotonicTimeNSec)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Char8 qualified as BSC
import Data.List (isPrefixOf)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC (runGhc, setSession, ms_mod_name, ms_hsc_src, parseModule, typecheckModule)
import GHC.Driver.Env (HscEnv(..))
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries', mkModuleGraph)
import GHC.Types.SourceFile (HscSource(..))
import Control.Monad.IO.Class (liftIO)
import GHC.Driver.Session (targetProfile)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Unit.Module (mkModuleName, moduleName, moduleNameString)
import Numeric (showHex)
import System.Directory
  ( copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive
  , removeFile )
import System.Environment (getArgs, getExecutablePath, setEnv)
import System.Exit (ExitCode(..))
import System.FilePath ((</>))
import System.IO (hClose, hPutStrLn, openTempFile, stderr)
import System.Process (readProcessWithExitCode)
import Tidepool.CertifiedProducts (encodeCertifiedProducts)
import Tidepool.ExecutionSchema
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencyImport(..)
  , DependencyResolution(..), dependencySourceSha256, sourceEvidence, selectedHomeRequirements )
import Tidepool.ExactHydration (ExactIfaceArtifact(..), freshExactState, installExactLexicalGraph)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.HomeProducts (hydrateCandidateHomeProducts)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), PipelineResult(..)
  , CompilePurpose(..), runPipelineSelected, runPipelineSessionSelected, withResidentPipelineSelected )
import Tidepool.ModuleCandidates (ModuleCandidate(..))
import Tidepool.PackageWitness (encodePackageImports)
import Tidepool.PreparedStg (PreparedModule(..))

main :: IO ()
main = getArgs >>= \case
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
    rebuilt <- liftIO (installExactLexicalGraph (mkModuleGraph consumers) lexical producer)
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
  putStrLn "retained package witnesses: authenticated map/gen0, home/gen7 and missing package refusal passed"

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
          (Map.findWithDefault [] key (pprPackageRoots cold))
        packagePath = hi ++ ".packages"
        text = encodeString . T.pack
        imports = dependencyModuleImports node
    BS.writeFile packagePath packages
    pure $ encodeListLen 13
      <> foldMap text [dependencyModuleUnit node, name, dependencyModuleSource node
        , dependencySourceSha256 source, hi, digest bytes]
      <> foldMap encodeString (replicate 3 (T.replicate 64 "0"))
      <> encodeListLen (fromIntegral (length imports))
      <> foldMap (\imported -> encodeListLen 4
        <> text (dependencyImportQualifier imported) <> text (dependencyImportName imported)
        <> encodeBool (dependencyImportBoot imported)
        <> text (maybe "" id (dependencyImportSelected imported))) imports
      <> encodeListLen 0 <> text packagePath <> text (digest packages)
  BS.writeFile (manifest work) (toStrictByteString
    (encodeListLen 3 <> encodeString "TPMCAN" <> encodeString "5"
      <> encodeListLen (fromIntegral (length candidates)) <> mconcat candidates))

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
