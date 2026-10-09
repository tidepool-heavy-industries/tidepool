{-# LANGUAGE ScopedTypeVariables #-}

-- | One admitted allowance for compiler work. GHC owns source scheduling;
-- independent lowering, projection and recovery share this executor.
module Tidepool.CompilerExecution
  ( CompilerExecutionGrant, compilerExecutionGrant, serialCompilerExecutionGrant
  , compilerModuleJobs
  , CompilerExecutor, withCompilerExecutor, runCompilerTasks, runCompilerWorklist, runCompilerWorklistWithStarted, dependencyClosedReuse
  ) where

import Control.Concurrent (forkFinally, killThread)
import Control.Concurrent.Chan (Chan, newChan, readChan, writeChan)
import Control.Concurrent.MVar
  (MVar, newMVar, newEmptyMVar, modifyMVar, modifyMVar_, putMVar, readMVar, tryPutMVar)
import Control.Exception
  (SomeException, SomeAsyncException, AsyncException(ThreadKilled), bracket, mask
  , fromException, onException, throwIO, toException, try)
import Control.Monad (forM_, when)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set

-- | Positive, explicitly admitted module jobs. Ordinary callers stay serial
-- until the enclosing owner supplies a qualified allocation.
newtype CompilerExecutionGrant = CompilerExecutionGrant Int
  deriving (Eq, Show)

compilerExecutionGrant :: Int -> Either String CompilerExecutionGrant
compilerExecutionGrant jobs
  | jobs > 0 = Right (CompilerExecutionGrant jobs)
  | otherwise = Left "compiler module jobs must be positive"

serialCompilerExecutionGrant :: CompilerExecutionGrant
serialCompilerExecutionGrant = CompilerExecutionGrant 1

compilerModuleJobs :: CompilerExecutionGrant -> Int
compilerModuleJobs (CompilerExecutionGrant jobs) = jobs

-- Pending settlements are removed before publication. Closing the executor
-- settles queued work too, so a task cancelled before a worker starts it cannot
-- leave a waiter behind. The closed flag fences later submissions.
data ExecutorState = ExecutorState Bool Int (Map.Map Int (SomeException -> IO ()))
data CompilerExecutor = CompilerExecutor (Chan (IO ())) (MVar ExecutorState)

withCompilerExecutor :: CompilerExecutionGrant -> (CompilerExecutor -> IO a) -> IO a
withCompilerExecutor grant use = mask $ \restore -> do
  queue <- newChan
  state <- newMVar (ExecutorState False 0 Map.empty)
  let executor = CompilerExecutor queue state
      worker = readChan queue >>= id >> worker
      start = do
        done <- newEmptyMVar
        thread <- forkFinally (restore worker) (const (putMVar done ()))
        pure (thread, done)
      stop workers = do
        pending <- modifyMVar state $ \(ExecutorState _ next known) ->
          pure (ExecutorState True next Map.empty, Map.elems known)
        forM_ pending ($ toException ThreadKilled)
        forM_ workers (killThread . fst)
        forM_ workers (readMVar . snd)
      spawn 0 known = pure known
      spawn remaining known = do
        worker' <- start `onException` stop known
        spawn (remaining - 1) (worker' : known)
  bracket (spawn (compilerModuleJobs grant) []) stop
    (const (restore (use executor)))

-- | Run task bodies on the shared allowance. Completion callbacks run on the
-- coordinator as soon as a result arrives and may enqueue further work on
-- this same executor. Returned values retain input order. Task bodies consume
-- already acquired inputs; live Session acquisition stays with its owner.
runCompilerTasks :: CompilerExecutor -> (input -> IO output)
  -> (input -> output -> IO ()) -> [input] -> IO [output]
runCompilerTasks executor action completed inputs =
  runCompilerWorklist executor action (\input output -> completed input output >> pure []) inputs

-- Dynamic demand is submitted to this same executor and consumed by one
-- coordinator. Completed ancestor tasks never wait behind a nested collector.
runCompilerWorklist :: CompilerExecutor -> (input -> IO output)
  -> (input -> output -> IO [input]) -> [input] -> IO [output]
runCompilerWorklist executor action completed inputs =
  runCompilerWorklistWithStarted executor action (const (pure ())) completed inputs

runCompilerWorklistWithStarted :: CompilerExecutor -> (input -> IO output)
  -> (input -> IO ()) -> (input -> output -> IO [input]) -> [input] -> IO [output]
runCompilerWorklistWithStarted (CompilerExecutor queue state) action started completed inputs = mask $ \restore -> do
  events <- newChan
  let submit first tasks = forM_ (zip [first..] tasks) $ \(ordinal, input) -> do
        result <- newEmptyMVar
        let settle outcome = do
              first <- tryPutMVar result outcome
              when first (writeChan events (Right (ordinal, input, outcome)))
        key <- modifyMVar state $ \(ExecutorState closed next pending) ->
          if closed then throwIO ThreadKilled else pure
            (ExecutorState False (next + 1)
              (Map.insert next (settle . Left) pending), next)
        writeChan queue $ mask $ \run -> do
          writeChan events (Left input)
          outcome <- try (run (action input))
          modifyMVar_ state $ \(ExecutorState closed next pending) ->
            pure (ExecutorState closed next (Map.delete key pending))
          settle outcome
          -- A shutdown signal must leave the worker loop after settling its task.
          -- Consuming it here would strand teardown waiting for an idle worker.
          case outcome of
            Left failure | Just async <- (fromException failure :: Maybe SomeAsyncException) -> throwIO async
            _ -> pure ()
      collect 0 _ outputs = pure (Map.elems outputs)
      collect remaining next outputs = do
        event <- restore (readChan events)
        case event of
          Left input -> restore (started input) >> collect remaining next outputs
          Right (ordinal,input,outcome) -> do
            output <- either throwIO pure outcome
            additions <- restore (completed input output)
            submit next additions
            collect (remaining - 1 + length additions) (next + length additions)
              (Map.insert ordinal output outputs)
  submit 0 inputs
  collect (length inputs) (length inputs) Map.empty

-- | Greatest dependency-closed subset of independently valid owners. Native
-- cycles may survive together; an invalid or absent dependency removes every
-- dependent, independent of the order in which jobs finish.
dependencyClosedReuse :: Ord owner => Map.Map owner (Set.Set owner)
  -> Set.Set owner -> Set.Set owner
dependencyClosedReuse graph = close
  where
    close owners =
      let next = Set.filter (\owner -> case Map.lookup owner graph of
            Nothing -> False
            Just dependencies -> Set.delete owner dependencies `Set.isSubsetOf` owners) owners
      in if next == owners then owners else close next
