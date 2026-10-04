module Main (main) where

import System.Environment (getArgs)
import ExecutionCorpusCases (runProbe)
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), runPipelineSelected )

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  [] -> fail "corpus producer requires source, targets and output arguments"
  _ -> runProbe (runPipelineSelected PreparedStg) arguments
