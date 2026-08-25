{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Durable vocabulary for the recursive-development-tree dogfood (v2).
--
-- This file intentionally contains no live Agent, Event, or Worktree handles:
-- those are loop-iteration-scoped runtime capabilities, and the seed\/task types that
-- carry them live in "Harness" instead.  Only the semantic plan, the typed
-- outcomes, and the fold receipts cross a resident-loop-iteration boundary.
--
-- Everything here is also the answerer's vocabulary: @Harness@ imports this
-- module, so a @runLLMTurn \@ReplanDecision@ or @askUser \@Triage@ agent session can
-- name these types (see @HarnessSource::answerer_imports@).
module HarnessTypes
  ( State (..)
  , Phase (..)
  , Budget (..)
  , ChoreMode (..)
  , SprintItem (..)
  , DevPlan (..)
  , OnFailure (..)
  , SplitSpec (..)
  , RepoSurvey (..)
  , Microtask (..)
  , MicrotaskResult (..)
  , MicrotaskRun (..)
  , MicroPlan (..)
  , WorkerResult (..)
  , ResolutionResult (..)
  , ReplanDecision (..)
  , Triage (..)
  , TriageAction (..)
  , LayerApproval (..)
  , PlanApproval (..)
  , Outcome (..)
  , Failure (..)
  , FailureKind (..)
  , FoldReceipt (..)
  , CheckResult (..)
  , RebaseNote (..)
  , RebaseTier (..)
  , RunSummary (..)
  , render
  , renderPlan
  , outcomeNodeName
  , outcomeTrailOf
  , outcomeIsDone
  , outcomeLine
  , renderFailure
  , failedOutcome
  , withTrail
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
  , -- | How many times a COMPLETED run has re-entered to consume a pending
    -- journaled amendment (bounded to one).  'Maybe' so checkpoints written
    -- before this field decode as 'Nothing'.
    rescueCount         :: Maybe Int
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

-- | Enforced, not advisory: failure is data.
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

-- | Whether the chore supplies its tree or asks the harness to propose one.
-- The payload constructor uses record syntax because this sum can influence
-- checkpointed state.
data ChoreMode
  = Authored {authoredPlan :: DevPlan}
  | ProposeFromGoal
  | SprintBacklog {sprintItems :: [SprintItem]}
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | One backlog item in a sprint run: a goal the planner turns into its own
-- subtree (or an authored override taken verbatim), sized by a cycle
-- allowance.  A sprint's items are designed to PARALLELIZE — disjoint
-- boundaries are validated before approval, and siblings execute
-- concurrently.
data SprintItem = SprintItem
  { itemGoal   :: Text
  , itemPlan   :: Maybe DevPlan
  , itemCycles :: Int
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
  , -- | Exact-or-directory-prefix paths a node MAY touch without failing the
    -- boundary check.  They are reported as informational evidence and are
    -- never product paths.
    nodeTolerated :: [Text]
  , nodeOnFailure :: OnFailure
  , -- | When set on a LEAF, the node's task is micro-decomposed on the fly
    -- (see 'SplitSpec') instead of being handed to one worker whole.  The
    -- MACRO tree stays authored data; this is the one place decomposition is
    -- delegated to a model, and it never creates worktrees or branches —
    -- everything happens inside this node's own worktree.  Ignored on an
    -- interior node.
    nodeSplit     :: Maybe SplitSpec
  , childPlans    :: [DevPlan]
  }
  -- No JsonSchema on purpose: runLLMTurn answers cross IN-HEAP through the
  -- Finalize-pinned row (no JSON Schema anywhere in that path), and the
  -- generic schema derivation cannot express this type's recursion anyway.
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- ---------------------------------------------------------------------------
-- On-the-fly micro-decomposition (intra-node)
--
-- The authored tree names lanes; a 'SplitSpec' lets ONE lane's task be split
-- into small sequential codex cycles by a model at run time.  Three steps,
-- three types: a read-only recon agent maps the repo ('RepoSurvey'), the
-- harness model turns task + survey into typed 'Microtask's ('MicroPlan'),
-- and deterministic code runs them in order — snapshot-committing and
-- check-running between cycles exactly as it does for whole workers.  The
-- rubric each microtask is held to ('microChecks') is authored BY the
-- planner but RUN by the orchestrator: ad-hoc in origin, code-owned in
-- enforcement.
-- ---------------------------------------------------------------------------

-- | Authored guidance for the on-the-fly split — what the chore is allowed
-- to say about HOW a node's task should be cut up.
data SplitSpec = SplitSpec
  { -- | Rubric hints handed to the planner verbatim: sizing, ordering,
    -- style, no-gos.
    splitHints    :: Text
  , -- | Hard cap on planned microtasks.  The effective cap is the smaller of
    -- this and the node's remaining agent-cycle allowance; anything the
    -- planner proposes past it is dropped LOUDLY (it rides in the receipt's
    -- evidence, never silently).
    splitMaxTasks :: Int
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | What the read-only recon agent reports back before any planning happens.
-- This type IS the recon cycle's @outputSchema@ (hence 'JsonSchema'), the
-- same way 'WorkerResult' is a worker's.  Deliberately FLAT: prose fields a
-- planning turn reads, not structure code acts on.
data RepoSurvey = RepoSurvey
  { -- | The repo's shape: layout, languages, build/test entry points.
    surveyLayout  :: Text
  , -- | Paths most relevant to the node's task, best first.
    relevantFiles :: [Text]
  , -- | Hazards a planner should route around (fragile files, missing
    -- tooling, surprising conventions).
    surveyRisks   :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | One small delegated codex task — the unit the planner emits.  Its
-- checks are commands the ORCHESTRATOR runs in the worktree after the
-- cycle, same trust rung as 'DevPlan''s @nodeChecks@: the planner authors
-- the rubric, code applies it.
data Microtask = Microtask
  { microName        :: Text
  , microInstruction :: Text
  , microChecks      :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The orchestrator's typed account of one accepted microtask.  The worker's
-- prose remains evidence, but the checks are the code-owned values that must
-- be carried into the containing node's receipt and trust ladder.
data MicrotaskResult = MicrotaskResult
  { microResultName    :: Text
  , microResultSummary :: Text
  , microResultChecks  :: [CheckResult]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The complete account of one sequential microtask run.  A run may stop
-- before all accepted tasks finish; the explicit accounting keeps that fact
-- typed rather than hiding it in an escalation line.
data MicrotaskRun = MicrotaskRun
  { microtaskResults      :: [MicrotaskResult]
  , microtasksAccepted    :: Int
  , microtasksRan         :: Int
  , microtasksComplete    :: Bool
  , microtaskEvidence     :: [Text]
  , microtaskEscalations  :: [Text]
  , microtaskObstacles    :: [Text]
  , microtaskFrictions    :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The planning turn's whole answer ('Harness.microLeaf' asks for it with
-- @runLLMTurn \@MicroPlan@).  Tasks run in LIST ORDER, sequentially, in the
-- node's one worktree — earlier tasks lay foundations later ones build on.
data MicroPlan = MicroPlan
  { microtasks     :: [Microtask]
  , microRationale :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The failure-policy sum, applied by deterministic code.  Model
-- cognition enters only through 'Replan' (a planning agent session scoped to the
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
  , -- | What went wrong along the way, in order, INCLUDING failures the
    -- worker later recovered from — the part of the story that vanishes
    -- when only the final state is reported.  Empty for a clean run.
    obstacles           :: [Text]
  , -- | Self-reported harness friction: anything about the brief, tooling,
    -- sandbox, or checks that made THIS task harder than it should have
    -- been, or a tweak the worker would request.  Empty when nothing stood
    -- out.
    --
    -- Both fields are ADVISORY ONLY — the optimization loop reads them to
    -- evolve the surface; no policy or ladder rung ever acts on them
    -- (receipts stay code-owned, prose stays prose).
    frictionNotes       :: [Text]
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

-- | The 'Replan' agent session's answer: a planning agent session scoped to the failed
-- subtree's render, not a free-form retry.
data ReplanDecision = ReplanDecision
  { amendedInstruction :: Text
  , abandonSubtree     :: Bool
  , rationale          :: Text
  , -- | Replan-as-decompose: a replacement subtree when the failure verdict
    -- is "mis-sized", not "mis-instructed" — the failed node is re-entered
    -- as this structure instead of as a rephrased leaf.  Its root's
    -- 'nodeName' is forced back to the failed node's own name on
    -- consumption ('Resume.amendPlan'), because retained worktrees rebind
    -- by name.  'Maybe' so journaled decisions from before this field
    -- decode as 'Nothing'.
    amendedSubtree     :: Maybe DevPlan
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

-- | Flat operator response to a prose rendering of a proposed plan. Recursive
-- plans are deliberately never exposed as editable form fields.
data PlanApproval = PlanApproval
  { planApproved :: Bool
  , revisionNote :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- ---------------------------------------------------------------------------
-- Outcomes, failures, receipts
-- ---------------------------------------------------------------------------

-- | What a node folds to: @Done Receipt | Failed Failure | Skipped
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
  -- Keep new constructors at the end so existing derived JSON constructor
  -- tags retain their clean-path meanings.
  | MicrotasksIncomplete
      { acceptedMicrotasksRan :: Int
      }
  | SnapshotFailed
      { snapshotName :: Text
      }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The typed receipt every fold carries — the trust ladder and
-- evidence.  Nothing merges without one; 'Harness.receipted' refuses a fold
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
  , receiptAgentRan  :: Bool
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
    [fmt|{n}: done at {r.receiptHead} ({checksLine r}, {length r.receiptRebases} rebase steps, {escalationCount r} escalated, {r.receiptCycles} agent cycles)|]
  Failed {outcomeNode = n, outcomeFailure = f} -> [fmt|{n}: FAILED — {renderFailure f}|]
  Skipped {outcomeNode = n, skipReason = why} -> [fmt|{n}: skipped — {why}|]

-- | How many of this fold's rebase steps reached tier 3.  Counted per node so
-- the run trail shows escalations wherever they happened, not only at the root
-- (a fused fold keeps no tree to walk back down).
escalationCount :: FoldReceipt -> Int
escalationCount r = length (filter ((== RebaseEscalation) . rebaseTier) r.receiptRebases)

checksLine :: FoldReceipt -> Text
checksLine r = case r.receiptChecks of
  [] -> "no checks"
  cs -> [fmt|{length (filter passed cs)}/{length cs} checks passed|]
  where
    passed c = c.checkExit == 0

renderFailure :: Failure -> Text
renderFailure f = [fmt|{renderFailureKind f.failureKind}: {f.failureDetail}{pathsPart}|]
  where
    pathsPart = case f.failurePaths of
      [] -> "" :: Text
      ps -> " [" <> T.intercalate ", " ps <> "]"

-- | Render payload-bearing failure kinds without making callers parse their
-- derived 'Show' form.  Existing nullary kinds retain their established text.
renderFailureKind :: FailureKind -> Text
renderFailureKind kind = case kind of
  MicrotasksIncomplete {acceptedMicrotasksRan = ran} ->
    [fmt|MicrotasksIncomplete ({ran} accepted microtasks ran)|]
  SnapshotFailed {snapshotName = snapshot} -> [fmt|SnapshotFailed ({snapshot})|]
  _ -> show kind

-- | Build a 'Failed' outcome.
--
-- The trail is left EMPTY on purpose: an outcome's trail is filled in one
-- place, by the @receipted@ stamp, which is the only code that has both this
-- node's line and its children's trails in hand.  See 'withTrail'.
failedOutcome :: Text -> Failure -> Maybe FoldReceipt -> Outcome
failedOutcome n f receipt =
  Failed
    { outcomeNode = n
    , outcomeTrail = []
    , outcomeFailure = f
    , partialReceipt = receipt
    }

-- | Fill an outcome's trail: its children's trails in plan order, then its own
-- line.  A plain record update — @outcomeTrail@ is present in every
-- constructor, so there is nothing to case-split on.
withTrail :: [Text] -> Outcome -> Outcome
withTrail childTrail o = o {outcomeTrail = childTrail <> [outcomeLine o]}

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
  indent <> "- " <> nodeName p <> ": " <> nodeTask p <> policy <> split <> children
  where
    indent = T.replicate depth "  "
    policy = [fmt| (on failure: {show (nodeOnFailure p)})|]
    split = case nodeSplit p of
      Nothing -> "" :: Text
      Just s -> [fmt| (micro-split on the fly, max {s.splitMaxTasks} tasks)|]
    children = case childPlans p of
      [] -> ""
      xs -> "\n" <> T.intercalate "\n" (map (renderPlan (depth + 1)) xs)
