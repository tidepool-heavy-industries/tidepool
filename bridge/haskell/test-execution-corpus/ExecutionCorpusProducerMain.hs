module Main (main) where

import Control.Monad (unless)
import Data.Set qualified as Set
import System.Environment (getArgs)
import ExecutionCorpusCases (runProbe, splitRequests)
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), CompilePurpose(GeneralCompile)
  , runPipelineSelected, withResidentPipelineSelectedRequests )

main :: IO ()
main = getArgs >>= \arguments -> case arguments of
  "--batch" : requests -> do
    let selected = splitRequests requests
    unless (not (null selected) && all (not . null) selected)
      (fail "corpus producer requires at least one complete request")
    withResidentPipelineSelectedRequests [] $ \runRequest ->
      mapM_ (\request -> runRequest (pure ()) $ \compiler ->
        runProbe (\source includes ->
          compiler PreparedStg Set.empty GeneralCompile Nothing source includes Nothing)
          request) selected
  [] -> fail "corpus producer requires source, targets and output arguments"
  _ -> runProbe (runPipelineSelected PreparedStg) arguments
