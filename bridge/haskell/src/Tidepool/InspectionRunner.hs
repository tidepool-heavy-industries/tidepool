-- | Request-local inspection orchestration. Compilation can be shared only
-- for the same generated source and the same wildcard-normalization policy.
module Tidepool.InspectionRunner
  ( runInspectionRequests
 ) where

import Control.Exception (SomeException, throwIO, try)
import Control.Monad (foldM)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import Tidepool.ExtractRequest
  ( InspectionRequest(..), InspectionPayload, inspectionInputs, inspectionBatch, inspectionStrict )
import Tidepool.GhcPipeline (CompilePurpose(..))
import Tidepool.Introspection (InspectionResult(..))
import Tidepool.WorkerDiagnostics (sourceFailureDiagnostics, renderInspectionDiagnostics)

data SourcePolicy = PreserveSource | NormalizeWildcards deriving (Eq, Ord)

runInspectionRequests ::
  InspectionPayload ->
  (CompilePurpose -> FilePath -> IO environment) ->
  (environment -> [InspectionRequest] -> IO [InspectionResult]) ->
  IO [InspectionResult]
runInspectionRequests request compile inspect = do
  let inputs = inspectionInputs request
      queries = map snd inputs
      singletons = snd <$> runSingletons (inspectionStrict request) inputs compile inspect
  case inspectionBatch request of
    Nothing -> singletons
    Just batchPath -> do
      -- Admission pairs sources and queries and validates the batch's type-only
      -- shape. Infrastructure failures still cannot become authored rejection.
      _ <- BS.readFile batchPath
      compiled <- try (compile GeneralCompile batchPath)
      case compiled of
        Left exception
          | inspectionStrict request -> throwIO exception
          | otherwise -> case sourceFailureDiagnostics exception of
              Just _ -> singletons
              Nothing -> throwIO exception
        Right environment -> inspect environment queries

runSingletons ::
  Bool -> [(FilePath, InspectionRequest)] ->
  (CompilePurpose -> FilePath -> IO environment) ->
  (environment -> [InspectionRequest] -> IO [InspectionResult]) ->
  IO (Map.Map (SourcePolicy, FilePath) (Either SomeException environment), [InspectionResult])
runSingletons strict inputs compile inspect = do
  (environments, answers) <- foldM inspectNext (Map.empty, []) inputs
  pure (environments, concat (reverse answers))
  where
    inspectNext (environments, answers) (path, query) = do
      let policy = case query of
            InspectTypeSearch _ -> NormalizeWildcards
            InspectTypeSearchBounded _ _ -> NormalizeWildcards
            _ -> PreserveSource
          purpose = case policy of
            NormalizeWildcards -> InspectionTypeCompile
            PreserveSource -> GeneralCompile
          key = (policy, path)
      compiled <- case Map.lookup key environments of
        Just previous -> pure previous
        Nothing -> try (compile purpose path)
      result <- case compiled of
        Left exception
          | strict -> throwIO exception
          | otherwise -> case sourceFailureDiagnostics exception of
              Just diagnostics -> pure [InspectionRejected (renderInspectionDiagnostics diagnostics)]
              Nothing -> throwIO exception
        Right environment -> inspect environment [query]
      pure (Map.insert key compiled environments, result : answers)
