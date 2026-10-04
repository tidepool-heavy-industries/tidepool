module Main (main) where

import System.Environment (getArgs)
import WorkerResponseCases (workerChild)

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  ["--worker", scratch] -> workerChild False scratch
  ["--worker-one-fd-slot", scratch] -> workerChild True scratch
  _ -> fail "worker-response child requires role and scratch directory"
