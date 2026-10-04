module Main (main, tests) where

import Control.Monad (unless)
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import ExecutionCorpusCases (mappingSelfTest, inventorySelfTest)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "execution-corpus-mapping"
  [ testCase "exact external identity mapping" mappingSelfTest
  , testCase "typed STG inventory retains unsupported facts" $
      inventorySelfTest (\label condition -> unless condition (fail label))
  ]
