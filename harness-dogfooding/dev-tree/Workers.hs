{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | The shared execution seam for dogfood workers and orchestrator checks.
--
-- This module owns the boundary between typed agent execution and the
-- harness's mechanical observations: workers are spawned here, their output
-- is snapshot-committed here, checks and boundary diffs are run here, and
-- small rendering helpers keep those paths byte-for-byte consistent.  It is
-- deliberately independent of "Harness" so the fold and microtask modules
-- can use the same seam without introducing a cycle.
module Workers
  ( SnapshotResult (..)
  , runWorker
  , snapshotWork
  , gitIn
  , runCheckCmd
  , runChecks
  , checkFailed
  , boundaryViolations
  , snapshotFailureFor
  , snapshotFailureMaybe
  , branchOf
  , firstLine
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Agent.Spawn
  ( renderSpawnError
  , spawnAgent
  )
import Tidepool.Effects
  ( SpawnError
  , WorktreeHandle (..)
  , runIn
  , say
  , spawnSpecIn
  )
import Tidepool.Event
  ( HeadChangeReceipt (..)
  , Observed (..)
  , headChanged
  , withHandlerTry
  )
import Tidepool.Harness (Harness)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Shell (renderExecError, runInTry)
import Tidepool.Worktree

-- | The harness-owned snapshot account for one worker cycle.  A clean tree
-- is a successful no-op; every other path records whether the snapshot was
-- needed and which git step, if any, failed so callers cannot mistake a
-- dirty uncommitted worktree for a completed worker.
data SnapshotResult = SnapshotResult
  { snapshotNeeded    :: Bool
  , snapshotSucceeded :: Bool
  , snapshotFailure   :: Maybe Text
  }

-- | One typed worker cycle, followed by the harness-owned snapshot.
--
-- The typed spawn fixes both the worker's result schema and its terminal
-- payload.  HEAD observation is advisory and degrades to the ordinary spawn
-- when the observation handler itself fails; snapshotting remains mandatory
-- before this function returns.
runWorker :: WorktreeHandle -> Text -> Text -> Harness (Either SpawnError (WorkerResult, SnapshotResult))
runWorker tree name prompt = do
  result <-
    withHandlerTry
      (headChanged tree)
      (\case
        Right change -> noteHeadMove name change
        Left err -> say [fmt|{name} HEAD-move observation failed: {show err}|])
      (spawnAgent @WorkerResult (spawnSpecIn (worktreeId tree) name prompt) <&> fmap snd)
      >>= \case
        Right workerResult -> pure workerResult
        Left err -> do
          say [fmt|{name} HEAD-move observation degraded: {show err}|]
          spawnAgent @WorkerResult (spawnSpecIn (worktreeId tree) name prompt) <&> fmap snd
  case result of
    Right wr -> do
      snapshot <- snapshotWork tree name
      let wr' = case snapshot.snapshotFailure of
            Nothing -> wr
            Just detail ->
              let line = [fmt|{name}: SNAPSHOT FAILED — {detail}|]
               in wr
                    { evidence = wr.evidence <> [line]
                    , readyForIntegration = False
                    , obstacles = wr.obstacles <> [line]
                    }
      traverse_ (say . (\f -> [fmt|{name}: {f}|])) (maybeToList snapshot.snapshotFailure)
      -- Surface self-reported friction immediately (note feed + log); it
      -- also rides the receipt via 'finishFold'.  Advisory only.
      traverse_ (\f -> say [fmt|{name} friction: {f}|]) wr'.frictionNotes
      pure (Right (wr', snapshot))
    Left err -> pure (Left err)

-- | The harness's own commit of whatever a worker left uncommitted.
--
-- Codex workers cannot commit in their sandbox.  A clean tree is a no-op;
-- otherwise the harness stages and commits the worker's output, then reads
-- HEAD again so every caller's next observation sees the real commit.
snapshotWork :: WorktreeHandle -> Text -> Harness SnapshotResult
snapshotWork tree name =
  gitIn tree "status --porcelain" >>= \case
    Left err -> pure (SnapshotResult True False (Just [fmt|status --porcelain failed: {err}|]))
    Right pr
      | T.null (T.strip pr.stdout) -> pure (SnapshotResult False True Nothing)
      | otherwise -> do
          gitIn tree "add -A" >>= \case
            Left err -> pure (SnapshotResult True False (Just [fmt|add -A failed: {err}|]))
            Right _ ->
              gitIn tree [fmt|commit -m "{name}: agent work"|] >>= \case
                Left err -> pure (SnapshotResult True False (Just [fmt|commit failed: {err}|]))
                Right _ -> do
                  sha <- worktreeHead tree
                  say [fmt|{name}: harness snapshot-committed uncommitted worker output at {renderGitOid sha}|]
                  pure (SnapshotResult True True Nothing)

snapshotFailureFor :: Text -> SnapshotResult -> Failure
snapshotFailureFor name snapshot =
  Failure
    { failureKind = SnapshotFailed {snapshotName = name}
    , failureDetail = maybe [fmt|{name}: snapshot did not complete|] (\detail -> [fmt|{name}: {detail}|]) snapshot.snapshotFailure
    , failurePaths = []
    }

snapshotFailureMaybe :: Text -> SnapshotResult -> Maybe Failure
snapshotFailureMaybe name snapshot
  | snapshot.snapshotSucceeded = Nothing
  | otherwise = Just (snapshotFailureFor name snapshot)

-- | Run all plan-authored checks in order, using the same command seam as
-- planner-authored microtask checks.
runChecks :: WorktreeHandle -> DevPlan -> Harness [CheckResult]
runChecks tree p = traverse (runCheckCmd tree) (nodeChecks p)

-- | One orchestrator-run check command.  A command that cannot run is
-- reported as exit 127 with the typed execution error, never as a pass.
runCheckCmd :: WorktreeHandle -> Text -> Harness CheckResult
runCheckCmd tree cmd =
  runIn tree.handleReceipt.cwd cmd >>= \case
    Left e -> pure (CheckResult cmd 127 (renderExecError e))
    Right pr -> pure (CheckResult cmd pr.exitCode (firstLine pr.stderr))

checkFailed :: CheckResult -> Bool
checkFailed c = c.checkExit /= 0

-- | Return changed paths outside the declared boundary and paths in the
-- tolerated hygiene tier separately.  An empty boundary is unrestricted.
boundaryViolations :: WorktreeHandle -> [Text] -> [Text] -> Harness ([Text], [Text])
boundaryViolations _ [] _ = pure ([], [])
boundaryViolations tree prefixes tolerated =
  gitIn tree [fmt|diff --name-only {seedHead}..HEAD|] >>= \case
    -- A boundary check that could not RUN is a failing boundary check, never
    -- a clean one: reporting [] here would silently convert "git is broken in
    -- this worktree" into "this node stayed inside its boundary".
    Left e -> pure ([[fmt|<boundary check could not run: {e}>|]], [])
    Right pr -> pure (classify (filter (not . T.null) (T.lines pr.stdout)))
  where
    seedHead = renderGitOid tree.handleReceipt.sourceHead
    inside f prefixes' = any (\pre -> f == pre || (pre <> "/") `T.isPrefixOf` f) prefixes'
    classify = foldr classifyPath ([], [])
    classifyPath f (outside, toleratedPaths)
      | inside f prefixes = (outside, toleratedPaths)
      | inside f tolerated = (outside, f : toleratedPaths)
      | otherwise = (f : outside, toleratedPaths)

gitIn :: WorktreeHandle -> Text -> Harness (Either Text Proc)
gitIn tree args = runInTry tree.handleReceipt.cwd ("git " <> args)

branchOf :: WorktreeHandle -> Text
branchOf tree = renderBranchName tree.handleReceipt.branch

firstLine :: Text -> Text
firstLine t = case T.lines t of
  [] -> ""
  (l : _) -> l

noteHeadMove :: Text -> Observed HeadChangeReceipt -> Harness ()
noteHeadMove name change = say (name <> " HEAD -> " <> renderGitOid receipt.newHead)
  where
    receipt = value change
