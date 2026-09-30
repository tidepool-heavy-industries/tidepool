module Main (main) where

import Codec.CBOR.Encoding (encodeBool, encodeListLen, encodeString)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (SomeException, bracket, finally, try)
import Control.Monad (forM, unless)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC (runGhc, setSession, ms_mod_name, ms_hsc_src)
import GHC.Driver.Env (HscEnv(..))
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries')
import GHC.Types.SourceFile (HscSource(..))
import Control.Monad.IO.Class (liftIO)
import GHC.Driver.Session (targetProfile)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Unit.Module (mkModuleName, moduleName, moduleNameString)
import Numeric (showHex)
import System.Directory
  ( copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive
  , removeFile )
import System.Environment (getArgs, getExecutablePath)
import System.Exit (ExitCode(..))
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (readProcessWithExitCode)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencyImport(..)
  , dependencySourceSha256, sourceEvidence )
import Tidepool.ExactHydration (ExactIfaceArtifact(..), freshExactState)
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
  ["--fresh", work] -> reuseFresh work >>= requireReused "fresh worker"
  [] -> withScratch $ \work -> do
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
  _ -> fail "unexpected SOURCE boot test arguments"

exerciseRefusals :: FilePath -> IO PreparedPipelineResult -> IO ()
exerciseRefusals work reuse = do
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
  reuse >>= requireReused "resident reuse after ABI refusal"
  -- CPP has readable inputs outside the bounded source graph. Refuse the
  -- entire SCC even when this particular source happens to typecheck.
  (BS.writeFile boot ("{-# LANGUAGE CPP #-}\n" <> original) >>
    reuse >>= requireRefused "untracked boot CPP")
    `finally` BS.writeFile boot original
  reuse >>= requireReused "resident reuse after refusal"

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
writeManifest work cold = do
  candidates <- forM ["CacheEven", "CacheOdd"] $ \name -> do
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
      <> encodeListLen 2 <> mconcat candidates))

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
