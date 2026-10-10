module RetainedPluginTest (verifyCompilerReuse, verifyPreparedScope) where

import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.IORef
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC
import GHC.Driver.Env (hsc_HPT)
import GHC.Driver.Env.Types (HscEnv(..))
import GHC.Driver.Make (load', newIfaceCache)
import GHC.Driver.Plugins
import GHC.Driver.Session (updOptLevel)
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries')
import GHC.Unit.Module.ModIface (mi_final_exts, mi_plugin_hash)
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import System.Directory (copyFile, renameFile)
import System.FilePath ((</>))
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.RetainedUnfoldings
  ( installRetainedUnfoldingsPlugin, scopeRetainedModuleGraph
  , scopeRetainedSummaryHscEnv, retainedContext, emptyRetainedContext )
import Tidepool.GhcPipeline
  ( PipelineSelection(..), pprPipelineResult, pprModules, CompilePurpose(..), prHscEnv
  , withResidentPipelineSelected )
import Tidepool.PreparedStg (pmModule, pmBindings)

-- Count actual compiler passes per module, independently of Tidepool's
-- separate Core memo, through the resident daemon's warm interface cache.
-- Reusing the same home interfaces must skip Core for an
-- unchanged set. A changed set recompiles the module that defines the
-- retained identities, and its consumer through the changed interface; a
-- library module that defines none of them is never recompiled.
verifyCompilerReuse :: FilePath -> IO ()
verifyCompilerReuse dir = do
  forM_ ["ImportProducerExposed.hs", "ImportConsumerExposed.hs", "RetainedUnrelatedLibrary.hs"] $ \name ->
    copyFile ("test-prepared-stg" </> name) (dir </> name)
  compiledRef <- newIORef (Map.empty :: Map.Map String Int)
  libdir <- getLibdir
  let a = Set.fromList [identity "producerValue", identity "producerFn"]
      b = Set.singleton (identity "producerFn")
      identity name = SymbolIdentity "main" "ImportProducerExposed" "value" name Nothing
      producer = "ImportProducerExposed"
      consumer = "ImportConsumerExposed"
      library = "RetainedUnrelatedLibrary"
      observer = defaultPlugin
        { pluginRecompile = purePlugin
        , installCoreToDos = \_ todos -> pure
            (CoreDoPluginPass "CountHomeCompiles" (\guts -> do
               let name = moduleNameString (moduleName (mg_module guts))
               liftIO (modifyIORef' compiledRef (Map.insertWith (+) name 1))
               pure guts) : todos)
        }
      counting = StaticPlugin (PluginWithArgs observer []) True
      steps =
        [ (Set.empty, [producer, consumer, library])
        , (a, [producer, consumer])
        , (a, [])
        , (b, [producer, consumer])
        , (a, [producer, consumer])
        , (Set.empty, [producer, consumer])
        ]
  cache <- newIfaceCache
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags ((updOptLevel 2 flags)
      { importPaths = [dir], ghcLink = NoLink
      , objectDir = Just dir, hiDir = Just dir })
    env <- getSession
    let installed = installRetainedUnfoldingsPlugin emptyRetainedContext env
        plugins = hsc_plugins installed
    setSession (installed { hsc_plugins = plugins
      { staticPlugins = counting : staticPlugins plugins } })
    forM_ steps $ \(retained, expected) -> do
      liftIO (writeIORef compiledRef Map.empty)
      current <- getSession
      setSession (installRetainedUnfoldingsPlugin (retainedContext retained) current)
      targets <- mapM (\name -> guessTarget (dir </> name ++ ".hs") Nothing Nothing)
        [consumer, library]
      setTargets targets
      graph <- depanal [] False
      result <- load' (Just cache) LoadAllTargets mkUnknownDiagnostic Nothing
        (scopeRetainedModuleGraph graph)
      compiled <- liftIO (readIORef compiledRef)
      let expectedCounts = Map.fromList [(name, 1) | name <- expected]
      liftIO $ unless (succeeded result && compiled == expectedCounts)
        (ioError (userError ("retained plugin recompilation under "
          ++ show (Set.toList retained) ++ ": expected "
          ++ show expectedCounts ++ ", got " ++ show compiled)))
  where
    succeeded Succeeded = True
    succeeded Failed = False

-- The custom prepared path rebuilds an interface after GHC's load phase.
-- Its saved HscEnv must have the same module scope as load's compileOne path.
-- A large unrelated retained set must not perturb that interface's plugin
-- fingerprint or the prepared result on a warm resident compiler.
verifyPreparedScope :: FilePath -> IO ()
verifyPreparedScope dir = do
  let target = dir </> "RetainedTarget.hs"
      producerFile = dir </> "ImportProducerExposed.hs"
      replacementFile = dir </> "ImportProducerExposed.next"
      relevant = SymbolIdentity "main" "ImportProducerExposed" "value" "producerFn" Nothing
      unrelated = Set.fromList
        [ SymbolIdentity "main" "Unrelated" "value" ("symbol" <> Text.pack (show n)) Nothing
        | n <- [1 .. 1000 :: Int] ]
      shape result =
        [ (moduleNameString (moduleName (pmModule prepared)), length (pmBindings prepared))
        | prepared <- pprModules result ]
      preparedPluginHash result = case lookupHpt
          (hsc_HPT (prHscEnv (pprPipelineResult result)))
          (mkModuleName "ImportProducerExposed") of
        Just info -> mi_plugin_hash (mi_final_exts (hm_iface info))
        Nothing -> error "prepared producer interface was not registered"
  writeFile target $ unlines
    [ "{-# LANGUAGE QuasiQuotes #-}"
    , "module RetainedTarget where"
    , "import ImportConsumerExposed (consumerResult)"
    , "result :: Int"
    , "result = consumerResult"
    ]
  originalProducer <- readFile producerFile
  let producerSource = "{-# LANGUAGE QuasiQuotes #-}\n" ++ originalProducer
  let replaceProducer contents = do
        writeFile replacementFile contents
        renameFile replacementFile producerFile
  replaceProducer producerSource
  producerSummary <- withResidentPipelineSelected [dir] $ \compile -> do
    base <- compile PreparedStg (Set.singleton relevant) GeneralCompile Nothing target [] Nothing
    warm <- compile PreparedStg (Set.insert relevant unrelated) GeneralCompile Nothing target [] Nothing
    replaceProducer (producerSource ++ "\n-- force fresh prepared interface\n")
    crowded <- compile PreparedStg (Set.insert relevant unrelated) GeneralCompile Nothing target [] Nothing
    replaceProducer producerSource
    recovered <- compile PreparedStg (Set.singleton relevant) GeneralCompile Nothing target [] Nothing
    changed <- compile PreparedStg Set.empty GeneralCompile Nothing target [] Nothing
    unless (shape base == shape warm && shape warm == shape crowded
        && shape crowded == shape recovered)
      (ioError (userError "unrelated retained identities changed prepared module shape"))
    unless (preparedPluginHash base == preparedPluginHash warm
        && preparedPluginHash warm == preparedPluginHash crowded
        && preparedPluginHash crowded == preparedPluginHash recovered
        && preparedPluginHash base /= preparedPluginHash changed)
      (ioError (userError "prepared interface used an unscoped retained fingerprint"))
    case [summary | ModuleNode _ summary <- mgModSummaries'
            (hsc_mod_graph (prHscEnv (pprPipelineResult base)))
          , ms_mod_name summary == mkModuleName "ImportProducerExposed"] of
      [summary] -> pure summary
      _ -> ioError (userError "prepared producer omitted its actual module summary")
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags
    env <- getSession
    let installed = installRetainedUnfoldingsPlugin emptyRetainedContext env
        original = installRetainedUnfoldingsPlugin (retainedContext (Set.singleton relevant)) env
        pluginFor scoped = case staticPlugins (hsc_plugins scoped) of
          plugin : _ -> spPlugin plugin
          [] -> error "retained plugin was not installed"
        fingerprint owningEnvironment = do
          let plugin = pluginFor (scopeRetainedSummaryHscEnv producerSummary owningEnvironment)
          result <- liftIO (pluginRecompile (paPlugin plugin) (paArguments plugin))
          case result of
            MaybeRecompile hash -> pure hash
            _ -> error "custom module environment was not scoped"
    baseline <- fingerprint original
    withUnrelated <- fingerprint (installRetainedUnfoldingsPlugin
      (retainedContext (Set.insert relevant unrelated)) env)
    changed <- fingerprint installed
    recovered <- fingerprint original
    let unscoped = pluginFor installed
    unscopedResult <- liftIO (pluginRecompile (paPlugin unscoped) (paArguments unscoped))
    liftIO $ case unscopedResult of
      ForceRecompile -> pure ()
      _ -> ioError (userError "unscoped retained plugin accepted an interface fingerprint")
    let other = StaticPlugin (PluginWithArgs defaultPlugin ["other-plugin", "argument"]) True
        installedPlugins = hsc_plugins installed
        withOther = installed { hsc_plugins = installedPlugins
          { staticPlugins = other : staticPlugins installedPlugins } }
        scopedOthers = staticPlugins (hsc_plugins (scopeRetainedSummaryHscEnv producerSummary withOther))
    liftIO $ case scopedOthers of
      first : _ | paArguments (spPlugin first) == ["other-plugin", "argument"]
                  && spInitialised first -> pure ()
      _ -> ioError (userError "retained scope changed another static plugin")
    liftIO $ unless (baseline == withUnrelated && baseline /= changed && baseline == recovered)
      (ioError (userError "scoped interface fingerprint changed with unrelated retained identities"))
