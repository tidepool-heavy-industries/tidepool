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
-- is snapshot-committed here, checks and boundary diffs are run here.  All
-- git goes through the typed "Git" seam; the only shell-string execution
-- left is 'runCheckCmd', which runs planner-authored check COMMANDS —
-- deliberately shell, deliberately at one seam.  It is independent of
-- "Harness" so the fold and microtask modules can use the same seam without
-- introducing a cycle.
module Workers
  ( SnapshotResult (..)
  , runWorker
  , snapshotWork
  , runCheckCmd
  , runChecks
  , boundaryViolations
  , snapshotFailureFor
  , snapshotFailureMaybe
  , branchOf
  ) where

import qualified Data.Text as T
import Git
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
import Tidepool.Shell (renderExecError)
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
--
-- The snapshot verdict stays TYPED ('SnapshotResult', consumed via
-- 'snapshotFailureMaybe'); it is announced on the note feed but never
-- edited into the model's own 'WorkerResult' — receipts keep harness facts
-- and model claims apart.
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
        -- withHandlerTry's Left is the SUBSCRIBE step failing, before the
        -- body ever runs ("Tidepool.Event") — this degraded re-spawn cannot
        -- double-spawn a worker.
        Left err -> do
          say [fmt|{name} HEAD-move observation degraded: {show err}|]
          spawnAgent @WorkerResult (spawnSpecIn (worktreeId tree) name prompt) <&> fmap snd
  case result of
    Right wr -> do
      snapshot <- snapshotWork tree name
      traverse_ (\detail -> say [fmt|{name}: snapshot failed — {detail}|]) (maybeToList snapshot.snapshotFailure)
      -- Surface self-reported friction immediately (note feed + log); it
      -- also rides the receipt via 'finishFold'.  Advisory only.
      traverse_ (\f -> say [fmt|{name} friction: {f}|]) wr.frictionNotes
      pure (Right (wr, snapshot))
    Left err -> pure (Left err)

-- | The harness's own commit of whatever a worker left uncommitted.
--
-- Codex workers cannot commit in their sandbox.  A clean tree is a no-op;
-- otherwise the harness stages and commits the worker's output, then reads
-- HEAD again so every caller's next observation sees the real commit.
-- Every step's exit code is checked: a failed commit (an unset git
-- identity, a hook) is a snapshot FAILURE, never reported as success with
-- the work still sitting uncommitted.
snapshotWork :: WorktreeHandle -> Text -> Harness SnapshotResult
snapshotWork tree name =
  statusEntries tree >>= \case
    Left f -> pure (SnapshotResult True False (Just (renderGitFailure f)))
    Right [] -> pure (SnapshotResult False True Nothing)
    Right _ ->
      gitRun tree ["add", "-A"] >>= \case
        Left f -> pure (SnapshotResult True False (Just (renderGitFailure f)))
        Right _ ->
          -- The name is DATA on the argv seam — a quote or metachar in a
          -- (model-derived) worker name cannot reach a shell.
          gitRun tree ["commit", "-m", name <> ": agent work"] >>= \case
            Left f -> pure (SnapshotResult True False (Just (renderGitFailure f)))
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

-- | One orchestrator-run check command.  The command is planner-authored
-- SHELL, run deliberately through the shell seam; its three outcomes stay
-- typed ('CheckOutcome').  A check that cannot run — a spawn failure, or
-- the shell's own command-not-found — indicts the CHECK, and is reported as
-- neither a pass nor a red verdict on the work.
runCheckCmd :: WorktreeHandle -> Text -> Harness CheckResult
runCheckCmd tree cmd =
  runIn (treeDir tree) cmd >>= \case
    Left e -> pure (CheckResult cmd (CheckUnrunnable (renderExecError e)))
    Right pr
      | ok pr -> pure (CheckResult cmd CheckPassed)
      | pr.exitCode == 127 ->
          pure (CheckResult cmd (CheckUnrunnable [fmt|exit 127 (command not found): {diagnose pr}|]))
      | otherwise -> pure (CheckResult cmd (CheckFailed pr.exitCode (diagnose pr)))

-- | Changed paths outside the declared boundary, and inside the tolerated
-- hygiene tier, at this worktree's HEAD relative to its seed.  An empty
-- boundary is unrestricted.  'Left' is a boundary that could not be CHECKED
-- — git failed, or an entry could not be parsed — and is loud, never
-- convertible to "stayed inside".
boundaryViolations :: WorktreeHandle -> [Text] -> [Text] -> Harness (Either Text ([Text], [Text]))
boundaryViolations _ [] _ = pure (Right ([], []))
boundaryViolations tree prefixes tolerated =
  case (traverse parseRepoPath prefixes, traverse parseRepoPath tolerated) of
    (Left why, _) -> pure (Left [fmt|boundary entry unparseable: {why}|])
    (_, Left why) -> pure (Left [fmt|tolerated entry unparseable: {why}|])
    (Right bounds, Right tol) ->
      changedPaths tree seedHead >>= \case
        Left f -> pure (Left (renderGitFailure f))
        Right changed -> pure (Right (foldr (classify bounds tol) ([], []) changed))
  where
    seedHead = renderGitOid tree.handleReceipt.sourceHead
    classify bounds tol f (outside, toleratedPaths)
      | any (pathWithin f) bounds = (outside, toleratedPaths)
      | any (pathWithin f) tol = (outside, renderRepoPath f : toleratedPaths)
      | otherwise = (renderRepoPath f : outside, toleratedPaths)

branchOf :: WorktreeHandle -> Text
branchOf tree = renderBranchName tree.handleReceipt.branch

noteHeadMove :: Text -> Observed HeadChangeReceipt -> Harness ()
noteHeadMove name change = say (name <> " HEAD -> " <> renderGitOid receipt.newHead)
  where
    receipt = value change
