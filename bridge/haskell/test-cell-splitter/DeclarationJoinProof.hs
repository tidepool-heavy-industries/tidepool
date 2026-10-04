module Main where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (bracket, try)
import Control.Monad (forM_, unless, void)
import Tidepool.DiagJson (DependencyLoadFailure(..), Diag(..), DiagSeverity(..))
import System.Directory (copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.GhcPipeline

-- Each case imports immutable source modules through a fresh checked environment.
-- A join wrapper is compiled first, then a consumer tests what that wrapper
-- actually makes available. The result deliberately distinguishes a rejected
-- join from an error deferred until use by a later cell.
main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "declaration-join-proof"
  [testCase name (scenario name modules joinAccepted consumerAccepted)
  | (name, modules, joinAccepted, consumerAccepted) <- cases]
  where
    cases =
      [ ("hidden", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, False)
      , ("shadowed", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, True)
      , ("distinct-instances", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, True)
      , ("duplicate-instances", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, False)
      , ("class-conflict", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], False, False)
      , ("family-conflict", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], False, False)
      ]

scenario :: String -> [FilePath] -> Bool -> Bool -> IO ()
scenario name modules joinAccepted consumerAccepted =
  bracket temporary removeDirectoryRecursive $ \root -> do
    let fixture = "test-cell-splitter/fixtures/declaration-join" </> name
    forM_ modules $ \file -> copyFile (fixture </> file) (root </> file)
    withResidentPipelineSelectedRequests [root] $ \runRequest -> do
      let check file = try (runRequest (pure ()) $ \compile ->
            void (compile CheckedEnvironment mempty GeneralCompile Nothing
              (root </> file) [root] Nothing))
            :: IO (Either DependencyLoadFailure ())
      join <- check "Join.hs"
      assertOutcome name "join" joinAccepted join
      consumer <- check "Consumer.hs"
      assertOutcome name "consumer" consumerAccepted consumer
  where
    assertOutcome name phase expected result = do
      case result of
        Left (DependencySourceFailure diagnostics) ->
          unless (any (\diag -> dSeverity diag == DiagError && dFile diag /= Nothing
              && not (null (dMessage diag))) diagnostics) $
            fail (name ++ " " ++ phase ++ " lost its source error evidence")
        Left DependencyWorkerFailure -> fail (name ++ " " ++ phase ++ " failed in the worker")
        Right _ -> pure ()
      unless (either (const False) (const True) result == expected) $
        fail (name ++ " " ++ phase ++ " unexpectedly " ++
          either (const "failed") (const "compiled") result)

    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-declaration-join"
      hClose handle
      removeFile path
      createDirectory path
      pure path
