module Main (main) where

import System.Environment (getArgs)
import ExecutionSourceDecodeTest (executionSourceDecodeBenchmark)

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  iterations : files | not (null files) -> executionSourceDecodeBenchmark iterations files
  _ -> fail "usage: execution-source-decode-benchmark ITERATIONS GRAPH..."
