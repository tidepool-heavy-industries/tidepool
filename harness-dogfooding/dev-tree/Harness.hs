{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE RecordWildCards #-}

-- | Forward dogfood: recursive coding agents in retained Git worktrees.
--
-- This is deliberately a sketch of the intended authored surface.  It will
-- compile after PRD 18 (typed headless subagents) and the forthcoming narrow
-- Worktree/Event PRD land.  TODO(PRD 18 / Worktree PRD): keep this file as the
-- executable acceptance target while those APIs are implemented.
module Harness
  ( State (..)
  , Phase (..)
  , DevPlan (..)
  , DevMessage (..)
  , WorkerResult (..)
  , RunSummary (..)
  , initialState
  , render
  , loop
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Agent
import Tidepool.Event
import Tidepool.Harness (Harness)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Worktree

-- Runtime-only values.  These never enter checkpointed 'State'.
data PreparedNode = PreparedNode
  { preparedPlan     :: DevPlan
  , preparedTree     :: WorktreeHandle
  , preparedChildren :: [PreparedNode]
  }

data LiveNode = LiveNode
  { livePlan     :: DevPlan
  , liveTree     :: WorktreeHandle
  , liveAgent    :: AgentHandle DevMessage WorkerResult
  , liveChildren :: [LiveNode]
  }

data FinishedNode = FinishedNode
  { finishedPlan     :: DevPlan
  , finishedTree     :: WorktreeHandle
  , workerResult     :: WorkerResult
  , finishedChildren :: [FinishedNode]
  }

data IntegratedNode = IntegratedNode
  { integratedPlan     :: DevPlan
  , integratedTree     :: WorktreeHandle
  , implementation     :: WorkerResult
  , integration        :: Maybe WorkerResult
  , integratedChildren :: [IntegratedNode]
  }

-- | One resident cycle unfolds a development tree into isolated agents, keeps
-- parent->child rebase pokes live while they work, then folds completed child
-- branches upward through fresh integration agents.  Haskell never runs
-- @git rebase@ or @git merge@: coding agents do so with their native tools.
loop :: State -> Harness State
loop st
  | phase st /= Ready = pure st
  | otherwise = do
      created <- createWorktree (rootWorktreeSpec st)
      case created of
        Left (SourceDirty summary) ->
          pure st
            { phase = Blocked [fmt|Source repository is dirty: {summary}|]
            , cycleCount = cycleCount st + 1
            }

        Left err ->
          pure st
            { phase = Blocked [fmt|Could not create root worktree: {renderWorktreeError err}|]
            , cycleCount = cycleCount st + 1
            }

        Right rootTree -> do
          prepared <- prepareNode rootTree (plan st)
          case prepared of
            Left err ->
              pure st
                { phase = Blocked [fmt|Could not prepare development tree: {renderWorktreeError err}|]
                , cycleCount = cycleCount st + 1
                }

            Right tree -> do
              finished <- runTree tree
              integrated <- integrateTree finished
              pure st
                { phase = Completed
                , cycleCount = cycleCount st + 1
                , lastRun = Just (summarize integrated)
                }

-- TODO(Worktree PRD): 'fromCurrentRepository' defaults to RequireClean.
-- 'allowDirtySnapshot' creates a hidden synthetic commit without touching the
-- user's branch or index.  Managed worktrees are retained indefinitely in v1.
rootWorktreeSpec :: State -> WorktreeSpec
rootWorktreeSpec st
  | snapshotDirtySource st = allowDirtySnapshot base
  | otherwise = base
  where
    base = fromCurrentRepository "dev-tree/integration"

-- | Allocate the entire tree before starting workers.  A child is seeded from
-- its parent's current HEAD, but no persistent parent/child graph is built into
-- the runtime; the recursive relationship belongs to this resident program.
prepareNode
  :: WorktreeHandle
  -> DevPlan
  -> Harness (Either WorktreeError PreparedNode)
prepareNode tree p =
  prepareChildren tree (childPlans p) >>= \case
    Left err -> pure (Left err)
    Right children -> pure (Right (PreparedNode
      { preparedPlan = p
      , preparedTree = tree
      , preparedChildren = children
      }))

prepareChildren
  :: WorktreeHandle
  -> [DevPlan]
  -> Harness (Either WorktreeError [PreparedNode])
prepareChildren _ [] = pure (Right [])
prepareChildren parentTree (p : ps) = do
  -- TODO(Worktree PRD): this snapshots the parent's committed HEAD only.  The
  -- parent worker has not started yet, so the seed is stable and clean.
  createWorktree (fromWorktree parentTree (nodeName p)) >>= \case
    Left err -> pure (Left err)
    Right childTree ->
      prepareNode childTree p >>= \case
        Left err -> pure (Left err)
        Right child ->
          prepareChildren parentTree ps >>= \case
            Left err -> pure (Left err)
            Right rest -> pure (Right (child : rest))

-- | CPS keeps every lexical handler scope alive until the root continuation
-- finishes.  Descendants start first; each node's HEAD handler is registered
-- before that node's worker can make its first commit.
withLiveNode
  :: PreparedNode
  -> (LiveNode -> Harness a)
  -> Harness a
withLiveNode PreparedNode {..} k =
  withLiveForest preparedChildren $ \children ->
    withHandler (headChanged preparedTree) (pokeChildren preparedPlan children) $ do
      worker <- spawnAgent
        (workerSpec preparedTree preparedPlan)
        (workerPrompt preparedPlan)
      k (LiveNode
        { livePlan = preparedPlan
        , liveTree = preparedTree
        , liveAgent = worker
        , liveChildren = children
        })

withLiveForest
  :: [PreparedNode]
  -> ([LiveNode] -> Harness a)
  -> Harness a
withLiveForest [] k = k []
withLiveForest (p : ps) k =
  withLiveNode p $ \node ->
    withLiveForest ps $ \nodes ->
      k (node : nodes)

-- | A real HEAD transition is a poke, not a magic rebase effect.  Each child
-- decides where to stop, whether to make a WIP commit, how to rebase, and how
-- to resolve conflicts.  Its subsequent Worktree events are the evidence.
pokeChildren
  :: DevPlan
  -> [LiveNode]
  -> Observed HeadChangeReceipt
  -> Harness ()
pokeChildren parentPlan children observed =
  for_ children $ \child ->
    pokeAgent (liveAgent child) (RebaseWhenSafe
      { upstreamNode = nodeName parentPlan
      , upstreamHead = renderGitOid (newHead (payload observed))
      })

-- TODO(PRD 18): this dogfood wants the typed "poke" behavior discussed after
-- the initial PRD: steer an active turn at its next safe boundary, or enqueue a
-- follow-up turn when the durable agent is idle.  It must never silently drop
-- the message.  'sendMessage' is the intended compact authored spelling.
pokeAgent
  :: AgentHandle DevMessage WorkerResult
  -> DevMessage
  -> Harness ()
pokeAgent = sendMessage

runTree :: PreparedNode -> Harness FinishedNode
runTree tree = withLiveNode tree finishTree

-- All agents were spawned asynchronously by 'withLiveNode', so these waits may
-- be traversed sequentially without serializing the workers themselves.
finishTree :: LiveNode -> Harness FinishedNode
finishTree LiveNode {..} = do
  result <- waitForResult liveAgent
  children <- traverse finishTree liveChildren
  pure FinishedNode
    { finishedPlan = livePlan
    , finishedTree = liveTree
    , workerResult = result
    , finishedChildren = children
    }

-- | Fold bottom-up.  Once all implementation workers are terminal there is
-- only one writer per worktree.  A fresh integration agent in each interior
-- node merges its already-integrated child branches and resolves conflicts.
integrateTree :: FinishedNode -> Harness IntegratedNode
integrateTree FinishedNode {..} = do
  children <- traverse integrateTree finishedChildren
  merged <- case children of
    [] -> pure Nothing
    _ -> do
      refs <- traverse (worktreeBranch . integratedTree) children
      integrator <- spawnAgent
        (integrationSpec finishedTree finishedPlan)
        (integrationPrompt finishedPlan children refs)
      Just <$> waitForResult integrator
  pure IntegratedNode
    { integratedPlan = finishedPlan
    , integratedTree = finishedTree
    , implementation = workerResult
    , integration = merged
    , integratedChildren = children
    }

-- TODO(PRD 18): 'noTools' is the empty generated-tool contract.  Workers keep
-- Codex's native edit/search/shell/test tools; this example does not need a
-- child-to-parent MCP tool beyond typed steering from the resident.
workerSpec tree p = agent
  { instructions = [fmt|
      You are the implementation worker for node {nodeName p}.
      Work only in the assigned worktree. Use native edit, shell, test, and Git
      tools. Commit coherent progress. If you receive RebaseWhenSafe, finish a
      safe unit of work (making a WIP commit if useful), rebase onto the given
      parent HEAD, resolve conflicts, re-run relevant checks, and continue.
      Do not merely claim Git work: perform it and report the resulting evidence.
    |]
  , tools = noTools
  , model = Capable
  , workspace = workspaceOf tree
  , retention = Durable
  }

workerPrompt :: DevPlan -> Text
workerPrompt p = [fmt|
  Goal for this development tree: implement your assigned node independently.
  Node: {nodeName p}
  Task: {nodeTask p}

  Inspect the repository before editing. Keep your branch buildable, commit
  coherent progress, and finish with a typed WorkerResult.
|]

integrationSpec tree p = agent
  { instructions = [fmt|
      You are the integration worker for node {nodeName p}.
      Work only in the assigned worktree. Merge the supplied child branches
      using native Git, resolve conflicts by understanding both implementations,
      run the relevant checks, and commit the integrated result. Never discard a
      child's work merely to make the merge easy.
    |]
  , tools = noTools
  , model = Capable
  , workspace = workspaceOf tree
  , retention = Ephemeral
  }

integrationPrompt
  :: DevPlan
  -> [IntegratedNode]
  -> [BranchName]
  -> Text
integrationPrompt p children refs = [fmt|
  Integrate the completed child branches into node {nodeName p}:
{branchLines}

  Inspect every child diff and its test evidence, merge them one at a time,
  resolve conflicts semantically, run the combined checks, commit the result,
  and finish with a typed WorkerResult.
|]
  where
    branchLines = T.intercalate "\n" (zipWith renderChild children refs)
    renderChild child ref =
      "- " <> nodeName (integratedPlan child) <> ": " <> renderBranchName ref

summarize :: IntegratedNode -> RunSummary
summarize root = RunSummary
  { implementationSummaries = map (workSummary . implementation) nodes
  , integrationSummaries =
      [ workSummary result
      | node <- nodes
      , Just result <- [integration node]
      ]
  , retainedWorktrees =
      map (renderWorktreeId . worktreeId . integratedTree) nodes
  }
  where
    nodes = flatten root

flatten :: IntegratedNode -> [IntegratedNode]
flatten node = node : concatMap flatten (integratedChildren node)
