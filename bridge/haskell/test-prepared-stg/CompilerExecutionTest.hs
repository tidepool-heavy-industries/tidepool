{-# LANGUAGE ScopedTypeVariables #-}

module CompilerExecutionTest (compilerExecutionTests) where

import Control.Concurrent (forkFinally, killThread)
import Control.Concurrent.MVar
  (MVar, newEmptyMVar, putMVar, readMVar)
import Control.Exception (SomeException, bracket, finally, throwIO)
import Control.Monad (forM_, unless)
import Data.IORef (atomicModifyIORef', newIORef, readIORef)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import System.Timeout (timeout)
import Tidepool.CompilerExecution
import Tidepool.Test.Runner (TestTree, testCase, testGroup)

compilerExecutionTests :: TestTree
compilerExecutionTests = testGroup "compiler execution"
  [ testCase "independent tasks overlap and publish completion before ordered return" overlap
  , testCase "completion-triggered work shares the admitted bound" completionWork
  , testCase "dynamic demand incorporates ready ancestors before blocked descendants" dynamicDemand
  , testCase "cancellation joins running work and fences submission" cancellation
  , testCase "failed work cancels and joins its siblings" failedWork
  , testCase "reuse agrees with independent graph closure" reuseOracle
  ]

assert :: Bool -> String -> IO ()
assert condition message = unless condition (fail message)

within :: IO a -> IO a
within action = timeout 5000000 action >>= maybe (fail "compiler executor test timed out") pure

jobs :: Int -> CompilerExecutionGrant
jobs = either error id . compilerExecutionGrant

start :: IO a -> IO (IO a, IO ())
start action = do
  done <- newEmptyMVar
  thread <- forkFinally action (putMVar done)
  pure (within (readMVar done) >>= either throwIO pure, killThread thread)

observe :: MVar a -> IO a
observe = within . readMVar

overlap :: IO ()
overlap = within $ do
  entered <- mapM (const newEmptyMVar) [0 :: Int, 1]
  release <- mapM (const newEmptyMVar) entered
  completion <- newEmptyMVar
  callbacks <- newIORef []
  bracket (start $ withCompilerExecutor (jobs 2) $ \executor ->
    runCompilerTasks executor
      (\ordinal -> putMVar (entered !! ordinal) () >> readMVar (release !! ordinal) >> pure ordinal)
      (\ordinal _ -> do
        atomicModifyIORef' callbacks (\known -> (known ++ [ordinal], ()))
        if ordinal == 1 then putMVar completion () else pure ()) [0, 1]) snd $ \(result, _) -> do
      mapM_ observe entered
      putMVar (release !! 1) ()
      observe completion
      seen <- readIORef callbacks
      assert (seen == [1]) "completion waited for the earlier input"
      putMVar (release !! 0) ()
      ordered <- result
      assert (ordered == [0, 1]) "results followed finish order instead of input order"

completionWork :: IO ()
completionWork = within $ do
  active <- newIORef (0 :: Int, 0 :: Int)
  let task value = bracket
        (atomicModifyIORef' active (\(current, peak) -> let next = current + 1 in ((next, max peak next), ())))
        (const (atomicModifyIORef' active (\(current, peak) -> ((current - 1, peak), ()))))
        (const (pure value))
  withCompilerExecutor (jobs 2) $ \executor -> do
    result <- runCompilerTasks executor task
      (\(value :: Int) _ -> do
        children <- runCompilerTasks executor task (\_ _ -> pure ()) [value + 10, value + 20]
        assert (children == [value + 10, value + 20]) "completion work lost deterministic order") [0, 1]
    assert (result == [0, 1]) "completion work changed the outer result"
  (remaining, peak) <- readIORef active
  assert (remaining == 0 && peak <= 2) "completion work escaped the shared execution grant"

-- The second initial job completes before the child of the first can finish.
-- A nested collector would wait for the child and strand that incorporation.
dynamicDemand :: IO ()
dynamicDemand = within $ do
  ancestor <- newEmptyMVar
  releaseChild <- newEmptyMVar
  incorporated <- newEmptyMVar
  bracket (start $ withCompilerExecutor (jobs 2) $ \executor ->
    runCompilerWorklist executor
      (\value -> case value of
        0 -> pure value
        1 -> observe ancestor >> pure value
        _ -> putMVar ancestor () >> observe releaseChild >> pure value)
      (\value _ -> case value of
        0 -> pure [2]
        1 -> putMVar incorporated () >> pure []
        _ -> pure []) [0 :: Int,1]) snd $ \(result,_) -> do
      observe incorporated
      putMVar releaseChild ()
      values <- result
      assert (values == [0,1,2]) "dynamic completion lost submission order"

cancellation :: IO ()
cancellation = within $ do
  entered <- newEmptyMVar
  finalized <- newEmptyMVar
  never <- newEmptyMVar
  retained <- newEmptyMVar
  done <- newEmptyMVar
  thread <- forkFinally
    (withCompilerExecutor (jobs 1) $ \executor -> do
      putMVar retained executor
      runCompilerTasks executor
        (\() -> (putMVar entered () >> readMVar never) `finally` putMVar finalized ())
        (\_ _ -> pure ()) [(), ()])
    (putMVar done)
  observe entered
  killThread thread
  observe finalized
  outcome <- observe done
  assert (case outcome of Left (_ :: SomeException) -> True; Right _ -> False)
    "cancelled coordinator reported success"
  executor <- observe retained
  stopped <- newEmptyMVar
  _ <- forkFinally (runCompilerTasks executor pure (\_ _ -> pure ()) [()]) (putMVar stopped)
  refusal <- observe stopped
  assert (case refusal of Left (_ :: SomeException) -> True; Right _ -> False)
    "closed executor accepted more work"

failedWork :: IO ()
failedWork = within $ do
  running <- newEmptyMVar
  finalized <- newEmptyMVar
  never <- newEmptyMVar
  finished <- newEmptyMVar
  _ <- forkFinally
    (withCompilerExecutor (jobs 2) $ \executor -> runCompilerTasks executor
      (\ordinal -> if ordinal == (0 :: Int)
        then readMVar running >> fail "deliberate compiler task failure"
        else (putMVar running () >> readMVar never) `finally` putMVar finalized ())
      (\_ _ -> pure ()) [0, 1]) (putMVar finished)
  outcome <- observe finished
  observe finalized
  assert (case outcome of Left (_ :: SomeException) -> True; Right _ -> False)
    "failed compiler task reported success"

-- Enumerate every three-owner graph and validity subset. The oracle walks
-- each root independently, instead of reusing the implementation's pruning.
reuseOracle :: IO ()
reuseOracle = do
  let owners = [0 :: Int, 1, 2]
      subsets [] = [[]]
      subsets (value : rest) = let tails = subsets rest in tails ++ map (value :) tails
      graphs = [Map.fromList (zip owners (map Set.fromList dependencies))
               | dependencies <- sequence (replicate 3 (subsets owners))]
      reachable graph root = walk Set.empty [root]
        where
          walk seen [] = seen
          walk seen (owner : rest)
            | owner `Set.member` seen = walk seen rest
            | otherwise = walk (Set.insert owner seen)
                (Set.toList (Map.findWithDefault Set.empty owner graph) ++ rest)
      expected graph local = Set.filter
        (\owner -> reachable graph owner `Set.isSubsetOf` local) local
  forM_ graphs $ \graph -> forM_ (subsets owners) $ \valid -> do
    let local = Set.fromList valid
    assert (dependencyClosedReuse graph local == expected graph local)
      ("reuse closure disagreed with reachability oracle: " ++ show (graph, valid))
  assert (Set.null (dependencyClosedReuse (Map.singleton (0 :: Int) (Set.singleton 3)) (Set.singleton 0)))
    "missing dependency remained reusable"
