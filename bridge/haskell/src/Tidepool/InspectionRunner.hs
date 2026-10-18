-- | Request-local inspection orchestration. Compilation can be shared only
-- for the same generated source and the same wildcard-normalization policy.
module Tidepool.InspectionRunner
  ( runInspectionRequests, runHostedLookupRequests, HostedMetadataEnvironment(..)
  , isInspectionTypeQuery ) where

import Control.Exception (SomeException, throwIO, try)
import Control.Monad (foldM)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import Tidepool.ExtractRequest
  ( InspectionRequest(..), WorkerRequest(..), HostedLookupBatch(..)
  , HostedLookupOperation(..), HostedLookupQuery(..), validateRequestShape )
import Tidepool.GhcPipeline (CompilePurpose(..))
import Tidepool.Introspection (InspectionResult(..), HostedLookupAnswer, selectHostedAnswers)
import Tidepool.WorkerDiagnostics (sourceFailureDiagnostics, renderInspectionDiagnostics)

data SourcePolicy = PreserveSource | NormalizeWildcards deriving (Eq, Ord)

-- The successful environment is the exact admitted metadata cohort, not any
-- other successful probe. A failed cohort never acquires fallback authority.
data HostedMetadataEnvironment environment
  = NoHostedMetadata
  | HostedMetadataRejected String
  | HostedMetadataReady environment

isInspectionTypeQuery :: InspectionRequest -> Bool
isInspectionTypeQuery (InspectTypeOf _) = True
isInspectionTypeQuery _ = False

runInspectionRequests ::
  WorkerRequest ->
  (CompilePurpose -> FilePath -> IO environment) ->
  (environment -> [InspectionRequest] -> IO [InspectionResult]) ->
  IO [InspectionResult]
runInspectionRequests args compile inspect = do
  case requestHostedLookup args of
    Just _ -> fail "complete inspection cannot consume a hosted lookup batch"
    Nothing -> pure ()
  let queries = requestInspections args
  requireSources (requestFiles args) queries
  let singletons = snd <$> runSingletons (requestInspectionStrict args) (requestFiles args) queries compile inspect
  case requestInspectTypeBatch args of
    Nothing -> singletons
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
                  Just _ -> singletons
                  Nothing -> throwIO exception
            Right environment -> inspect environment queries
      | otherwise -> fail "inspection type batch requires at least two type queries and no other query kinds"

runHostedLookupRequests ::
  WorkerRequest ->
  (CompilePurpose -> FilePath -> IO environment) ->
  (environment -> [InspectionRequest] -> IO [InspectionResult]) ->
  (HostedMetadataEnvironment environment -> [HostedLookupAnswer] -> IO output) ->
  IO output
runHostedLookupRequests args compile inspect finish = do
  either (fail . show) pure (validateRequestShape args)
  batch <- maybe (fail "hosted lookup request is missing its batch") pure (requestHostedLookup args)
  let operations = hostedLookupOperations batch
      queries = map operationQuery operations
  (environments, results) <- runSingletons (requestInspectionStrict args) (requestFiles args) queries compile inspect
  selected <- either fail pure (selectHostedAnswers batch results)
  metadata <- case [path | (path, operation) <- zip (requestFiles args) operations, metadataOperation operation] of
    [] -> pure NoHostedMetadata
    path : _ -> case Map.lookup (PreserveSource, path) environments of
      Nothing -> fail "hosted lookup lost its admitted metadata cohort"
      Just (Right environment) -> pure (HostedMetadataReady environment)
      Just (Left exception) -> case sourceFailureDiagnostics exception of
        Just diagnostics -> pure (HostedMetadataRejected (renderInspectionDiagnostics diagnostics))
        Nothing -> throwIO exception
  finish metadata selected
  where
    operationQuery (HostedLookupPrimary _ _ (HostedLookupName query)) = InspectLookupNameOf query
    operationQuery (HostedLookupPrimary _ _ (HostedLookupTypeSearch query)) = InspectTypeSearch query
    operationQuery (HostedLookupExactReference _ query) = InspectExactNameOf query
    metadataOperation (HostedLookupPrimary _ _ (HostedLookupTypeSearch _)) = False
    metadataOperation _ = True

requireSources :: [FilePath] -> [InspectionRequest] -> IO ()
requireSources paths queries
  | length paths == length queries = pure ()
  | otherwise = fail "inspection request must carry exactly one source per query"

runSingletons ::
  Bool -> [FilePath] -> [InspectionRequest] ->
  (CompilePurpose -> FilePath -> IO environment) ->
  (environment -> [InspectionRequest] -> IO [InspectionResult]) ->
  IO (Map.Map (SourcePolicy, FilePath) (Either SomeException environment), [InspectionResult])
runSingletons strict paths queries compile inspect = do
  requireSources paths queries
  (environments, answers) <- foldM inspectNext (Map.empty, []) (zip paths queries)
  pure (environments, concat (reverse answers))
  where
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
          | strict -> throwIO exception
          | otherwise -> case sourceFailureDiagnostics exception of
              Just diagnostics -> pure [InspectionRejected (renderInspectionDiagnostics diagnostics)]
              Nothing -> throwIO exception
        Right environment -> inspect environment [query]
      pure (Map.insert key compiled environments, result : answers)
