module Main (main) where

import System.Environment (getArgs)
import WorkerResponseCases (workerChild, compilerClusterChild)

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  ["--compiler-cluster", scratch] -> compilerClusterChild scratch
  ["--worker", scratch] -> workerChild False scratch
  ["--worker-one-fd-slot", scratch] -> workerChild True scratch
  _ -> fail "worker-response child requires role and scratch directory"
