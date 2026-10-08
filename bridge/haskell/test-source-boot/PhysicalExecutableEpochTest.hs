module PhysicalExecutableEpochTest (physicalExecutableEpoch, exactInterfaceOwnerReuse) where

import Control.Concurrent (forkIO, killThread, threadDelay)
import Control.Concurrent.MVar (newEmptyMVar, putMVar, readMVar, takeMVar)
import Control.Exception (AsyncException(ThreadKilled), SomeException, bracket, displayException, finally, throwIO, try, fromException, evaluate)
import Control.Monad (forM, unless)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import Data.List (isInfixOf)
import Data.Maybe (isJust, isNothing)
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC.Driver.Env (HscEnv(..), hsc_HPT, hscEPS)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Linker.Loader qualified as Linker
import GHC.Linker.Types (linkableModule, linkableObjs, linkableLibs)
import GHC.Runtime.Interpreter.Types
  ( Interp(..), InterpInstance(ExternalInterp), ExtInterp(ExtIServ), ExtInterpState(..)
  , InterpStatus(..), ExtInterpInstance(..), InterpProcess(..) )
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt, lookupHpt)
import GHC.Unit.Env (UnitEnv(..))
import GHC.Unit.External (ExternalUnitCache(..), ExternalPackageState(..))
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Env (moduleEnvElts)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Module.Graph (mgModSummaries')
import GHC.Unit.Types (unitString)
import SourceBootFixtureSupport (captureDiagnostics, withScratch, withTiming)
import System.Directory (copyFile, doesFileExist)
import System.FilePath ((</>))
import System.IO (hPutStr, stderr)
import System.Mem.StableName (StableName, makeStableName)
import System.Process (ProcessHandle, getPid, getProcessExitCode)
import System.Timeout (timeout)
import Numeric (showHex)
import Tidepool.GhcPipeline
  ( CompilerScope(..), CompilerTransactionFailure(..), CheckedEnvironmentResult(..), PipelineSelection(..)
  , CompilePurpose(..), renderType
  , withResidentCompilerScopes, withScopedExactInterfaceTransaction )
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), ExactInterfaceOperations, runExactInterfaceOperation
  , serializeOriginalInterface )
import Tidepool.DeclarationJoin
  ( DeclarationArtifact(..), DeclarationJoinInput(..), DeclarationInventory(..), DeclarationInventoryOutcome(..)
  , DeclarationJoinOutcome(..), InstanceInventory(..), JoinDecision(..), JoinRejection(..), ModuleSnapshot(..)
  , ReservedJoin(..), inspectDeclarationArtifacts, validateDeclarationJoin )

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

-- Source-free declaration operations must borrow the compiler's resident GHC
-- owner. Real native Template Haskell calls bracket inspection and a valid
-- join so the interpreter image and package-interface cells have observable use.
exactInterfaceOwnerReuse :: IO ()
exactInterfaceOwnerReuse = withTiming $ withScratch $ \work -> do
  let fixtures = "test-source-boot/fixtures"
      provider = work </> "NativeEpochProvider.hs"
      firstTarget = work </> "NativeEpochTarget.hs"
      secondTarget = work </> "NativeEpochTargetAgain.hs"
      recoveryTarget = work </> "NativeEpochTargetAfterCancellation.hs"
      marker = work </> "native-calls"
      interfacePath = work </> "NativeEpochProvider.original.hi"
      joinedPath = work </> "JoinedNativeEpochProvider.hi"
      snapshot bytes = ModuleSnapshot "NativeEpochProvider" interfacePath (digest bytes)
      compile scope path = scopedCompile scope CheckedEnvironment Set.empty GeneralCompile
        Nothing path [work] Nothing
      writeProvider = renderFixture (fixtures </> "NativeEpochProvider.hs") provider
        [("EPOCH_VALUE", "41"), ("\"EPOCH_MARKER\"", show marker)]
  writeProvider
  nativeTarget <- T.pack <$> readFile (fixtures </> "NativeEpochTarget.hs")
  writeFile firstTarget (T.unpack nativeTarget)
  writeFile secondTarget $ T.unpack $ T.replace "NativeEpochTarget" "NativeEpochTargetAgain" nativeTarget
  writeFile recoveryTarget $ T.unpack $ T.replace "NativeEpochTarget" "NativeEpochTargetAfterCancellation" nativeTarget

  escaped <- newIORef Nothing :: IO (IORef (Maybe ExactInterfaceOperations))
  processBeforeCancellationRef <- newIORef Nothing :: IO (IORef (Maybe (Integer, ProcessHandle)))
  completed <- timeout 120000000 $ withResidentCompilerScopes [work] $ \runScope -> do
    runScope (pure ()) $ \compiler -> do
      first <- compile compiler firstTarget
      let firstEnvironment = crHscEnv first
      interp <- maybe (fail "first exact-owner splice has no interpreter") pure (hsc_interp firstEnvironment)
      firstProcess <- runningProcess interp
      requireNativeValue 41 first
      requireNativeOwner interp firstEnvironment
      requireCall marker 41 firstProcess
      requireCallCount marker 41 firstProcess 1

      providerHome <- maybe (fail "the compiled provider has no home interface") pure
        (lookupHpt (hsc_HPT firstEnvironment) (mkModuleName "NativeEpochProvider"))
      interfaceBytes <- serializeOriginalInterface firstEnvironment work (hm_iface providerHome)
      BS.writeFile interfacePath interfaceBytes
      let artifact = ExactIfaceArtifact "main" "NativeEpochProvider" interfacePath
            (digest interfaceBytes) []
          declarationArtifact = DeclarationArtifact artifact Nothing
      withScopedExactInterfaceTransaction compiler [work] $ \operations -> do
        (beforeEpsOwner, beforeExternal) <- runExactInterfaceOperation operations $ \environment -> do
          requireEmptyExactHome environment
          external <- hscEPS environment
          pure (euc_eps (ue_eps (hsc_unit_env environment)), external)
        beforeCells <- packageInterfaceCells beforeExternal
        unless (not (null beforeCells))
          (fail "the resident compiler has no package interface cells to retain")
        inspected <- inspectDeclarationArtifacts operations [declarationArtifact]
        inventory <- case inspectionResult inspected of
          Right [entry] | inventoryArtifact entry == declarationArtifact -> pure entry
          other -> fail ("source-free exact interface inspection failed: " ++ show other)
        let joinInput = DeclarationJoinInput
              { expectedPublicVersion = "paired-public-snapshot"
              , publicModule = Just (snapshot interfaceBytes)
              , privateBase = Nothing
              , privateTip = Nothing
              , declarationWrites = []
              , joinReservation = ReservedJoin "main" "JoinedNativeEpochProvider" joinedPath
              , expectedExports = inventoryExports inventory
              , expectedInstances = inventoryInstances inventory
              , implementationArtifacts = [declarationArtifact]
              , retainedFamilyClosure = inventoryFamilies (inventoryInstances inventory)
              }
        joined <- validateDeclarationJoin operations joinInput
        case outcomeDecision joined of
          JoinAccepted | Just output <- outcomeArtifact joined
            , snapshotPath output == joinedPath -> do
                exists <- doesFileExist joinedPath
                unless exists (fail "accepted declaration join emitted no interface artifact")
          other -> fail ("real declaration join failed: " ++ show other)

        (afterEpsOwner, afterExternal) <- runExactInterfaceOperation operations $ \environment -> do
          requireEmptyExactHome environment
          external <- hscEPS environment
          pure (euc_eps (ue_eps (hsc_unit_env environment)), external)
        unless (beforeEpsOwner == afterEpsOwner)
          (fail "source-free operations did not reuse the resident EPS owner")
        afterJoinCells <- packageInterfaceCells afterExternal
        requirePackageInterfaceReuse beforeCells afterJoinCells

        BS.writeFile interfacePath (interfaceBytes <> BS.singleton 0)
        changed <- inspectDeclarationArtifacts operations [declarationArtifact]
          `finally` BS.writeFile interfacePath interfaceBytes
        case inspectionResult changed of
          Left (ArtifactChanged, _) -> pure ()
          other -> fail ("changed exact artifact was not refused: " ++ show other)

        reentrant <- runExactInterfaceOperation operations $ \_ ->
          try (inspectDeclarationArtifacts operations [declarationArtifact])
            :: IO (Either SomeException DeclarationInventoryOutcome)
        case reentrant of
          Left failure | Just CompilerTransactionBusy <- fromException failure -> pure ()
          other -> fail ("nested exact operation did not hit the busy guard: " ++ showResult other)

      afterInterface <- runningProcess interp
      unless (fst afterInterface == fst firstProcess)
        (fail "source-free exact operations replaced the resident interpreter process")
      requireNativeOwner interp firstEnvironment
      (second, secondDiagnostics) <- captureDiagnostics (compile compiler secondTarget)
      hPutStr stderr secondDiagnostics
      requireThFresh secondDiagnostics
      requireRotation "epoch" secondDiagnostics
      requireNativeValue 41 second
      secondProcess <- runningProcess interp
      unless (fst secondProcess /= fst firstProcess)
        (fail "the source-fresh second splice did not rotate its native interpreter image")
      requireStopped firstProcess
      writeIORef processBeforeCancellationRef (Just secondProcess)
      requireNativeOwner interp (crHscEnv second)
      requireCall marker 41 secondProcess
      requireCallCount marker 41 secondProcess 1

      withScopedExactInterfaceTransaction compiler [work] $ \operations -> do
        writeIORef escaped (Just operations)
        cancelled <- try (runExactInterfaceOperation operations $ \_ -> throwIO ThreadKilled)
          :: IO (Either SomeException ())
        case cancelled of
          Left failure | Just ThreadKilled <- fromException failure -> pure ()
          other -> fail ("async exact operation did not preserve cancellation: " ++ showResult other)
        reused <- try (inspectDeclarationArtifacts operations [])
          :: IO (Either SomeException DeclarationInventoryOutcome)
        case reused of
          Left failure | Just CompilerTransactionFailed <- fromException failure -> pure ()
          other -> fail ("cancelled exact operation capability was still usable: " ++ showResult other)

    runScope (pure ()) $ \compiler -> do
      recovered <- compile compiler recoveryTarget
      requireNativeValue 41 recovered
      recoveredInterp <- maybe (fail "post-cancellation splice has no interpreter") pure
        (hsc_interp (crHscEnv recovered))
      recoveredProcess <- runningProcess recoveredInterp
      previous <- readIORef processBeforeCancellationRef
        >>= maybe (fail "missing pre-cancellation process") pure
      unless (fst recoveredProcess /= fst previous)
        (fail "cancellation did not start a fresh native interpreter process")
      requireStopped previous
      requireNativeOwner recoveredInterp (crHscEnv recovered)
      requireCall marker 41 recoveredProcess
      requireCallCount marker 41 recoveredProcess 1
      calls <- lines <$> readFile marker
      unless (length calls == 3) $
        fail ("native Template Haskell did not call the C provider once per target: " ++ show calls)
  unless (isJust completed)
    (fail "exact-interface owner reuse and guard checks exceeded 120 seconds")

  maybeOperations <- readIORef escaped
  operations <- maybe (fail "the exact transaction did not issue its operation capability") pure maybeOperations
  released <- try (inspectDeclarationArtifacts operations []) :: IO (Either SomeException DeclarationInventoryOutcome)
  case released of
    Left failure | Just CompilerTransactionReleased <- fromException failure -> pure ()
    other -> fail ("exact operation capability survived its compiler scope: " ++ showResult other)
  putStrLn "exact interface owner: real join and inspection retained fixed package state and native image; changed artifact, reentrancy, cancellation, lifetime and recovery checks passed"

requireEmptyExactHome :: HscEnv -> IO ()
requireEmptyExactHome environment = unless
  (null (eltsHpt (hsc_HPT environment)) && null (hsc_targets environment)
    && null (mgModSummaries' (hsc_mod_graph environment))) $
  fail "exact interface operations borrowed home visibility, targets or a module graph"

requireNativeValue :: Int -> CheckedEnvironmentResult -> IO ()
requireNativeValue expected checked = unless
  (fmap renderType (crResultType checked) == Just ("Proxy " ++ show expected)) $
  fail "native Template Haskell splice returned the wrong type-level value"

requireThFresh :: String -> IO ()
requireThFresh diagnostics = unless
  (any (\line -> all (`isInfixOf` line)
    ["tidepool-reuse {", "\"stage\":\"source_frontend\"", "\"decision\":\"miss\""
    ,"\"reason\":\"th_fresh\"", "\"unit\":\"main\"", "\"module\":\"NativeEpochProvider\""]) $
    lines diagnostics) $
  fail "second native source frontend did not report the expected Template Haskell freshness event"

requireCallCount :: FilePath -> Integer -> (Integer, ProcessHandle) -> Int -> IO ()
requireCallCount marker expected (pid, _) count = do
  calls <- lines <$> readFile marker
  let ownerCalls = filter (== show expected ++ " " ++ show pid) calls
  unless (length ownerCalls == count)
    (fail ("native C call count for interpreter " ++ show pid ++ " differs: " ++ show calls))

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let digits = showHex byte "" in replicate (2 - length digits) '0' ++ digits)
  . BS.unpack . SHA.hash

packageInterfaceCells :: ExternalPackageState -> IO [(Module, StableName ModIface)]
packageInterfaceCells external = forM
  [interface | interface <- moduleEnvElts (eps_PIT external)
    , unitString (moduleUnit (mi_module interface)) /= "main"] $ \interface -> do
  forced <- evaluate interface
  stable <- makeStableName forced
  pure (mi_module forced, stable)

requirePackageInterfaceReuse :: [(Module, StableName ModIface)] -> [(Module, StableName ModIface)] -> IO ()
requirePackageInterfaceReuse before after = do
  unless (not (null after) && length after >= length before)
    (fail "fixed package interface facts shrank during exact operations")
  let retained = any (\(module', stable) -> lookup module' before == Just stable) after
  unless retained (fail "exact operations replaced every fixed package interface cell")

showResult :: Show a => Either SomeException a -> String
showResult = either displayException show

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
