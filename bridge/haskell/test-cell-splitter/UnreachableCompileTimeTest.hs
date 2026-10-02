{-# LANGUAGE OverloadedStrings #-}
module UnreachableCompileTimeTest (unreachableCompileTimeCompilation) where

import Control.Exception (bracket, finally)
import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC.Unit.Module (moduleName, moduleNameString)
import System.Directory
  ( copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile )
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile, readFile')
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionProjection (ProjectionContext(..), projectPrepared)
import Tidepool.ExecutionSchema (Architecture(..), Endianness(..), TargetDescriptor(..), SymbolIdentity(..))
import Tidepool.GhcPipeline
import Tidepool.PreparedStg (PreparedModule(..))

unreachableCompileTimeCompilation :: IO ()
unreachableCompileTimeCompilation = bracket temporary removeDirectoryRecursive $ \root -> do
  let counter = root </> "counter.txt"
      target = root </> "UnusedCompileTimeTarget.hs"
  mapM_ (\name -> copyFile ("test-cell-splitter/fixtures/unreachable-compile-time" </> name) (root </> name))
    ["UnusedCompileTime.hs", "UnusedCompileTimeTarget.hs"]
  previousCounter <- lookupEnv "TIDEPOOL_TEST_UNUSED_SPLICE_COUNTER"
  previousTiming <- lookupEnv "TIDEPOOL_TIMING"
  let restore name = maybe (unsetEnv name) (setEnv name)
      measure compile = do
        writeFile counter ""
        prepared <- compile
        let owners = map (moduleNameString . moduleName . pmModule) (pprModules prepared)
        unless (owners == ["UnusedCompileTimeTarget"]) $
          fail ("unreachable imported body entered the artifact: " ++ show owners)
        let context = ProjectionContext "counter-test" "ghc-9.12.2"
              (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
              (SymbolIdentity "main" "UnusedCompileTimeTarget" "value" "result" Nothing)
              [] Nothing Nothing Nothing Nothing
        program <- either (fail . show) pure (projectPrepared context (pprModules prepared))
        let bytes = encodeWireProgram program
        unless (BS.length bytes > 0) (fail "empty prepared artifact")
        events <- lines <$> readFile' counter
        unless (all (== "splice") events) (fail "unexpected scratch counter content")
        pure (length events, bytes)
  (do
      setEnv "TIDEPOOL_TEST_UNUSED_SPLICE_COUNTER" counter
      setEnv "TIDEPOOL_TIMING" "1"
      (directCount, directBytes) <- measure (runPipelineSelected PreparedStg target [root])
      (residentCount, residentBytes) <- measure $
        withResidentPipelineSelected [root] $ \compile ->
          compile PreparedStg Set.empty GeneralCompile Nothing target [] Nothing
      unless (directCount > 0) (fail "counter control did not execute a splice")
      unless (residentCount == directCount) $
        fail ("resident request repeated unreachable compile-time IO: direct="
          ++ show directCount ++ " resident=" ++ show residentCount)
      unless (residentBytes == directBytes) (fail "resident memo changed the reachable artifact")
      putStrLn ("unreachable compile-time counter: direct=" ++ show directCount
        ++ " resident=" ++ show residentCount ++ " identical_artifact_bytes=" ++ show (BS.length residentBytes)))
    `finally` (restore "TIDEPOOL_TEST_UNUSED_SPLICE_COUNTER" previousCounter
      >> restore "TIDEPOOL_TIMING" previousTiming)
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-unreachable-compile-time"
      hClose handle
      removeFile path
      createDirectory path
      pure path
