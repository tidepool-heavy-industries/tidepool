module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import FamilyConsistencyCases (runFamilyCases)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "family-consistency"
  [testCase "original duplicate compatible hidden associated and injective families" (runFamilyCases False)]
