-- | Request-local inspection orchestration. Compilation can be shared only
-- for the same generated source and the same wildcard-normalization policy.
module Tidepool.InspectionRunner
  ( runInspectionRequests, isInspectionTypeQuery ) where

import Control.Exception (throwIO, try)
import Control.Monad (foldM)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import Tidepool.ExtractRequest (InspectionRequest(..), WorkerRequest(..))
import Tidepool.GhcPipeline (CompilePurpose(..))
import Tidepool.Introspection (InspectionResult(..))
import Tidepool.WorkerDiagnostics (sourceFailureDiagnostics, renderInspectionDiagnostics)

data SourcePolicy = PreserveSource | NormalizeWildcards deriving (Eq, Ord)

isInspectionTypeQuery :: InspectionRequest -> Bool
isInspectionTypeQuery (InspectTypeOf _) = True
isInspectionTypeQuery _ = False

runInspectionRequests ::
  WorkerRequest ->
  (CompilePurpose -> FilePath -> IO environment) ->
  (environment -> [InspectionRequest] -> IO [InspectionResult]) ->
  IO [InspectionResult]
runInspectionRequests args compile inspect = do
  let queries = requestInspections args
  if length queries /= length (requestFiles args)
    then fail "inspection request must carry exactly one source per query"
    else pure ()
  case requestInspectTypeBatch args of
    Nothing -> runSingletons queries
    Just batchPath
      | length queries > 1 && all isInspectionTypeQuery queries -> do
          -- A missing/unreadable producer file is an infrastructure failure,
          -- not an authored-source rejection eligible for singleton fallback.
          _ <- BS.readFile batchPath
          compiled <- try (compile GeneralCompile batchPath)
          case compiled of
            Left exception
              | requestInspectionStrict args -> throwIO exception
              | otherwise -> case sourceFailureDiagnostics exception of
                  Just _ -> runSingletons queries
                  Nothing -> throwIO exception
            Right environment -> inspect environment queries
      | otherwise -> fail "inspection type batch requires at least two type queries and no other query kinds"
  where
    runSingletons queries = concat . reverse . snd <$>
      foldM inspectNext (Map.empty, []) (zip (requestFiles args) queries)
    inspectNext (environments, answers) (path, query) = do
      let policy = case query of
            InspectTypeSearch _ -> NormalizeWildcards
            _ -> PreserveSource
          purpose = case policy of
            NormalizeWildcards -> LookupTypeCompile
            PreserveSource -> GeneralCompile
          key = (policy, path)
      compiled <- case Map.lookup key environments of
        Just previous -> pure previous
        Nothing -> try (compile purpose path)
      result <- case compiled of
        Left exception
          | requestInspectionStrict args -> throwIO exception
          | otherwise -> case sourceFailureDiagnostics exception of
              Just diagnostics -> pure [InspectionRejected (renderInspectionDiagnostics diagnostics)]
              Nothing -> throwIO exception
        Right environment -> inspect environment [query]
      pure (Map.insert key compiled environments, result : answers)
