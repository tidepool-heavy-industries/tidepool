module RetainedPluginTest (verifyCompilerReuse) where

import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.IORef
import Data.Set qualified as Set
import GHC
import GHC.Driver.Env.Types (HscEnv(..))
import GHC.Driver.Plugins
import GHC.Driver.Session (updOptLevel)
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..))
import System.Directory (copyFile)
import System.FilePath ((</>))
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.RetainedUnfoldings (installRetainedUnfoldingsPlugin)

-- Count actual compiler passes, independently of Tidepool's separate Core
-- memo. Reusing the same home interfaces must skip Core for an unchanged set
-- and recompile both modules when the withholding fingerprint changes.
verifyCompilerReuse :: FilePath -> IO ()
verifyCompilerReuse dir = do
  forM_ ["ImportProducerExposed.hs", "ImportConsumerExposed.hs"] $ \name ->
    copyFile ("test-prepared-stg" </> name) (dir </> name)
  retainedRef <- newIORef Set.empty
  compiledRef <- newIORef (0 :: Int)
  libdir <- getLibdir
  let a = Set.fromList [identity "producerValue", identity "producerFn"]
      b = Set.singleton (identity "producerFn")
      identity name = SymbolIdentity "main" "ImportProducerExposed" "value" name Nothing
      observer = defaultPlugin
        { pluginRecompile = purePlugin
        , installCoreToDos = \_ todos -> pure
            (CoreDoPluginPass "CountHomeCompiles" (\guts -> do
               liftIO (modifyIORef' compiledRef (+ 1))
               pure guts) : todos)
        }
      counting = StaticPlugin (PluginWithArgs observer []) True
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
    forM_ (zip [Set.empty, a, a, b, a, Set.empty] [2, 2, 0, 2, 2, 2]) $ \(retained, expected) -> do
      before <- liftIO (readIORef compiledRef)
      liftIO (writeIORef retainedRef retained)
      target <- guessTarget (dir </> "ImportConsumerExposed.hs") Nothing Nothing
      setTargets [target]
      result <- load LoadAllTargets
      after <- liftIO (readIORef compiledRef)
      liftIO $ unless (succeeded result && after - before == expected)
        (ioError (userError ("retained plugin recompilation count: expected "
          ++ show expected ++ ", got " ++ show (after - before))))
  where
    succeeded Succeeded = True
    succeeded Failed = False
