{-# LANGUAGE ScopedTypeVariables #-}

-- | Request results survive release. A completed body is independent of the
-- releases still owed by its enclosing compiler and output owners.
module Tidepool.RequestCompletion
  ( Completion, CleanupStage(..), CleanupFailure, cleanupStage, cleanupException
  , captureCompletion, flattenCompletion, withCompletionRelease, appendCleanupFailures
  , attemptReleases, asCleanupFailures, BodyOutcome(..), completionResult, completionFailures, completionAsync
  , CompletionPhase(..), PhaseSink, emitPhase, releaseWithPhase
  , finishCompletion, RequestReleaseFailure
  ) where

import Control.Exception
  ( Exception(..), SomeException(..), SomeAsyncException, mask, throwIO, try, catch
  , asyncExceptionFromException, asyncExceptionToException )
import Data.Maybe (listToMaybe, mapMaybe, isJust)
import Data.Typeable (cast)
import Data.List.NonEmpty (NonEmpty(..))
import qualified Data.List.NonEmpty as NE

data CleanupStage
  = ExecutorStop | CapabilitiesRestore | CompilerScopeRestore | OutputRestore
  | CaptureClose | CaptureRead | CaptureRemove
  deriving (Eq, Show)

-- Closed, context-free records written only to the retained diagnostic channel.
-- The sink carries observation, never request or cleanup authority.
data CompletionPhase
  = BodyStarted | BodyFinished | CleanupStarted CleanupStage
  | CleanupFinished CleanupStage | RenderingResponse | WritingResponse | ResponseWritten
  deriving Show

type PhaseSink = CompletionPhase -> IO ()

emitPhase :: PhaseSink -> CompletionPhase -> IO ()
emitPhase sink phase = sink phase `catch` \(failure :: SomeException) ->
  case fromException failure :: Maybe SomeAsyncException of
    Just _ -> throwIO failure
    Nothing -> pure ()

-- An interrupted observer cannot skip its release. Each observer and the real
-- release is attempted once; only synchronous observation failure is ignored.
releaseWithPhase :: PhaseSink -> CleanupStage -> IO () -> IO ()
releaseWithPhase sink stage release = do
  failures <- attemptReleases
    [ (stage, emitPhase sink (CleanupStarted stage))
    , (stage, release)
    , (stage, emitPhase sink (CleanupFinished stage))
    ]
  completed <- captureCompletion (pure ())
  finishCompletion (appendCleanupFailures completed failures)

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

flattenCompletion :: Completion (Completion a) -> Completion a
flattenCompletion (Completion (Right inner) failures) = appendCleanupFailures inner failures
flattenCompletion (Completion (Left failure) failures) = Completion (Left failure) failures
flattenCompletion (AbortedByCleanup failures) = AbortedByCleanup failures

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

-- Reclassify a failed owned IO extent as release evidence while retaining
-- nested stages; a successful extent contributes only its actual release facts.
asCleanupFailures :: CleanupStage -> Completion a -> [CleanupFailure]
asCleanupFailures stage (Completion result failures) =
  either (releaseFailures stage) (const []) result ++ failures
asCleanupFailures _ (AbortedByCleanup failures) = NE.toList failures

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
instance Exception RequestReleaseFailure where
  toException failure@(RequestReleaseFailure primary failures)
    | isJust (completionAsync (Completion (maybe (Right ()) Left primary) (NE.toList failures))) =
        asyncExceptionToException failure
    | otherwise = SomeException failure
  fromException failure@(SomeException actual) = case cast actual of
    Just known -> Just known
    Nothing -> asyncExceptionFromException failure

finishCompletion :: Completion a -> IO a
finishCompletion (Completion result []) = either throwIO pure result
finishCompletion (Completion result (first : rest)) =
  throwIO (RequestReleaseFailure (either Just (const Nothing) result) (first :| rest))
finishCompletion (AbortedByCleanup failures) =
  throwIO (RequestReleaseFailure Nothing failures)
