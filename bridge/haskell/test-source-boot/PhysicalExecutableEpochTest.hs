module PhysicalExecutableEpochTest (physicalExecutableEpoch) where

import Control.Concurrent (forkIO, killThread, threadDelay)
import Control.Concurrent.MVar (newEmptyMVar, putMVar, readMVar, takeMVar)
import Control.Exception (SomeException, bracket, displayException, throwIO, try)
import Control.Monad (unless)
import Data.List (isInfixOf)
import Data.Maybe (isJust, isNothing)
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC.Driver.Env (HscEnv(..), hsc_HPT)
import GHC.Linker.Loader qualified as Linker
import GHC.Linker.Types (linkableModule, linkableObjs, linkableLibs)
import GHC.Runtime.Interpreter.Types
  ( Interp(..), InterpInstance(ExternalInterp), ExtInterp(ExtIServ), ExtInterpState(..)
  , InterpStatus(..), ExtInterpInstance(..), InterpProcess(..) )
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module (mkModuleName)
import GHC.Unit.Module.Env (moduleEnvElts)
import GHC.Unit.Module.ModIface (mi_module)
import SourceBootFixtureSupport (captureDiagnostics, withScratch, withTiming)
import System.Directory (copyFile, doesFileExist)
import System.FilePath ((</>))
import System.IO (hPutStr, stderr)
import System.Process (ProcessHandle, getPid, getProcessExitCode)
import System.Timeout (timeout)
import Tidepool.GhcPipeline
  ( CompilerScope(..), CheckedEnvironmentResult(..), PipelineSelection(..)
  , CompilePurpose(..), renderType, withResidentCompilerScopes )

-- C symbol replacement and partial loader failure require retiring the actual
-- interpreter process. A rolled-back LoaderState cannot unload its objects.
physicalExecutableEpoch :: IO ()
physicalExecutableEpoch = withTiming $ withScratch $ \work -> do
  let provider = work </> "NativeEpochProvider.hs"
      target = work </> "NativeEpochTarget.hs"
      marker = work </> "native-calls"
      reached = work </> "native-before-bco-failure"
      release = work </> "continue-bco-linking"
      fixtures = "test-source-boot/fixtures"
      compile scope = scopedCompile scope CheckedEnvironment Set.empty GeneralCompile
        Nothing target [work] Nothing
      writeProvider :: Int -> IO ()
      writeProvider value = renderFixture (fixtures </> "NativeEpochProvider.hs") provider
        [("EPOCH_VALUE", show value), ("\"EPOCH_MARKER\"", show marker)]
      requireValue :: Int -> CheckedEnvironmentResult -> IO ()
      requireValue expected checked = unless
        (fmap renderType (crResultType checked) == Just ("Proxy " ++ show expected)) $
          fail ("native executable result differs: " ++ show (fmap renderType (crResultType checked)))
  copyFile (fixtures </> "NativeEpochTarget.hs") target
  copyFile (fixtures </> "NativeEpochMissing.hs") (work </> "NativeEpochMissing.hs")
  writeProvider (41 :: Int)
  completed <- timeout 120000000 $ withResidentCompilerScopes [work] $ \runScope ->
    runScope (pure ()) $ \scope -> do
      first <- compile scope
      requireValue (41 :: Int) first
      interp <- maybe (fail "native quoter did not retain its interpreter") pure (hsc_interp (crHscEnv first))
      firstProcess <- runningProcess interp
      requireNativeOwner interp (crHscEnv first)
      requireCall marker 41 firstProcess

      writeProvider 42
      (second, replacement) <- captureDiagnostics (compile scope)
      hPutStr stderr replacement
      requireValue (42 :: Int) second
      secondProcess <- runningProcess interp
      requireNativeOwner interp (crHscEnv second)
      requireCall marker 42 secondProcess
      requireReplacement firstProcess secondProcess
      requireRotation "epoch" replacement

      -- The first splice calls the new C object. It waits while the observer
      -- captures that real process, then the second splice requires an absent
      -- foreign symbol and fails in GHC's BCO linker.
      writeProvider 43
      renderFixture (fixtures </> "NativeEpochFailingTarget.hs") target
        [("\"EPOCH_REACHED\"", show reached), ("\"EPOCH_RELEASE\"", show release)]
      observed <- newEmptyMVar
      let observe = do
            process <- try (do
              waitForFile reached
              process <- runningProcess interp
              writeFile release "observed actual native process\n"
              pure process)
              :: IO (Either SomeException (Integer, ProcessHandle))
            putMVar observed process
      (failed, diagnostics, failedProcess) <- bracket (forkIO observe) killThread $ \_ -> do
        (failed, diagnostics) <- captureDiagnostics
          (try (compile scope) :: IO (Either SomeException CheckedEnvironmentResult))
        started <- doesFileExist reached
        unless started $ do
          hPutStr stderr diagnostics
          either throwIO (const (fail "native call did not reach the real linker failure barrier")) failed
        process <- timeout 1000000 (takeMVar observed)
          >>= maybe (fail "native process observer did not settle") pure
          >>= either throwIO pure
        pure (failed, diagnostics, process)
      hPutStr stderr diagnostics
      requireCall marker 43 failedProcess
      requireReplacement secondProcess failedProcess
      let linkerOutput = diagnostics ++ either displayException (const "") failed
      unless (case failed of
          Left _ -> "During interactive linking" `isInfixOf` linkerOutput
            && "tidepool_epoch_missing_required" `isInfixOf` linkerOutput
          Right _ -> False) $
        fail "the real foreign-object/BCO sequence did not fail at its missing linker symbol"
      requireStopped failedProcess
      requirePending interp
      requireRotation "recovery" diagnostics

      writeProvider 41
      copyFile (fixtures </> "NativeEpochTarget.hs") target
      recovered <- compile scope
      requireValue (41 :: Int) recovered
      recoveredProcess <- runningProcess interp
      requireNativeOwner interp (crHscEnv recovered)
      requireCall marker 41 recoveredProcess
      requireReplacement failedProcess recoveredProcess
  unless (isJust completed) $
    fail "physical native epoch lifetime (including resident cleanup) exceeded 120 seconds"
  putStrLn "physical native epoch: same owner 41/42, actual object call before BCO failure, retired processes and recovered 41"

renderFixture :: FilePath -> FilePath -> [(String, String)] -> IO ()
renderFixture source destination replacements = do
  original <- T.pack <$> readFile source
  writeFile destination $ T.unpack $ foldl
    (\bytes (needle, value) -> T.replace (T.pack needle) (T.pack value) bytes) original replacements

runningProcess :: Interp -> IO (Integer, ProcessHandle)
runningProcess interp = case interpInstance interp of
  ExternalInterp (ExtIServ server) -> readMVar (interpStatus server) >>= \case
    InterpRunning instance' -> do
      let process = interpHandle (instProcess instance')
      pid <- getPid process >>= maybe (fail "native interpreter has no live process") pure
      exited <- getProcessExitCode process
      unless (isNothing exited) (fail "native interpreter process has already exited")
      pure (fromIntegral pid, process)
    InterpPending -> fail "native interpreter is not running"
  _ -> fail "physical native epochs require the pinned external interpreter"

requireNativeOwner :: Interp -> HscEnv -> IO ()
requireNativeOwner interp environment = do
  home <- maybe (fail "native provider is absent from the actual HPT") pure
    (lookupHpt (hsc_HPT environment) (mkModuleName "NativeEpochProvider"))
  state <- Linker.getLoaderState interp >>= maybe (fail "native loader state is absent") pure
  let owner = mi_module (hm_iface home)
  unless (any (\linkable -> linkableModule linkable == owner
      && not (null (linkableObjs linkable) && null (linkableLibs linkable)))
      (moduleEnvElts (Linker.objs_loaded state))) $
    fail "the source-issued native provider has no actual loaded object"

requireCall :: FilePath -> Integer -> (Integer, ProcessHandle) -> IO ()
requireCall marker expected (pid, _) = do
  calls <- lines <$> readFile marker
  unless (show expected ++ " " ++ show pid `elem` calls) $
    fail "native return value and physical interpreter PID were not observed together"

requireStopped :: (Integer, ProcessHandle) -> IO ()
requireStopped (_, process) = do
  exited <- getProcessExitCode process
  unless (isJust exited) (fail "retired native interpreter process remains live")

requireReplacement :: (Integer, ProcessHandle) -> (Integer, ProcessHandle) -> IO ()
requireReplacement previous current = do
  unless (fst previous /= fst current) (fail "incompatible native code reused its interpreter process")
  requireStopped previous

requirePending :: Interp -> IO ()
requirePending interp = case interpInstance interp of
  ExternalInterp (ExtIServ server) -> readMVar (interpStatus server) >>= \case
    InterpPending -> pure ()
    InterpRunning _ -> fail "late loader failure left a running interpreter epoch"
  _ -> fail "late loader failure used an internal interpreter"

requireRotation :: String -> String -> IO ()
requireRotation reason diagnostics = unless
  (any (\line -> all (`isInfixOf` line)
    ["\"stage\":\"native_image\"", "\"decision\":\"epoch_rotated\""
    , "\"reason\":\"" ++ reason ++ "\""]) (lines diagnostics)) $
      fail ("actual native epoch rotation lacks its " ++ reason ++ " event")

waitForFile :: FilePath -> IO ()
waitForFile path = do
  reached <- timeout 30000000 wait
  unless (isJust reached) (fail "native call did not reach the BCO linker observation barrier")
  where
    wait = doesFileExist path >>= \present -> unless present (threadDelay 10000 >> wait)
