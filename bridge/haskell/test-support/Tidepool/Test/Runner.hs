module Tidepool.Test.Runner
  ( TestTree, runTests, testCase, testGroup, requiredInput ) where

import System.Environment (lookupEnv)
import System.IO (hPutStrLn, stderr)
import Test.Tasty (TestTree, defaultMainWithIngredients, defaultIngredients, sequentialTestGroup)
import Test.Tasty.HUnit (testCase)
import Test.Tasty.Ingredients (Ingredient(..))
import Test.Tasty.Ingredients.Basic (testsNames)
import Test.Tasty.Runners (DependencyType(AllFinish))

-- Compiler cases mutate process-wide GHC state. Continue after failures, but
-- never overlap cases within a suite. Buck may isolate independent suites.
testGroup :: String -> [TestTree] -> TestTree
testGroup name = sequentialTestGroup name AllFinish

-- Standard Tasty discovery and patterns own selection. A miss is a failed
-- selection, including when --list-tests is used to preview that selection.
runTests :: TestTree -> IO ()
runTests = defaultMainWithIngredients (nonemptySelection : defaultIngredients)
  where
    nonemptySelection = TestManager [] $ \options tree ->
      if null (testsNames options tree)
        then Just (hPutStrLn stderr "error: Haskell test selection matched zero cases" >> pure False)
        else Nothing

-- Inputs belong to the build graph; an absent resource is never a skipped test.
requiredInput :: String -> IO FilePath
requiredInput name = lookupEnv name >>= maybe
  (fail ("missing declared Haskell test input: " ++ name)) pure
