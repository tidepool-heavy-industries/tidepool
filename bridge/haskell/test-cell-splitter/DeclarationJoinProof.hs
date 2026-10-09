module Main where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (Handler(..), bracket, catches)
import Control.Monad (forM_, unless, void)
import Tidepool.DiagJson (DependencyLoadFailure(..), Diag(..), DiagSeverity(..), diagsFromSourceError)
import GHC.Types.SourceError (SourceError)
import Tidepool.DeclarationJoin (JoinRejection(..))
import Tidepool.FamilyConsistency (FamilyConsistencyRejection(..))
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
  [testCase name (scenario name modules joinExpected consumerExpected)
  | (name, modules, joinExpected, consumerExpected) <- cases]
  where
    cases =
      [ ("hidden", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], ExpectedAccepted, ExpectedSourceRefusal)
      , ("shadowed", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], ExpectedAccepted, ExpectedAccepted)
      , ("distinct-instances", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], ExpectedAccepted, ExpectedAccepted)
      , ("duplicate-instances", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], ExpectedAccepted, ExpectedSourceRefusal)
      , ("class-conflict", ["Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], ExpectedSourceRefusal, ExpectedSourceRefusal)
      , ("family-conflict", ["Common.hs", "Private.hs", "Public.hs", "Join.hs", "Consumer.hs"], ExpectedFamilyRefusal, ExpectedFamilyRefusal)
      ]

data ExpectedOutcome = ExpectedAccepted | ExpectedSourceRefusal | ExpectedFamilyRefusal
  deriving (Eq, Show)

data CompilationRefusal = SourceRefusal [Diag] | FamilyRefusal JoinRejection String | WorkerRefusal

-- Catch only the operation's classified refusal types. Unexpected exceptions
-- escape the oracle and fail the test, including unrelated IO failures.
compilationOutcome :: IO () -> IO (Either CompilationRefusal ())
compilationOutcome action = (Right <$> action) `catches`
  [ Handler $ \(failure :: SourceError) -> pure (Left (SourceRefusal (diagsFromSourceError failure)))
  , Handler $ \(failure :: DependencyLoadFailure) -> pure (Left (case failure of
      DependencySourceFailure diagnostics -> SourceRefusal diagnostics
      DependencyWorkerFailure -> WorkerRefusal))
  , Handler $ \(FamilyConsistencyRejection reason diagnostic) -> pure (Left (FamilyRefusal reason diagnostic))
  ]

scenario :: String -> [FilePath] -> ExpectedOutcome -> ExpectedOutcome -> IO ()
scenario name modules joinExpected consumerExpected =
  bracket temporary removeDirectoryRecursive $ \root -> do
    let fixture = "test-cell-splitter/fixtures/declaration-join" </> name
    forM_ modules $ \file -> copyFile (fixture </> file) (root </> file)
    withResidentPipelineSelectedRequests [root] $ \runRequest -> do
      let check file = compilationOutcome (runRequest (pure ()) $ \compile ->
            void (compile CheckedEnvironment mempty GeneralCompile Nothing
              (root </> file) [root] Nothing))
      join <- check "Join.hs"
      assertOutcome name "join" joinExpected join
      consumer <- check "Consumer.hs"
      assertOutcome name "consumer" consumerExpected consumer
  where
    assertOutcome name phase expected result = do
      actual <- case result of
        Left (SourceRefusal diagnostics) -> do
          unless (any (\diag -> dSeverity diag == DiagError && dFile diag /= Nothing
              && not (null (dMessage diag))) diagnostics) $
            fail (name ++ " " ++ phase ++ " lost its source error evidence")
          pure ExpectedSourceRefusal
        Left (FamilyRefusal FamilyInstanceConflict diagnostic) -> do
          unless (not (null diagnostic)) (fail (name ++ " " ++ phase ++ " lost its family conflict evidence"))
          pure ExpectedFamilyRefusal
        Left (FamilyRefusal reason _) -> fail (name ++ " " ++ phase ++ " rejected another declaration class: " ++ show reason)
        Left WorkerRefusal -> fail (name ++ " " ++ phase ++ " failed in the worker")
        Right () -> pure ExpectedAccepted
      unless (actual == expected) $
        fail (name ++ " " ++ phase ++ " expected " ++ show expected ++ ", observed " ++ show actual)

    temporary = do
      parent <- getTemporaryDirectory
      (path, handle) <- openTempFile parent "tidepool-declaration-join"
      hClose handle
      removeFile path
      createDirectory path
      pure path
