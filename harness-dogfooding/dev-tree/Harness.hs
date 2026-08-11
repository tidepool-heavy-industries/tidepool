{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Forward dogfood: recursive coding agents in retained Git worktrees.
--
-- Every name this file calls exists today: managed worktrees and typed
-- repository events (PRD 19, "Tidepool.Worktree"\/"Tidepool.Event") and the
-- typed one-cycle spawn (PRD 18 lane 1, "Tidepool.Agent.Spawn").
--
-- __Assumed row.__ @Harness@ is an alias for @M@, so this file needs a compile
-- whose row carries @RunLLMTurn@ (the harness monad) plus @Console@,
-- @Worktree@, @RepoEvent@, and @Subagent@.  The self-iterating driver's v1
-- outer session is @RunLLMTurn@-only, so pointing the driver at this file does
-- not run it yet; that is a row-composition gap, not a missing API.
--
-- __Why there is no rebase propagation.__ @spawnAgent \@r spec@ runs ONE cycle
-- to completion, so a child worktree created AFTER its parent's worker
-- returned is seeded from the parent's FINAL HEAD.  There is no window in
-- which a parent HEAD moves under a live child, and therefore nothing for a
-- rebase poke to do: depth-first ordering answers the whole problem.  See
-- 'runNode'.
module Harness
  ( State (..)
  , Phase (..)
  , DevPlan (..)
  , WorkerResult (..)
  , RunSummary (..)
  , initialState
  , render
  , loop
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Agent.Spawn (spawnAgent)
-- `SpawnError`/`spawnSpecIn`/`renderSpawnError` and the Console `say` are
-- generated into `Tidepool.Effects`; `Tidepool.Agent.Spawn` re-exports only the
-- spawn verbs themselves.
import Tidepool.Effects (SpawnError, renderSpawnError, say, spawnSpecIn)
import Tidepool.Event
import Tidepool.Harness (Harness)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Worktree

-- | A fully processed node: its own worker's result, its integrated children,
-- and the integration worker's result when there was anything to merge.
-- Runtime-only — never enters checkpointed 'State'.
data IntegratedNode = IntegratedNode
  { integratedPlan     :: DevPlan
  , integratedTree     :: WorktreeHandle
  , implementation     :: WorkerResult
  , integration        :: Maybe WorkerResult
  , integratedChildren :: [IntegratedNode]
  }

-- | Why a node stopped.  Both halves of the saga fail with a typed, renderable
-- value — no exceptions cross this boundary.
data NodeError
  = NodeWorktreeFailed Text WorktreeError
  | NodeSpawnFailed Text SpawnError

renderNodeError :: NodeError -> Text
renderNodeError = \case
  NodeWorktreeFailed n e -> n <> " (worktree): " <> renderWorktreeError e
  NodeSpawnFailed n e -> n <> " (agent): " <> renderSpawnError e

-- | One resident cycle unfolds a development tree into isolated agents and
-- folds their branches back upward.  Haskell never runs @git rebase@ or
-- @git merge@: coding agents do so with their native tools.
loop :: State -> Harness State
loop st
  | phase st /= Ready = pure st
  | otherwise =
      createWorktree (rootWorktreeSpec st) >>= \case
        -- Matching the SPECIFIC Left is what earns a better message than the
        -- generic one: this is the only failure the operator can act on
        -- directly, so it says how much is uncommitted and names the flag that
        -- drops the requirement.
        Left (SourceDirty summary) ->
          let dirtyFiles =
                length summary.staged + length summary.unstaged + length summary.untracked
           in pure
                ( blocked
                    st
                    [fmt|Source repository is dirty ({dirtyFiles} uncommitted paths). Commit them, or set snapshotDirtySource to run against a hidden snapshot.|]
                )
        Left err ->
          pure (blocked st [fmt|Could not create root worktree: {renderWorktreeError err}|])
        Right rootTree ->
          runNode rootTree (plan st) >>= \case
            Left err ->
              pure (blocked st [fmt|Development tree stopped at {renderNodeError err}|])
            Right integrated ->
              pure
                st
                  { phase = Completed
                  , cycleCount = cycleCount st + 1
                  , lastRun = Just (summarize integrated)
                  }

blocked :: State -> Text -> State
blocked st reason =
  st {phase = Blocked {blockedReason = reason}, cycleCount = cycleCount st + 1}

-- TODO(Worktree PRD): 'fromCurrentRepository' defaults to RequireClean.
-- 'allowDirtySnapshot' creates a hidden synthetic commit without touching the
-- user's branch or index.  Managed worktrees are retained indefinitely in v1.
rootWorktreeSpec :: State -> WorktreeSpec
rootWorktreeSpec st
  | snapshotDirtySource st = allowDirtySnapshot base
  | otherwise = base
  where
    base = fromCurrentRepository "dev-tree/integration"

-- | Run one node to completion, then its children, then integrate.
--
-- DEPTH-FIRST, PARENT FIRST, and that ordering is the whole design.  The node's
-- own implementation worker runs to completion before any child worktree is
-- created, so each child is seeded from a parent HEAD that is already final.
--
-- The 'withHandler' scope is observation, not control: a HEAD move recorded
-- from the repository is authoritative evidence that the worker committed,
-- where the worker's own 'workSummary' is only a claim.  The subscription
-- begins at registration and unregisters when the body ends, so it covers
-- exactly this node's worker.
runNode :: WorktreeHandle -> DevPlan -> Harness (Either NodeError IntegratedNode)
runNode tree p =
  withHandler (headChanged tree) (noteHeadMove p) (spawnWorker tree p) >>= \case
    Left err -> pure (Left (NodeSpawnFailed (nodeName p) err))
    Right implResult ->
      runChildren tree (childPlans p) >>= \case
        Left err -> pure (Left err)
        Right children -> integrateNode tree p implResult children

-- | The typed spawn: @\@WorkerResult@ is what fixes the schema the worker is
-- held to AND the type its terminal payload decodes into.  A payload that does
-- not fit comes back as @Left (SpawnResultMalformed …)@, never as a success
-- with a defaulted field.  The worktree already exists, so the spec names it by
-- id ('spawnSpecIn') rather than asking for a new one.
spawnWorker :: WorktreeHandle -> DevPlan -> Harness (Either SpawnError WorkerResult)
spawnWorker tree p =
  spawnAgent @WorkerResult (spawnSpecIn (worktreeId tree) (nodeName p) (workerPrompt p))
    <&> fmap snd

-- | Repository events are authoritative; agent summaries are not.
noteHeadMove :: DevPlan -> Observed HeadChangeReceipt -> Harness ()
noteHeadMove p change =
  say (nodeName p <> " HEAD -> " <> renderGitOid receipt.newHead)
  where
    receipt = value change

-- | Allocate and run each child in turn, stopping at the first failure.  Each
-- child worktree is created from the parent's CURRENT state, which by the
-- ordering in 'runNode' is the parent worker's final commit.
runChildren :: WorktreeHandle -> [DevPlan] -> Harness (Either NodeError [IntegratedNode])
runChildren _ [] = pure (Right [])
runChildren parentTree (p : ps) =
  createWorktree (fromWorktree parentTree (nodeName p)) >>= \case
    Left err -> pure (Left (NodeWorktreeFailed (nodeName p) err))
    Right childTree ->
      runNode childTree p >>= \case
        Left err -> pure (Left err)
        Right child ->
          runChildren parentTree ps >>= \case
            Left err -> pure (Left err)
            Right rest -> pure (Right (child : rest))

-- | Fold the children back in.  Every child is already integrated and its
-- worker terminal, so there is exactly one writer per worktree here.  A fresh
-- integration agent merges the child branches and resolves conflicts with its
-- own native Git tools.
integrateNode
  :: WorktreeHandle
  -> DevPlan
  -> WorkerResult
  -> [IntegratedNode]
  -> Harness (Either NodeError IntegratedNode)
integrateNode tree p implResult children = case children of
  [] -> pure (Right (node Nothing))
  _ -> do
    refs <- traverse (worktreeBranch . integratedTree) children
    spawnAgent @WorkerResult
      (spawnSpecIn (worktreeId tree) (nodeName p <> "-integration") (integrationPrompt p children refs))
      >>= \case
        Left err -> pure (Left (NodeSpawnFailed (nodeName p <> "-integration") err))
        Right (_, merged) -> pure (Right (node (Just merged)))
  where
    node merged =
      IntegratedNode
        { integratedPlan = p
        , integratedTree = tree
        , implementation = implResult
        , integration = merged
        , integratedChildren = children
        }

workerPrompt :: DevPlan -> Text
workerPrompt p = [fmt|
  You are the implementation worker for node {nodeName p}.
  Work only in the assigned worktree, using your native edit, shell, test, and
  Git tools.

  Task: {nodeTask p}

  Inspect the repository before editing. Keep your branch buildable and commit
  coherent progress — the commits are what your parent integrates, and the
  repository events they raise are the authoritative record of your work.
  Do not merely claim Git work: perform it, and cite the evidence.

  Finish your turn with a WorkerResult: a one-paragraph workSummary, an
  evidence list (commands run, checks passed, commits made), and
  readyForIntegration.
|]

integrationPrompt :: DevPlan -> [IntegratedNode] -> [BranchName] -> Text
integrationPrompt p children refs = [fmt|
  You are the integration worker for node {nodeName p}. Merge the completed
  child branches into this worktree:
{branchLines}

  Inspect every child diff and its test evidence, merge them one at a time with
  your native Git tools, resolve conflicts by understanding both
  implementations, run the combined checks, and commit the integrated result.
  Never discard a child's work merely to make the merge easy.

  Finish your turn with a WorkerResult describing what you merged and what you
  ran.
|]
  where
    branchLines = T.intercalate "\n" (zipWith renderChild children refs)
    renderChild child ref =
      "  - " <> nodeName (integratedPlan child) <> ": " <> renderBranchName ref

summarize :: IntegratedNode -> RunSummary
summarize root =
  RunSummary
    { implementationSummaries = map (workSummary . implementation) nodes
    , integrationSummaries =
        [ workSummary result
        | node <- nodes
        , Just result <- [integration node]
        ]
    , retainedWorktrees = map (renderWorktreeId . worktreeId . integratedTree) nodes
    }
  where
    nodes = flatten root

flatten :: IntegratedNode -> [IntegratedNode]
flatten node = node : concatMap flatten (integratedChildren node)
