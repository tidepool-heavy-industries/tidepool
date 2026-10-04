module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testGroup)
import Test.Tasty (localOption)
import Test.Tasty.QuickCheck (QuickCheckTests(..), testProperty)
import SwarmSpec (properties)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = localOption (QuickCheckTests 200) $
  testGroup "swarm-spec" [testProperty name property | (name, property) <- properties]
