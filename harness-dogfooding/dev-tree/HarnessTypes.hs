{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Durable vocabulary for the recursive-development-tree dogfood (v2).
--
-- This file intentionally contains no live Agent, Event, or Worktree handles:
-- those are cycle-scoped runtime capabilities, and the seed\/task types that
-- carry them live in "Harness" instead.  Only the semantic plan, the typed
-- outcomes, and the fold receipts cross a resident-cycle boundary.
--
-- Everything here is also the answerer's vocabulary: @Harness@ imports this
-- module, so a @runLLMTurn \@ReplanDecision@ or @askUser \@Triage@ window can
-- name these types (see @HarnessSource::answerer_imports@).
module HarnessTypes
  ( State (..)
  , Phase (..)
  , Budget (..)
  , DevPlan (..)
  , OnFailure (..)
  , WorkerResult (..)
  , ResolutionResult (..)
  , ReplanDecision (..)
  , Triage (..)
  , TriageAction (..)
  , LayerApproval (..)
  , Outcome (..)
  , Failure (..)
  , FailureKind (..)
  , FoldReceipt (..)
  , CheckResult (..)
  , RebaseNote (..)
  , RebaseTier (..)
  , RunSummary (..)
  , initialState
  , render
  , outcomeNodeName
  , outcomeTrailOf
  , outcomeIsDone
  , outcomeLine
  , renderFailure
  , failedOutcome
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

-- ---------------------------------------------------------------------------
-- Checkpointed state
-- ---------------------------------------------------------------------------

data State = State
  { goal                :: Text
  , plan                :: DevPlan
  , phase               :: Phase
  , cycleCount          :: Int
  , snapshotDirtySource :: Bool
  , budget              :: Budget
  , lastRun             :: Maybe RunSummary
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | A payload constructor in a SUM must use record syntax: generic JSON has no
-- key to put a positional field under, and 'State' is checkpointed through
-- 'ToJSON'\/'FromJSON'.  A positional @Blocked Text@ typechecks as a plain ADT
-- and fails only when the derive is demanded.
data Phase
  = Ready
  | Completed
  | Blocked { blockedReason :: Text }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Enforced, not advisory (PRD 20, "Failure as data").
--
-- @maxAgentCycles@ is the whole run's agent-cycle allowance.  It is spent
-- STRUCTURALLY rather than through a counter: a node reserves what it needs
-- and divides the remainder among its children (see @Harness.childAllowance@),
-- because a whole run happens inside ONE @loop@ call and the row carries no
-- shared-mutable-state effect — by decision, not omission.  Conservative (an
-- underspending subtree does not return its share), deterministic, and
-- structurally immune to completion order.
data Budget = Budget
  { maxDepth       :: Int
  , maxAgentCycles :: Int
  , -- | Unfold a layer wider than this without asking, and the operator gate
    -- (@Harness.approveLayer@) escalates past its deterministic tier.
    gateWiderThan  :: Int
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- ---------------------------------------------------------------------------
-- The authored plan
-- ---------------------------------------------------------------------------

-- | The tree is authored data, not a runtime workflow graph.  'Harness.loop'
-- unfolds it with 'Tidepool.Swarm.hyloM'; the plan itself never materializes
-- as a worked tree.
--
-- @nodeChecks@ is trust-ladder rung 2 — commands the ORCHESTRATOR runs in the
-- node's worktree at the actual fold sha, never something an agent is asked to
-- claim.  @nodeBoundary@ is rung 1: file prefixes the node's diff must stay
-- inside, checked against the observed diff rather than requested in prose.
data DevPlan = DevPlan
  { nodeName      :: Text
  , nodeTask      :: Text
  , nodeChecks    :: [Text]
  , nodeBoundary  :: [Text]
  , nodeOnFailure :: OnFailure
  , childPlans    :: [DevPlan]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | PRD 20's failure-policy sum, applied by deterministic code.  Model
-- cognition enters only through 'Replan' (a planning window scoped to the
-- failure) and 'AskOperator' (a typed triage form).
data OnFailure = Retry | Replan | AskOperator | Abandon
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- ---------------------------------------------------------------------------
-- Agent result schemas
-- ---------------------------------------------------------------------------

-- | What every implementation and integration worker must finish its turn
-- with.
--
-- This type IS the worker's @outputSchema@: @spawnAgent \@WorkerResult@ derives
-- the schema the backend holds the worker to from this declaration's own
-- 'Generic' metadata, and decodes the terminal payload back through the same
-- 'FromJSON'.  There is no second description of the result shape to drift
-- from the one the caller pattern-matches on, which is why 'JsonSchema' is in
-- the derive set and not optional.
--
-- SINGLE-CONSTRUCTOR RECORD, deliberately: a sum renders @oneOf@ at the schema
-- root and the backend refuses the turn whole at request validation.  An
-- alternative is modelled as a field ('readyForIntegration'), never as a
-- constructor.
data WorkerResult = WorkerResult
  { workSummary         :: Text
  , evidence            :: [Text]
  , readyForIntegration :: Bool
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The ephemeral conflict-resolution agent's result — tier 2 of the rebase
-- cascade.  @resolved@ false is an ordinary answer, not an error: it is what
-- turns into an escalation the parent's failure policy reads as data.
data ResolutionResult = ResolutionResult
  { resolved        :: Bool
  , resolutionNotes :: Text
  , conflictedPaths :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | The 'Replan' window's answer: a planning window scoped to the failed
-- subtree's render, not a free-form retry.
data ReplanDecision = ReplanDecision
  { amendedInstruction :: Text
  , abandonSubtree     :: Bool
  , rationale          :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The 'AskOperator' typed triage form.
data Triage = Triage
  { triageAction :: TriageAction
  , triageNote   :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

data TriageAction = TriageRetry | TriageSkip | TriageAbandon
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The layer gate's answer.  The operator approves an UNFOLD, one layer at a
-- time, with the parent's real scaffold outcome already landed — never a
-- speculative whole-tree sign-off.
data LayerApproval = LayerApproval
  { layerApproved :: Bool
  , approvalNote  :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- ---------------------------------------------------------------------------
-- Outcomes, failures, receipts
-- ---------------------------------------------------------------------------

-- | What a node folds to.  PRD 20's @Done Receipt | Failed Failure | Skipped
-- Reason@, with one addition: @outcomeTrail@, a FLAT bottom-up list of one
-- rendered line per node in this subtree.
--
-- The trail is why @render@ can show the whole annotated run without the plan
-- tree ever materializing — a node's trail is its children's trails
-- concatenated in plan order, plus its own line.  A list, not a tree: nothing
-- here is a second artifact store, and nothing here is re-traversed.
data Outcome
  = Done
      { outcomeNode  :: Text
      , outcomeTrail :: [Text]
      , doneReceipt  :: FoldReceipt
      }
  | Failed
      { outcomeNode    :: Text
      , outcomeTrail   :: [Text]
      , outcomeFailure :: Failure
      , partialReceipt :: Maybe FoldReceipt
      }
  | Skipped
      { outcomeNode  :: Text
      , outcomeTrail :: [Text]
      , skipReason   :: Text
      }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Failure is DATA and it accumulates: a failed child arrives at its parent's
-- algebra as an ordinary value in the input list, and @traverse@ has already
-- visited every sibling.  Nothing here short-circuits.
data Failure = Failure
  { failureKind   :: FailureKind
  , failureDetail :: Text
  , failurePaths  :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Enumerated so the parent's failure policy is an exhaustive case the
-- compiler audits, and so render/triage can speak about kinds rather than
-- parse prose.
data FailureKind
  = WorktreeDenied
  | SpawnDenied
  | NoHeadMove
  | ChecksFailed
  | BoundaryViolated
  | RebaseEscalated
  | MergeEscalated
  | BudgetSpent
  | DepthCapped
  | LayerRefused
  | ChildrenFailed
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The typed receipt every fold carries (PRD 20, "The trust ladder and
-- evidence").  Nothing merges without one; 'Harness.receipted' refuses a fold
-- that cannot produce it.
--
-- Rung 3 (adversarial review) has its slot here (@receiptReviewed@) and is
-- deliberately @False@ in this lane — the reviewer spawn is the next increment
-- on the same seam, and a receipt that CLAIMED review would be exactly the
-- kind of unearned trust the ladder exists to refuse.
data FoldReceipt = FoldReceipt
  { receiptNode      :: Text
  , receiptBranch    :: Text
  , receiptSeedHead  :: Text
  , receiptHead      :: Text
  , receiptHeadMoved :: Bool
  , receiptChecks    :: [CheckResult]
  , receiptRebases   :: [RebaseNote]
  , receiptOutside   :: [Text]
  , receiptCycles    :: Int
  , receiptReviewed  :: Bool
  , receiptSummary   :: Text
  , receiptEvidence  :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | One orchestrator-run check (rung 2).  @checkDetail@ carries the first line
-- of stderr, or the typed 'Tidepool.Effects.ExecError' when the command could
-- not be spawned at all — a check that never ran is a FAILING check, never a
-- silently missing one.
data CheckResult = CheckResult
  { checkCommand :: Text
  , checkExit    :: Int
  , checkDetail  :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | One entry in the eager rebase cascade: which tip was moved, onto what, and
-- by which tier.
data RebaseNote = RebaseNote
  { rebaseBranch :: Text
  , rebaseOnto   :: Text
  , rebaseTier   :: RebaseTier
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The three tiers, plus the fast path.  'RebaseCurrent' costs one
-- @merge-base --is-ancestor@ and no rebase at all; 'RebaseClean' is mechanical
-- git at zero tokens; 'RebaseResolved' spent one ephemeral agent cycle;
-- 'RebaseEscalation' is the typed hand-off to the parent's failure policy.
data RebaseTier
  = RebaseCurrent
  | RebaseClean
  | RebaseResolved
  | RebaseEscalation
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Model reports are useful summaries; Agent and Worktree receipts remain the
-- authoritative account of commands, changed files, commits, and HEAD moves.
data RunSummary = RunSummary
  { runRoot           :: Text
  , runStatus         :: Text
  , runTrail          :: [Text]
  , runEscalations    :: [Text]
  , retainedWorktrees :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- ---------------------------------------------------------------------------
-- Outcome helpers (pure; shared by Harness and render)
-- ---------------------------------------------------------------------------

outcomeNodeName :: Outcome -> Text
outcomeNodeName o = case o of
  Done {outcomeNode = n} -> n
  Failed {outcomeNode = n} -> n
  Skipped {outcomeNode = n} -> n

outcomeTrailOf :: Outcome -> [Text]
outcomeTrailOf o = case o of
  Done {outcomeTrail = t} -> t
  Failed {outcomeTrail = t} -> t
  Skipped {outcomeTrail = t} -> t

outcomeIsDone :: Outcome -> Bool
outcomeIsDone o = case o of
  Done {} -> True
  Failed {} -> False
  Skipped {} -> False

-- | One rendered line per node — the unit the trail is built from.
outcomeLine :: Outcome -> Text
outcomeLine o = case o of
  Done {outcomeNode = n, doneReceipt = r} ->
    [fmt|{n}: done at {r.receiptHead} ({checksLine r}, {length r.receiptRebases} rebase steps)|]
  Failed {outcomeNode = n, outcomeFailure = f} -> [fmt|{n}: FAILED — {renderFailure f}|]
  Skipped {outcomeNode = n, skipReason = why} -> [fmt|{n}: skipped — {why}|]

checksLine :: FoldReceipt -> Text
checksLine r = case r.receiptChecks of
  [] -> "no checks"
  cs -> [fmt|{length (filter passed cs)}/{length cs} checks passed|]
  where
    passed c = c.checkExit == 0

renderFailure :: Failure -> Text
renderFailure f = [fmt|{show f.failureKind}: {f.failureDetail}{pathsPart}|]
  where
    pathsPart = case f.failurePaths of
      [] -> "" :: Text
      ps -> " [" <> T.intercalate ", " ps <> "]"

-- | Build a 'Failed' outcome whose trail already carries its own line.
failedOutcome :: Text -> [Text] -> Failure -> Maybe FoldReceipt -> Outcome
failedOutcome n childTrail f receipt =
  Failed
    { outcomeNode = n
    , outcomeTrail = childTrail <> [[fmt|{n}: FAILED — {renderFailure f}|]]
    , outcomeFailure = f
    , partialReceipt = receipt
    }

-- ---------------------------------------------------------------------------
-- Initial state
-- ---------------------------------------------------------------------------

initialState :: State
initialState =
  State
    { goal = "Land the first useful typed Worktree/Event substrate in Tidepool"
    , plan = initialPlan
    , phase = Ready
    , cycleCount = 0
    , snapshotDirtySource = False
    , budget = Budget {maxDepth = 3, maxAgentCycles = 24, gateWiderThan = 4}
    , lastRun = Nothing
    }

initialPlan :: DevPlan
initialPlan =
  DevPlan
    { nodeName = "integration"
    , nodeTask =
        "Own the shared seam, keep the tree coherent, and integrate the child "
          <> "branches after their workers finish."
    , nodeChecks = ["cargo check --workspace"]
    , nodeBoundary = []
    , nodeOnFailure = AskOperator
    , childPlans =
        [ DevPlan
            { nodeName = "worktree-runtime"
            , nodeTask =
                "Implement managed worktree allocation, stable identities, "
                  <> "clean-source rejection, and the opt-in dirty snapshot."
            , nodeChecks = ["cargo check -p tidepool-worktree"]
            , nodeBoundary = ["tidepool-worktree"]
            , nodeOnFailure = Replan
            , childPlans =
                [ DevPlan
                    { nodeName = "commit-monitor"
                    , nodeTask =
                        "Implement reconciled commit and HEAD-change observation. "
                          <> "Start with polling and leave the hook/socket seam clean."
                    , nodeChecks = ["cargo check -p tidepool-worktree"]
                    , nodeBoundary = ["tidepool-worktree/src/events"]
                    , nodeOnFailure = Retry
                    , childPlans = []
                    }
                ]
            }
        , DevPlan
            { nodeName = "haskell-surface"
            , nodeTask =
                "Implement Event and withHandler with lexical, cycle-scoped "
                  <> "handler lifetimes and parent-effect execution."
            , nodeChecks = ["cargo check -p tidepool-mcp"]
            , nodeBoundary = ["haskell/lib/Tidepool", "tidepool-mcp/src"]
            , nodeOnFailure = Retry
            , childPlans = []
            }
        , DevPlan
            { nodeName = "acceptance"
            , nodeTask =
                "Write end-to-end acceptance coverage for worktree isolation, "
                  <> "dirty-source policy, event delivery, and retained state."
            , nodeChecks = ["cargo check --workspace --tests"]
            , nodeBoundary = ["tidepool-worktree/tests"]
            , nodeOnFailure = Abandon
            , childPlans = []
            }
        ]
    }

-- ---------------------------------------------------------------------------
-- Render
-- ---------------------------------------------------------------------------

-- | @render :: State -> Text@ — the LOCKED signature (see
-- @examples\/harness\/HarnessTypes.hs@).  Domain policy only: the driver
-- composes this output with the loop-iteration count, the prior compaction
-- summary, and capability\/finalization instructions.  This function does not
-- take the compaction summary as an argument, because that is a runtime fact
-- and the runtime's to supply.
-- @[fmt|{hole}|]@ renders a hole through @Tidepool.Render.Render@, which has
-- instances for Text\/String\/Int\/Double\/Bool\/Char and nothing else — an
-- author-defined type has no rendering the quoter could guess. So 'Phase' and
-- 'RunSummary' get explicit ones here, which is also where they belong: how a
-- phase reads to the model is domain policy, not a @Show@ accident.
render :: State -> Text
render st =
  [fmt|You are operating a typed recursive software-development tree.
Goal: {goal st}
Phase: {phaseLine}
Resident cycle: {cycleCount st}
Budget: depth {b.maxDepth}, {b.maxAgentCycles} agent cycles, gate layers wider than {b.gateWiderThan}

Plan:
{renderPlan 0 (plan st)}

Dirty source snapshot allowed: {snapshotDirtySource st}
{lastRunBlock}

The Haskell resident owns orchestration: the tree is a monadic hylomorphism
whose coalgebra splits (scaffold worker, then child worktrees seeded from the
reviewed scaffold HEAD) and whose algebra combines (leaf implementation, or the
eager rebase cascade plus the merge). Headless coding agents retain their
native edit, shell, test, and Git tools. Repository events are authoritative;
agent summaries are not.|]
  where
    b = budget st
    phaseLine = case phase st of
      Ready -> "ready" :: Text
      Completed -> "completed"
      Blocked {blockedReason = reason} -> "blocked — " <> reason
    lastRunBlock = case lastRun st of
      Nothing -> "No development-tree run has completed yet." :: Text
      Just summary ->
        T.intercalate "\n" $
          [[fmt|Last run: {summary.runRoot} — {summary.runStatus}|]]
            <> map ("  " <>) summary.runTrail
            <> map ("  escalation: " <>) summary.runEscalations
            <> map ("  retained worktree: " <>) summary.retainedWorktrees

renderPlan :: Int -> DevPlan -> Text
renderPlan depth p =
  indent <> "- " <> nodeName p <> ": " <> nodeTask p <> policy <> children
  where
    indent = T.replicate depth "  "
    policy = [fmt| (on failure: {show (nodeOnFailure p)})|]
    children = case childPlans p of
      [] -> ""
      xs -> "\n" <> T.intercalate "\n" (map (renderPlan (depth + 1)) xs)
