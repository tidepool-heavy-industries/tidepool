{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Microtask decomposition and execution for recursive-development-tree
-- leaves.  This module owns the read-only recon lane, the typed planning turn,
-- and the sequential microtask runner; it delegates final fold receipt
-- construction through the callback seam supplied by the fold facade.
module Micro
  ( NodeSeed (..)
  , microLeaf
  , runMicrotasks
  , MicroAcc (..)
  ) where

import qualified Data.Text as T
import DevTreeJournal
  ( JournalEvent (..)
  , JournalKey (..)
  , recordEvent
  )
import Git
import HarnessTypes
import Tidepool.Agent.Spawn
  ( renderSpawnError
  , spawnAgent
  )
import Tidepool.Effects
  ( WorktreeHandle (..)
  , say
  , spawnSpecIn
  )
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Worktree
import Prompts
import Workers
  ( branchOf
  , runCheckCmd
  , runChecks
  , runWorker
  , snapshotFailureMaybe
  )

-- | What a node needs in order to be unfolded.  The hylo's @a@.
data NodeSeed = NodeSeed
  { seedPlan   :: DevPlan
  , seedTree   :: WorktreeHandle
  , seedDepth  :: Int
  , -- | Agent-cycle allowance for THIS SUBTREE.  Spent structurally: a node
    -- reserves what it needs and divides the remainder among its children, so
    -- the run's total is bounded with no mutable counter anywhere — and
    -- completion order cannot reach it.  See 'childAllowance'.
    seedCycles :: Int
  , -- | A scaffold commit a RESUMED run found already sitting in this node's
    -- retained worktree and verified (checks and boundary, at that sha).
    -- 'decompose' uses it as its scaffold head instead of spawning the
    -- scaffold worker again — the "never redo blind" half of adopt-and-verify.
    -- Always 'Nothing' on a fresh run.
    seedAdopted :: Maybe GitOid
  }

-- | The fold seam needed by a micro leaf.  The Fold module is still in the
-- facade during this incremental extraction, so Micro receives its existing
-- receipt builder rather than importing the facade or duplicating it.
type FinishFold =
  NodeSeed
    -> WorkerResult
    -> (GitOid, GitOid)
    -> [RebaseNote]
    -> [Text]
    -> Int
    -> Bool
    -> [CheckResult]
    -> Maybe Failure
    -> Harness Outcome

-- | A leaf with a 'SplitSpec': recon → plan → sequential microtask cycles,
-- all inside this node's ONE worktree.  The macro tree stays authored data;
-- this is the single seam where a model decides decomposition, and its
-- decision is enforced by the same code-owned machinery as everything else:
-- 'runWorker' snapshot-commits each cycle, the orchestrator runs each
-- microtask's own checks (planner-authored rubric, code-applied), and the
-- node's checks + fold ladder judge the whole at the end.
--
-- Cognition is split on the usual line: a READ-ONLY codex recon harvests the
-- repo into a typed 'RepoSurvey', the harness model turns task + survey into
-- a typed 'MicroPlan', and deterministic code routes from there.
--
-- Resume granularity is the NODE, deliberately: the micro-plan is not
-- journaled, so a crash mid-sequence resumes through the ordinary
-- adopt-and-verify path over whatever snapshot commits the sequence left.
-- Journaling per-microtask progress is a later increment on this same seam.
microLeaf :: FinishFold -> NodeSeed -> SplitSpec -> Harness Outcome
microLeaf finishFold seed spec = do
  before <- worktreeHead tree
  -- The dirty-state BASELINE, before recon ever runs.  A retained worktree
  -- resumed mid-crash can hold uncommitted work that is NOT recon's; without
  -- the baseline the cleanup below would blame recon for it and destroy it.
  statusEntries tree >>= \case
    Left f ->
      pure (failedOutcome name (Failure InfraFailure [fmt|{name}: pre-recon status failed — {renderGitFailure f}|] []) Nothing)
    Right preexisting
      | not (null preexisting) ->
          pure
            ( failedOutcome
                name
                ( Failure
                    InfraFailure
                    [fmt|{name}: worktree already dirty before recon ({T.intercalate ", " preexisting}) — refusing to run recon over uncommitted state|]
                    []
                )
                Nothing
            )
      | otherwise -> spawnRecon before
  where
    spawnRecon before =
      spawnAgent @RepoSurvey
        (spawnSpecIn (worktreeId tree) (name <> "-recon") (reconPrompt p spec))
        >>= \case
          Left err ->
            pure (failedOutcome name (Failure SpawnDenied [fmt|{name} recon: {renderSpawnError err}|] []) Nothing)
          Right (_, survey) ->
            -- Recon is contractually read-only, but its native shell still
            -- runs in the node worktree.  Check the actual tree after the
            -- spawn; the pre-spawn baseline above proved it was clean, so
            -- anything dirty now is recon's.
            statusEntries tree >>= \case
              Left f ->
                reconFailure
                  before
                  survey
                  [fmt|{name}: could not verify that recon stayed read-only — {renderGitFailure f}|]
              Right [] -> runPlanned before survey []
              Right stray -> do
                let strayLine = T.intercalate ", " stray
                say [fmt|{name}: RECON STRAYED — resetting recon-owned changes: {strayLine}|]
                resetToHead >>= \case
                  Just why -> reconFailure before survey [fmt|{name}: recon strayed ({strayLine}) and the reset failed — {why}|]
                  Nothing -> do
                    let resetEvidence = [fmt|{name}: recon strayed ({strayLine}) and was reset (git reset --hard + clean -fd, verified clean)|]
                    say resetEvidence
                    runPlanned before survey [resetEvidence]

    -- Reset the worktree to HEAD and PROVE it landed: @reset --hard@ undoes
    -- staged changes too (a bare @checkout -- .@ restores from the index and
    -- cannot), and the closing status re-read is the load-bearing part — a
    -- reset that silently left dirt would otherwise smuggle recon's writes
    -- into the next worker's snapshot commit.
    resetToHead =
      gitRun tree ["reset", "--hard", "HEAD"] >>= \case
        Left f -> pure (Just (renderGitFailure f))
        Right _ ->
          gitRun tree ["clean", "-fd"] >>= \case
            Left f -> pure (Just (renderGitFailure f))
            Right _ ->
              statusEntries tree >>= \case
                Left f -> pure (Just (renderGitFailure f))
                Right [] -> pure Nothing
                Right still -> pure (Just [fmt|still dirty after reset: {T.intercalate ", " still}|])

    runPlanned before survey reconEvidence = do
      -- One cycle is already spent on recon; the rest of this subtree's
      -- allowance bounds the sequence, under the authored cap.  An allowance
      -- that leaves room for NOTHING is a budget refusal, said as one —
      -- never a "complete" run of zero microtasks.
      let cap = min spec.splitMaxTasks (max 0 (seed.seedCycles - 1))
      if cap <= 0
        then
          pure
            ( failedOutcome
                name
                ( Failure
                    BudgetSpent
                    [fmt|{name}: no agent cycles remain for microtasks after recon (subtree allowance {seed.seedCycles}, authored cap {spec.splitMaxTasks})|]
                    []
                )
                Nothing
            )
        else do
          microPlan <- runLLMTurn @MicroPlan (microPlanPrompt p spec survey)
          let planned = normalizeMicroNames microPlan.microtasks
              (toRun, dropped) = splitAt cap planned
              droppedNames = T.intercalate ", " (map (.microName) dropped)
              droppedNote =
                [ [fmt|micro-split: {length dropped} planned microtasks past the cap ({cap}) were not run: {droppedNames}|]
                | not (null dropped)
                ]
          say [fmt|{name}: planned {length planned} microtasks, running {length toRun}|]
          let acceptedMicrotaskNames = map (.microName) toRun
          recordEvent
            MicroSplitEvent
              { evKey = JournalKey (branchOf tree)
              , evMicrotaskNames = acceptedMicrotaskNames
              }
          (microRun, microSnapshotFailure) <- runMicrotasks tree p toRun
          when microRun.microtasksComplete $
            recordEvent
              MicroCompleteEvent
                { evKey = JournalKey (branchOf tree)
                , evMicrotaskNames = acceptedMicrotaskNames
                }
          after <- worktreeHead tree
          checks <- runChecks tree p
          let wr =
                WorkerResult
                  { workSummary =
                      [fmt|Micro-split leaf: {microRun.microtasksRan} of {microRun.microtasksAccepted} accepted microtasks ran ({length planned} proposed). Plan rationale: {microPlan.microRationale}|]
                  , evidence = [fmt|recon survey: {survey.surveyLayout}|] : reconEvidence <> microRun.microtaskEvidence <> droppedNote
                  , readyForIntegration = null microRun.microtaskEscalations
                  , obstacles = microRun.microtaskObstacles
                  , frictionNotes = microRun.microtaskFrictions
                  }
              -- Keep every microtask check in the receipt, including the failing
              -- one that stopped the sequence.  The node checks remain first so
              -- the pre-existing receipt ordering stays stable; the ladder then
              -- judges both sets mechanically at the final head.
              receiptChecks = checks <> concatMap (.microResultChecks) microRun.microtaskResults
          folded <-
            finishFold
              seed
              wr
              (before, after)
              []
              microRun.microtaskEscalations
              (1 + microRun.microtasksRan)
              True
              receiptChecks
              microSnapshotFailure
          pure $ case folded of
            Done {doneReceipt = receipt}
              | not microRun.microtasksComplete ->
                  failedOutcome
                    name
                    ( Failure
                        (MicrotasksIncomplete {acceptedMicrotasksRan = microRun.microtasksRan})
                        [fmt|microtask sequence stopped after {microRun.microtasksRan} of {microRun.microtasksAccepted} accepted microtasks ran|]
                        []
                    )
                    (Just receipt)
            _ -> folded

    -- Recon-lane machinery failure: the environment, not the work, is
    -- indicted, and the kind says so.
    reconFailure before survey detail = do
      let failure = Failure InfraFailure detail []
          wr =
            WorkerResult
              { workSummary = [fmt|Read-only recon could not be verified for {name}|]
              , evidence = [[fmt|recon survey: {survey.surveyLayout}|], detail]
              , readyForIntegration = False
              , obstacles = [detail]
              , frictionNotes = []
              }
      say detail
      after <- worktreeHead tree
      checks <- runChecks tree p
      finishFold seed wr (before, after) [] [] 1 True checks (Just failure)

    tree = seed.seedTree
    p = seed.seedPlan
    name = nodeName p

-- | Model-authored 'microName's become worker names, commit-message text,
-- and journaled accept/complete lists — so they are normalized to a safe
-- slug at ACCEPTANCE, the one seam where model text enters, and duplicates
-- are disambiguated so the journaled name lists compare exactly.
normalizeMicroNames :: [Microtask] -> [Microtask]
normalizeMicroNames ms = go [] (zip [1 :: Int ..] ms)
  where
    go _ [] = []
    go seen ((i, m) : rest) =
      let base = case slug m.microName of
            "" -> [fmt|task-{i}|]
            s -> s
          named = if base `elem` seen then [fmt|{base}-{i}|] else base
       in m {microName = named} : go (named : seen) rest
    slug = T.intercalate "-" . T.words . T.map keepSafe
    keepSafe c
      | c `elem` ("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_" :: [Char]) = c
      | otherwise = ' '

-- | Run accepted microtasks in LIST order.  A microtask whose own checks RAN
-- RED stops the sequence — later tasks were planned against a foundation
-- that did not hold — and everything unrun is said so, as data.  A check
-- that could not run at all ('CheckUnrunnable') is recorded but does NOT
-- halt: it indicts the planner's rubric line, not the foundation, and the
-- node's final ladder still refuses to trust it.  The returned pair's
-- 'MicrotaskRun' carries every executed microtask's checks in typed form,
-- together with explicit accepted/ran/completed accounting; the optional
-- 'Failure' carries a snapshot failure without hiding it in prose.
runMicrotasks :: WorktreeHandle -> DevPlan -> [Microtask] -> Harness (MicrotaskRun, Maybe Failure)
runMicrotasks tree p accepted = walk emptyMicroAcc accepted
  where
    walk acc [] = pure (closeRun True acc, Nothing)
    walk acc (m : rest) =
      runWorker tree (microWorkerName m) (microPrompt p m) >>= \case
        Left err ->
          halt (noteEscalation [fmt|{m.microName}: spawn failed — {renderSpawnError err}|] acc) rest Nothing
        Right (wr, snapshot) -> do
          checks <- traverse (runCheckCmd tree) m.microChecks
          let acc' = recordCycle m wr checks acc
          case snapshotFailureMaybe (microWorkerName m) snapshot of
            Just failure -> halt (noteEscalation (renderFailure failure) acc') rest (Just failure)
            Nothing
              | any checkRed checks ->
                  halt (noteEscalation [fmt|{m.microName}: micro checks failed — {failedNames checks}|] acc') rest Nothing
              | otherwise -> walk acc' rest

    -- A sequence that stops says how much was left undone, then closes.
    halt acc rest failure = pure (closeRun False (unrunNote rest acc), failure)
    unrunNote rest acc
      | null rest = acc
      | otherwise = noteEscalation [fmt|{length rest} remaining microtasks not run (sequence stopped)|] acc

    -- One completed worker cycle, folded into the accumulator whole: its
    -- typed result, its evidence line, and its tagged self-reports.
    recordCycle m wr checks acc =
      MicroAcc
        { accResults = MicrotaskResult m.microName wr.workSummary checks : accResults acc
        , accEvidence = cycleLine m wr checks : accEvidence acc
        , accEscalations = accEscalations acc
        , accObstacles = map (tag m) wr.obstacles <> accObstacles acc
        , accFrictions = map (tag m) wr.frictionNotes <> accFrictions acc
        , accSpent = accSpent acc + 1
        }
    cycleLine m wr checks =
      let passed = length (filter (not . checkFailed) checks)
       in [fmt|{m.microName}: {wr.workSummary} ({passed}/{length checks} micro checks passed)|]
    noteEscalation line acc = acc {accEscalations = line : accEscalations acc}
    failedNames checks = T.intercalate ", " (map (.checkCommand) (filter checkRed checks))
    tag m t = m.microName <> ": " <> t
    microWorkerName m = nodeName p <> "-" <> m.microName

    -- The accumulator holds newest-first lists; this is the one reversal.
    -- @ranToEnd@ is the honest completion bit: a halt is incomplete even
    -- when it happened on the final task.
    closeRun ranToEnd acc =
      MicrotaskRun
        { microtaskResults = reverse (accResults acc)
        , microtasksAccepted = length accepted
        , microtasksRan = accSpent acc
        , microtasksComplete = ranToEnd
        , microtaskEvidence = reverse (accEvidence acc)
        , microtaskEscalations = reverse (accEscalations acc)
        , microtaskObstacles = reverse (accObstacles acc)
        , microtaskFrictions = reverse (accFrictions acc)
        }

-- | The interior accumulator for one micro sequence.  Every list is
-- newest-first while accumulating; 'runMicrotasks' reverses once when it
-- closes the run.
data MicroAcc = MicroAcc
  { accResults     :: [MicrotaskResult]
  , accEvidence    :: [Text]
  , accEscalations :: [Text]
  , accObstacles   :: [Text]
  , accFrictions   :: [Text]
  , accSpent       :: Int
  }

emptyMicroAcc :: MicroAcc
emptyMicroAcc = MicroAcc {accResults = [], accEvidence = [], accEscalations = [], accObstacles = [], accFrictions = [], accSpent = 0}
