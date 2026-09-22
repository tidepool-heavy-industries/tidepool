module RetainedPluginTest (verifyCompilerReuse) where

import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.IORef
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC
import GHC.Driver.Env.Types (HscEnv(..))
import GHC.Driver.Make (load', newIfaceCache)
import GHC.Driver.Plugins
import GHC.Driver.Session (updOptLevel)
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..))
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import System.Directory (copyFile)
import System.FilePath ((</>))
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.RetainedUnfoldings (installRetainedUnfoldingsPlugin, scopeRetainedModuleGraph)

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
  retainedRef <- newIORef Set.empty
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
    let installed = installRetainedUnfoldingsPlugin retainedRef env
        plugins = hsc_plugins installed
    setSession (installed { hsc_plugins = plugins
      { staticPlugins = counting : staticPlugins plugins } })
    forM_ steps $ \(retained, expected) -> do
      liftIO (writeIORef compiledRef Map.empty)
      liftIO (writeIORef retainedRef retained)
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
