module Tidepool.Test.Runner
  ( TestTree, runTests, testCase, testGroup, requiredInput ) where

import System.Environment (lookupEnv)
import System.IO (hPutStrLn, stderr)
import Test.Tasty (TestTree, defaultMainWithIngredients, defaultIngredients, localOption)
import Test.Tasty qualified as Tasty
import Test.Tasty.HUnit (testCase)
import Test.Tasty.Ingredients (Ingredient(..))
import Test.Tasty.Ingredients.Basic (testsNames)
import Test.Tasty.Runners (NumThreads(..))

-- These groups have no dependency edges: selecting a later case must not
-- select earlier compiler cases. The runner serializes the selected leaves.
testGroup :: String -> [TestTree] -> TestTree
testGroup = Tasty.testGroup

-- Standard Tasty discovery and patterns own selection. A miss is a failed
-- selection, including when --list-tests is used to preview that selection.
runTests :: TestTree -> IO ()
runTests tree = defaultMainWithIngredients
  (nonemptySelection : defaultIngredients)
  -- Tasty applies root options before starting its scheduler. GHC cases share
  -- process state; Buck provides concurrency by isolating independent suites.
  (localOption (NumThreads 1) tree)
  where
    nonemptySelection = TestManager [] $ \options selectedTree ->
      if null (testsNames options selectedTree)
        then Just (hPutStrLn stderr "error: Haskell test selection matched zero cases" >> pure False)
        else Nothing

-- Inputs belong to the build graph; an absent resource is never a skipped test.
requiredInput :: String -> IO FilePath
requiredInput name = lookupEnv name >>= maybe
  (fail ("missing declared Haskell test input: " ++ name)) pure
