module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testGroup, testCase)
import Test.Tasty (askOption, localOption)
import Test.Tasty.QuickCheck (QuickCheckTests(..), QuickCheckMaxSize(..), QuickCheckReplay(..), testProperty)
import Test.QuickCheck (stdArgs, Args(..))
import Test.QuickCheck.Random (mkQCGen)
import SwarmSpec (properties, allocationContractHistories, allocationContractPartitions)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "swarm-spec" $
  [ testCase "splitAllowance deterministic contract partitions" allocationContractPartitions
  , askOption $ \(QuickCheckTests count) ->
    askOption $ \(QuickCheckMaxSize size) ->
    askOption $ \seed ->
      let replaySeed = case seed of
            QuickCheckReplayNone -> Nothing
            QuickCheckReplayLegacy legacy -> Just (mkQCGen legacy, 0)
            QuickCheckReplay value -> Just value
      in testCase "splitAllowance generated contract" $
        allocationContractHistories stdArgs {maxSuccess=count, maxSize=size, replay=replaySeed}
  ] ++ [localOption (QuickCheckTests 200) (testProperty name property) | (name,property) <- properties]
