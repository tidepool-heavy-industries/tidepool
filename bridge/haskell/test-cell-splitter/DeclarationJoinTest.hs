module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import DeclarationJoinCases
import Tidepool.ExecutionSchema qualified as Execution

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "declaration-join"
  [ testCase "current typed declaration request roundtrip" wireRoundTripChecks
  , testCase "original package roots" originalProductRootsProof
  , testCase "evaluated original package roots control" $
      originalProductRootsProofWith (\global ->
        (Execution.globalIdentity global, Execution.globalRequiredEvaluated global))
  , testCase "original interfaces fresh consumers and retained family conflicts" declarationJoinScenario
  ]
