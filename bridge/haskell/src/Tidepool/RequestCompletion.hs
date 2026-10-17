{-# LANGUAGE ScopedTypeVariables #-}

-- | Request results survive release. A completed body is independent of the
-- releases still owed by its enclosing compiler and output owners.
module Tidepool.RequestCompletion
  ( Completion, CleanupStage(..), CleanupFailure, cleanupStage, cleanupException
  , captureCompletion, withCompletionRelease, appendCleanupFailures
  , attemptReleases, BodyOutcome(..), completionResult, completionFailures, completionAsync
  , finishCompletion, RequestReleaseFailure
  ) where

import Control.Exception
  ( Exception, SomeException, SomeAsyncException, fromException, mask, throwIO, try )
import Data.Maybe (listToMaybe, mapMaybe)
import Data.List.NonEmpty (NonEmpty(..))
import qualified Data.List.NonEmpty as NE

data CleanupStage
  = ExecutorStop | CapabilitiesRestore | OutputRestore
  | CaptureClose | CaptureRead | CaptureRemove
  deriving (Eq, Show)

data CleanupFailure = CleanupFailure CleanupStage SomeException
  deriving Show

cleanupStage :: CleanupFailure -> CleanupStage
cleanupStage (CleanupFailure stage _) = stage

cleanupException :: CleanupFailure -> SomeException
cleanupException (CleanupFailure _ failure) = failure

data BodyOutcome a = BodySucceeded a | BodyFailed SomeException | BodyAbortedByCleanup

data Completion a
  = Completion (Either SomeException a) [CleanupFailure]
  | AbortedByCleanup (NonEmpty CleanupFailure)

completionResult :: Completion a -> BodyOutcome a
completionResult (Completion (Right value) _) = BodySucceeded value
completionResult (Completion (Left failure) _) = BodyFailed failure
completionResult (AbortedByCleanup _) = BodyAbortedByCleanup

completionFailures :: Completion a -> [CleanupFailure]
completionFailures (Completion _ failures) = failures
completionFailures (AbortedByCleanup failures) = NE.toList failures

captureCompletion :: IO a -> IO (Completion a)
captureCompletion action = do
  result <- try action
  pure (fromResult result)

-- Exception-based compiler helpers can finish an inner owner before returning
-- to an enclosing request. Keep its typed release assessment when the outer
-- owner captures that exception; successful outer release cannot clear it.
fromResult :: Either SomeException a -> Completion a
fromResult (Right value) = Completion (Right value) []
fromResult (Left failure) = case fromException failure of
  Just (RequestReleaseFailure primary failures) ->
    case primary of
      Nothing -> AbortedByCleanup failures
      Just original -> appendCleanupFailures (fromResult (Left original)) (NE.toList failures)
  Nothing -> Completion (Left failure) []

-- A release can itself own nested releases. Its independent primary belongs
-- to this release stage; the nested owner's failures keep their own stages.
releaseFailures :: CleanupStage -> SomeException -> [CleanupFailure]
releaseFailures stage failure = case fromException failure of
  Just (RequestReleaseFailure primary failures) ->
    maybe [] (releaseFailures stage) primary ++ normalizeFailures (NE.toList failures)
  Nothing -> [CleanupFailure stage failure]

normalizeFailures :: [CleanupFailure] -> [CleanupFailure]
normalizeFailures = concatMap (\(CleanupFailure stage failure) -> releaseFailures stage failure)

appendCleanupFailures :: Completion a -> [CleanupFailure] -> Completion a
appendCleanupFailures (Completion result known) failures =
  Completion result (known ++ normalizeFailures failures)
appendCleanupFailures (AbortedByCleanup known) failures =
  AbortedByCleanup (foldl (\items item -> items <> (item :| [])) known (normalizeFailures failures))

-- The owner attempts every acquired release, including after an asynchronous
-- exception. Masked interruptible releases remain cancellable; no join becomes
-- uninterruptible. The original exception is retained until the outer owner
-- has attempted its releases too.
attemptReleases :: [(CleanupStage, IO ())] -> IO [CleanupFailure]
attemptReleases operations = mask $ \_ -> go operations
  where
    go [] = pure []
    go ((stage, operation) : rest) = do
      result <- try operation
      remaining <- go rest
      pure $ case result of
        Left failure -> releaseFailures stage failure ++ remaining
        Right () -> remaining

withCompletionRelease
  :: CleanupStage -> IO resource -> (resource -> IO ())
  -> (resource -> IO (Completion a)) -> IO (Completion a)
withCompletionRelease stage acquire release use = mask $ \restore -> do
  acquired <- try acquire
  case acquired of
    Left failure -> pure (fromResult (Left failure))
    Right resource -> do
      nested <- try (restore (use resource))
      let completed = case nested of
            Left failure -> fromResult (Left failure)
            Right value -> value
      failures <- attemptReleases [(stage, release resource)]
      pure (appendCleanupFailures completed failures)

-- Cancellation keeps its actual exception identity, whether it interrupted
-- the body or an interruptible release. It is never a source diagnostic.
completionAsync :: Completion a -> Maybe SomeException
completionAsync completed =
  listToMaybe (mapMaybe asynchronous (primary ++ map cleanupException failures))
  where
    result = completionResult completed
    failures = completionFailures completed
    primary = case result of
      BodyFailed failure -> [failure]
      _ -> []
    asynchronous failure = case fromException failure :: Maybe SomeAsyncException of
      Just _ -> Just failure
      Nothing -> Nothing

-- Non-reporting owners still receive both the primary exception and all
-- release failures. Reporting owners retain Completion until terminal output.
data RequestReleaseFailure = RequestReleaseFailure (Maybe SomeException) (NonEmpty CleanupFailure)
  deriving Show
instance Exception RequestReleaseFailure

finishCompletion :: Completion a -> IO a
finishCompletion (Completion result []) = either throwIO pure result
finishCompletion (Completion result (first : rest)) =
  throwIO (RequestReleaseFailure (either Just (const Nothing) result) (first :| rest))
finishCompletion (AbortedByCleanup failures) =
  throwIO (RequestReleaseFailure Nothing failures)
