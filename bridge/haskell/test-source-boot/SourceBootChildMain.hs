module Main (main) where

import System.Environment (getArgs)
import Text.Read (readMaybe)
import SourceBootCases (reuseFresh, requireReused, requireMixed)

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  ["--fresh", work] -> reuseFresh work >>= requireReused "fresh worker"
  ["--mixed-fresh", work, rawCount] -> case readMaybe rawCount of
    Just count | count > 0 -> reuseFresh work >>= requireMixed count
    _ -> fail "mixed fresh worker requires a positive owner count"
  _ -> fail "source-boot child requires a fresh-worker role and scratch directory"
