module Main where

import Control.Exception (bracket, try)
import Control.Monad (forM_, unless)
import GHC.Types.SourceError (SourceError)
import System.Directory (copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.GhcPipeline

-- Each case imports immutable source modules through a fresh checked environment.
-- A join wrapper is compiled first, then a consumer tests what that wrapper
-- actually makes available. The result deliberately distinguishes a rejected
-- join from an error deferred until use by a later cell.
main :: IO ()
main = forM_ cases $ \(name, modules, joinAccepted, consumerAccepted) ->
  bracket temporary removeDirectoryRecursive $ \root -> do
    let fixture = "test-cell-splitter/fixtures/declaration-join" </> name
    forM_ modules $ \file -> copyFile (fixture </> file) (root </> file)
    withResidentPipelineSelectedRequests [root] (const (pure ())) $ \runRequest -> do
      let check file = try (runRequest $ \compile ->
            compile CheckedEnvironment mempty GeneralCompile Nothing
              (root </> file) [root] Nothing)
            :: IO (Either SourceError CheckedEnvironmentResult)
      join <- check "Join.hs"
      assertOutcome name "join" joinAccepted join
      consumer <- check "Consumer.hs"
      assertOutcome name "consumer" consumerAccepted consumer
  where
    cases =
      [ ("hidden", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, False)
      , ("shadowed", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, True)
      , ("distinct-instances", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, True)
      , ("duplicate-instances", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], True, False)
      , ("class-conflict", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], False, False)
      , ("family-conflict", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], False, False)
      ]

    assertOutcome name phase expected result =
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
