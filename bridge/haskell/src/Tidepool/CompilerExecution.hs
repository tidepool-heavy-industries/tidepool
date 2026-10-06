{-# LANGUAGE ScopedTypeVariables #-}

-- | One admitted allowance for compiler work. GHC owns source scheduling;
-- independent lowering, projection and recovery share this executor.
module Tidepool.CompilerExecution
  ( CompilerExecutionGrant, compilerExecutionGrant, serialCompilerExecutionGrant
  , compilerModuleJobs
  , CompilerExecutor, withCompilerExecutor, runCompilerTasks
  ) where

import Control.Concurrent (forkFinally, killThread)
import Control.Concurrent.Chan (Chan, newChan, readChan, writeChan)
import Control.Concurrent.MVar
  (MVar, newMVar, newEmptyMVar, modifyMVar, modifyMVar_, putMVar, readMVar, tryPutMVar)
import Control.Exception (SomeException, AsyncException(ThreadKilled), bracket, mask, throwIO, toException, try)
import Control.Monad (forM_, replicateM, when)
import qualified Data.Map.Strict as Map

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
  bracket (replicateM (compilerModuleJobs grant) start) stop
    (const (restore (use executor)))

-- | Run task bodies on the shared allowance. Completion callbacks run on the
-- coordinator as soon as a result arrives and may enqueue further work on
-- this same executor. Returned values retain input order. Task bodies consume
-- already acquired inputs; live Session acquisition stays with its owner.
runCompilerTasks :: CompilerExecutor -> (input -> IO output)
  -> (input -> output -> IO ()) -> [input] -> IO [output]
runCompilerTasks (CompilerExecutor queue state) action completed inputs = mask $ \restore -> do
  events <- newChan
  forM_ (zip [0 :: Int ..] inputs) $ \(ordinal, input) -> do
    result <- newEmptyMVar
    let settle outcome = do
          first <- tryPutMVar result outcome
          when first (writeChan events (ordinal, input, outcome))
    key <- modifyMVar state $ \(ExecutorState closed next pending) ->
      if closed then throwIO ThreadKilled else pure
        (ExecutorState False (next + 1)
          (Map.insert next (settle . Left) pending), next)
    writeChan queue $ mask $ \run -> do
      outcome <- try (run (action input))
      modifyMVar_ state $ \(ExecutorState closed next pending) ->
        pure (ExecutorState closed next (Map.delete key pending))
      settle outcome
  let collect 0 outputs = pure (Map.elems outputs)
      collect remaining outputs = do
        (ordinal, input, outcome) <- restore (readChan events)
        output <- either throwIO pure outcome
        restore (completed input output)
        collect (remaining - 1) (Map.insert ordinal output outputs)
  collect (length inputs) Map.empty
