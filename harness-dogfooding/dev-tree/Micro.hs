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
  , checkFailed
  , firstLine
  , gitIn
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
  recon <-
    spawnAgent @RepoSurvey
      (spawnSpecIn (worktreeId tree) (name <> "-recon") (reconPrompt p spec))
  case recon of
    Left err ->
      pure (failedOutcome name (Failure SpawnDenied [fmt|{name} recon: {renderSpawnError err}|] []) Nothing)
    Right (_, survey) -> do
      -- Recon is contractually read-only, but its native shell still runs in
      -- the node worktree.  Check the actual tree after the spawn, and clear
      -- only what this authorized recon lane could have left behind.
      gitIn tree "status --porcelain" >>= \case
        Left err ->
          reconFailure
            before
            survey
            [fmt|{name}: RECON STATUS FAILED — could not verify that recon stayed read-only: {err}|]
        Right status
          | status.exitCode /= 0 ->
              reconFailure
                before
                survey
                [fmt|{name}: RECON STATUS FAILED — git status --porcelain exited {status.exitCode}: {firstLine status.stderr}|]
          | T.null (T.strip status.stdout) -> runPlanned before survey []
          | otherwise -> do
              let stray = T.intercalate ", " (filter (not . T.null) (T.lines status.stdout))
              say [fmt|{name}: RECON STRAYED — resetting recon-owned changes: {stray}|]
              resets <- traverse (gitIn tree) ["checkout -- .", "clean -fd"]
              let resetFailures =
                    [ detail
                    | (command, result) <- zip ["git checkout -- .", "git clean -fd"] resets
                    , let detail = case result of
                            Left err -> [fmt|{command} could not run: {err}|]
                            Right proc
                              | proc.exitCode /= 0 -> [fmt|{command} exited {proc.exitCode}: {firstLine proc.stderr}|]
                              | otherwise -> ""
                    , not (T.null detail)
                    ]
              if null resetFailures
                then do
                  let resetEvidence = [fmt|{name}: recon strayed ({stray}) and was reset with git checkout -- . and git clean -fd|]
                  say resetEvidence
                  runPlanned before survey [resetEvidence]
                else
                  reconFailure
                    before
                    survey
                    [fmt|{name}: RECON RESET FAILED — recon strayed ({stray}); {T.intercalate "; " resetFailures}|]
  where
    runPlanned before survey reconEvidence = do
      microPlan <- runLLMTurn @MicroPlan (microPlanPrompt p spec survey)
      let planned = microPlan.microtasks
          -- One cycle is already spent on recon; the rest of this subtree's
          -- allowance bounds the sequence, under the authored cap.
          cap = min spec.splitMaxTasks (max 0 (seed.seedCycles - 1))
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
      if microRun.microtasksComplete
        then
          recordEvent
            MicroCompleteEvent
              { evKey = JournalKey (branchOf tree)
              , evMicrotaskNames = acceptedMicrotaskNames
              }
        else pure ()
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

    reconFailure before survey detail = do
      let failure = Failure BoundaryViolated detail []
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

-- | Run accepted microtasks in LIST order.  A microtask whose own checks fail
-- STOPS the sequence — later tasks were planned against a foundation that did
-- not hold — and everything unrun is said so, as data.  The returned pair's
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
              | any checkFailed checks ->
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
    failedNames checks = T.intercalate ", " (map (.checkCommand) (filter checkFailed checks))
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
