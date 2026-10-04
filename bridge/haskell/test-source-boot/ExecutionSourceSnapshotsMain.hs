module Main (main) where

import System.Environment (getArgs)
import ExecutionSourceDecodeTest (executionSourceDecodeSnapshots)

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  output : files | not (null files) -> executionSourceDecodeSnapshots output files
  _ -> fail "usage: execution-source-snapshots OUTPUT GRAPH..."
