{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE LambdaCase #-}
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
  , NoteRouting (..)
  , Outcome (..)
  , Failure (..)
  , FailureKind (..)
  , FoldReceipt (..)
  , CheckResult (..)
  , RebaseNote (..)
  , RebaseTier (..)
  , RunSummary (..)
  , CheckOutcome (..)
  , NoOpVerdict (..)
  , checkFailed
  , checkRed
  , checkUnrunnable
  , renderCheck
  , renderRebaseNote
  , render
  , renderPlan
  , outcomeNodeName
  , outcomeTrailOf
  , outcomeIsDone
  , outcomeReceipt
  , outcomeLine
  , renderFailure
  , failedOutcome
  , withTrail
  , RepoPath
  , parseRepoPath
  , gitPath
  , renderRepoPath
  , pathWithin
  , pathsOverlap
  , planSubtree
  , planScaffolds
  , planNames
  , duplicateNames
  , subtreeBoundaries
  , subtreeWriteable
  ) where

import qualified Data.List as L
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
  , -- | Whether an INTERIOR node runs a scaffold worker before its children
    -- fork.  @Just False@ is the TYPED integration-only form — children
    -- seed straight from the parent's HEAD and no worker runs (a prose
    -- "make no edits" brief demonstrably cannot be trusted; two live runs
    -- had the scaffold invent files and trip its own boundary).  'Nothing'
    -- derives from the task: scaffold exactly when 'nodeTask' is non-blank
    -- ('planScaffolds'), so structure is never switched by prose emptiness
    -- alone when a planner can say it outright.  Ignored on a leaf.
    nodeScaffold  :: Maybe Bool
  , childPlans    :: [DevPlan]
  , -- | This node's agent-cycle ask.  'Nothing' takes an equal share of the
    -- parent's remaining allowance; 'Just' is honored (scaled down
    -- proportionally when siblings' asks oversubscribe the parent).  This is
    -- what makes a sprint item's budget REAL at runtime rather than prompt
    -- advice (sol review, run 24).  'Maybe' so pre-field journaled plans
    -- decode.
    nodeCycles    :: Maybe Int
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

-- | One operator revision note, routed ONCE to the sprint items it concerns
-- — a bounded semantic question ("which item is this about?") answered by
-- one model call, instead of N item planners each independently deciding
-- whether a note about someone else applies to them.  @itemNotes@ is
-- positional per sprint item; an empty entry means "not concerned" and that
-- item's planner sees no revision text at all.
data NoteRouting = NoteRouting
  { itemNotes        :: [Text]
  , routingRationale :: Text
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
  | -- | The harness's own machinery could not observe or act — git failed in
    -- a worktree, recon status could not run, a boundary could not be
    -- checked.  The WORK is not indicted; triage should read this as "fix
    -- the environment", never as a verdict on the node's changes.
    InfraFailure
  -- Wire note: generic JSON tags a sum by constructor NAME, not position, so
  -- adding or reordering constructors is safe.  The breaking change is
  -- turning an existing nullary constructor into a record one (its payload
  -- shape changes) — rename instead when that happens.
  | MicrotasksIncomplete
      { acceptedMicrotasksRan :: Int
      }
  | SnapshotFailed
      { snapshotName :: Text
      }
  | -- | An interior node whose fold left children unmerged — the verdict
    -- CARRIES the per-child ledger instead of erasing it into a bare Done
    -- (the sprint-25 defect: a checkless integration root folded Done over
    -- three failed children, which both lied and structurally blocked the
    -- amendment-rescue re-entry, since rescue keys off a non-Done root).
    -- Partial delivery is a live, resumable state, not a terminal crash:
    -- merged children are already folded and harvestable, pending ones
    -- re-enter on resume with their journaled amendments.
    ChildrenPending
      { pendingChildren :: [Text]
      , mergedChildren  :: [Text]
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
  , -- | @Just reason@ when an agent cycle moved no HEAD and a bounded model
    -- verdict ('NoOpVerdict') judged that legitimate — the task genuinely
    -- required no edit.  'Nothing' otherwise; the ladder fails a headless
    -- cycle that carries no justification.
    receiptNoOp      :: Maybe Text
  , receiptSummary   :: Text
  , receiptEvidence  :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The bounded semantic question behind a headless agent cycle: "did this
-- agent do nothing, or was there nothing to do?"  Deterministic code cannot
-- answer it (the honest no-op and the dressed-up failure look identical in
-- git), so it is deferred to a model over the worker's own evidence.
data NoOpVerdict = NoOpVerdict
  { noOpLegitimate :: Bool
  , noOpReason     :: Text
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | One orchestrator-run check (rung 2), with its three honest outcomes kept
-- apart.  A check that never RAN earns no trust — it is not a pass — but it
-- also indicts the check's own spelling or environment rather than the work,
-- and policy treats the two differently ('checkRed' vs 'checkUnrunnable').
data CheckResult = CheckResult
  { checkCommand :: Text
  , checkOutcome :: CheckOutcome
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

data CheckOutcome
  = CheckPassed
  | -- | Ran and exited nonzero.  @checkDiag@ is the tail-biased capture of
    -- BOTH streams ('Git.diagnose') — compilers and test runners put the
    -- verdict at the end, often on stdout.
    CheckFailed
      { checkExit :: Int
      , checkDiag :: Text
      }
  | -- | Could not run at all: spawn failure, or the shell's own
    -- command-not-found.  Never a pass, never a red verdict on the work.
    CheckUnrunnable
      { checkReason :: Text
      }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Untrustworthy: ran red, or could not run.  The trust ladder refuses both.
checkFailed :: CheckResult -> Bool
checkFailed c = case c.checkOutcome of
  CheckPassed -> False
  CheckFailed {} -> True
  CheckUnrunnable {} -> True

-- | Ran, and genuinely failed.
checkRed :: CheckResult -> Bool
checkRed c = case c.checkOutcome of
  CheckFailed {} -> True
  _ -> False

checkUnrunnable :: CheckResult -> Bool
checkUnrunnable c = case c.checkOutcome of
  CheckUnrunnable {} -> True
  _ -> False

-- | One line per check, for prompts and evidence — the diagnosis rides along,
-- so an agent asked to repair a failure is shown what actually failed.
renderCheck :: CheckResult -> Text
renderCheck c = case c.checkOutcome of
  CheckPassed -> [fmt|{c.checkCommand} — passed|]
  CheckFailed {checkExit = e, checkDiag = d} -> [fmt|{c.checkCommand} — exit {e}: {d}|]
  CheckUnrunnable {checkReason = r} -> [fmt|{c.checkCommand} — could not run: {r}|]

-- | One entry in the eager rebase cascade: which tip was moved, onto what, and
-- by which tier.  @rebaseOnto@ is 'Nothing' for an escalation — nothing was
-- rebased onto anything, and no sentinel pretends otherwise.
data RebaseNote = RebaseNote
  { rebaseBranch :: Text
  , rebaseOnto   :: Maybe Text
  , rebaseTier   :: RebaseTier
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

renderRebaseNote :: RebaseNote -> Text
renderRebaseNote n = case n.rebaseOnto of
  Just onto -> [fmt|{n.rebaseBranch} onto {onto}: {show n.rebaseTier}|]
  Nothing -> [fmt|{n.rebaseBranch}: escalated|]

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

-- | The receipt an outcome carries, when it carries one.
outcomeReceipt :: Outcome -> Maybe FoldReceipt
outcomeReceipt o = case o of
  Done {doneReceipt = r} -> Just r
  Failed {partialReceipt = r} -> r
  Skipped {} -> Nothing

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
  cs -> [fmt|{length (filter (not . checkFailed) cs)}/{length cs} checks passed|]

renderFailure :: Failure -> Text
renderFailure f = [fmt|{renderFailureKind f.failureKind}: {f.failureDetail}{pathsPart}|]
  where
    pathsPart = case f.failurePaths of
      [] -> "" :: Text
      ps -> " [" <> T.intercalate ", " ps <> "]"

-- | Render payload-bearing failure kinds without making callers parse their
-- derived 'Show' form.  EXHAUSTIVE on purpose — the type's own contract is
-- that render/triage speak about kinds, so a new kind must decide its
-- operator-facing text here, audited by the compiler.
renderFailureKind :: FailureKind -> Text
renderFailureKind kind = case kind of
  WorktreeDenied -> "WorktreeDenied"
  SpawnDenied -> "SpawnDenied"
  NoHeadMove -> "NoHeadMove"
  ChecksFailed -> "ChecksFailed"
  BoundaryViolated -> "BoundaryViolated"
  RebaseEscalated -> "RebaseEscalated"
  MergeEscalated -> "MergeEscalated"
  BudgetSpent -> "BudgetSpent"
  DepthCapped -> "DepthCapped"
  LayerRefused -> "LayerRefused"
  ChildrenFailed -> "ChildrenFailed"
  InfraFailure -> "InfraFailure"
  MicrotasksIncomplete {acceptedMicrotasksRan = ran} ->
    [fmt|MicrotasksIncomplete ({ran} accepted microtasks ran)|]
  SnapshotFailed {snapshotName = snapshot} -> [fmt|SnapshotFailed ({snapshot})|]
  ChildrenPending {pendingChildren = pend, mergedChildren = merged} ->
    [fmt|ChildrenPending (merged: {T.intercalate ", " merged}; pending: {T.intercalate ", " pend})|]

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

-- | The approval-time rendering.  Checks and boundaries are SHOWN, not just
-- names and tasks: the fold ladder enforces exactly these, so approving a
-- plan without seeing them is approving blind (run 24: a stale-path check
-- rode an approved plan into two worker frictions).
renderPlan :: Int -> DevPlan -> Text
renderPlan depth p =
  indent <> "- " <> nodeName p <> ": " <> nodeTask p <> policy <> split
    <> detail "checks" (nodeChecks p)
    <> detail "boundary" (nodeBoundary p)
    <> detail "tolerated" (nodeTolerated p)
    <> children
  where
    indent = T.replicate depth "  "
    -- The cycle ask is SHOWN: it is enforced arithmetic, and an operator
    -- approving a plan without seeing it is approving a starvation they
    -- could have caught.
    policy = [fmt| (on failure: {show (nodeOnFailure p)}{cyclesNote})|]
    cyclesNote = case nodeCycles p of
      Nothing -> "" :: Text
      Just c -> [fmt|; {c} cycles|]
    split = case nodeSplit p of
      Nothing -> "" :: Text
      Just s -> [fmt| (micro-split on the fly, max {s.splitMaxTasks} tasks)|]
    detail label = \case
      [] -> "" :: Text
      xs -> "\n" <> indent <> "    " <> label <> ": " <> T.intercalate " | " xs
    children = case childPlans p of
      [] -> ""
      xs -> "\n" <> T.intercalate "\n" (map (renderPlan (depth + 1)) xs)

-- ---------------------------------------------------------------------------
-- Repo-relative paths — parsed once, compared structurally
-- ---------------------------------------------------------------------------

-- | A repo-relative path held as its normalized segments, so no comparison
-- anywhere downstream depends on how a path was SPELLED.  Paths enter the
-- harness from exactly two fuzzy sources — model-authored plan fields
-- ('parseRepoPath', which validates) and git machine output ('gitPath',
-- which is already clean) — and both land here; raw-'Text' path comparison
-- is unrepresentable past this boundary.  (The bug this closes: "ci/"
-- string-prefixed into the match-nothing "ci//", failing a node for a
-- boundary it never left; "." and "./ci" were the same failure one spelling
-- over.)
newtype RepoPath = RepoPath [Text]
  deriving (Show, Eq, Ord)

-- | Parse a MODEL-AUTHORED path (a boundary, tolerated, or check-adjacent
-- entry): strip whitespace, normalize @.@ and empty segments away, and
-- REJECT what cannot literally name a file or subtree.  An entry that
-- normalizes to nothing is rejected too — an unrestricted boundary is
-- declared by having no entries, never by an empty one that would silently
-- match everything.
parseRepoPath :: Text -> Either Text RepoPath
parseRepoPath raw
  | T.null stripped = Left "empty path (an unrestricted boundary declares no entries instead)"
  | T.any (\c -> c == '*' || c == '?' || c == '[') stripped =
      Left [fmt|{raw}: glob characters are not supported — boundaries are literal files or directory prefixes|]
  | ".." `elem` segs = Left [fmt|{raw}: ".." would escape the repository|]
  | null segs = Left [fmt|{raw}: no path segments|]
  | otherwise = Right (RepoPath segs)
  where
    stripped = T.strip raw
    segs = filter (\s -> not (T.null s) && s /= ".") (T.splitOn "/" stripped)

-- | A path from git's own machine output (@-z@, quotePath off): already
-- repo-relative and clean, so this is total.
gitPath :: Text -> RepoPath
gitPath = RepoPath . filter (not . T.null) . T.splitOn "/"

renderRepoPath :: RepoPath -> Text
renderRepoPath (RepoPath segs) = T.intercalate "/" segs

-- | True when @path@ is @dir@ itself or lies anywhere inside it.
pathWithin :: RepoPath -> RepoPath -> Bool
pathWithin (RepoPath path) (RepoPath dir) = dir `L.isPrefixOf` path

-- | True when two paths denote the same file/subtree or one contains the
-- other — the symmetric form of 'pathWithin'.
pathsOverlap :: RepoPath -> RepoPath -> Bool
pathsOverlap x y = pathWithin x y || pathWithin y x

-- ---------------------------------------------------------------------------
-- The one subtree walk
-- ---------------------------------------------------------------------------

-- | Does this node run a scaffold worker before its children fork?  The
-- typed field wins; without one, a non-blank task means yes.
planScaffolds :: DevPlan -> Bool
planScaffolds p = fromMaybe (not (T.null (T.strip (nodeTask p)))) p.nodeScaffold

-- | Every node of a plan, root first.  The ONE subtree traversal — name,
-- boundary, and check collectors are projections of this, so two call sites
-- can never disagree about what "the whole subtree" means.
planSubtree :: DevPlan -> [DevPlan]
planSubtree p = p : concatMap planSubtree (childPlans p)

planNames :: DevPlan -> [Text]
planNames = map nodeName . planSubtree

-- | Names that appear more than once anywhere in the plan.  Names key
-- retained worktrees and journal branch lookups, so a duplicate is a
-- correctness hazard, not a style problem.
duplicateNames :: DevPlan -> [Text]
duplicateNames p = [n | (n : _ : _) <- L.group (L.sort (planNames p))]

-- | Every PRODUCT boundary entry in the subtree.
subtreeBoundaries :: DevPlan -> [Text]
subtreeBoundaries = concatMap nodeBoundary . planSubtree

-- | Everything the subtree may WRITE: product boundaries plus tolerated
-- hygiene paths.  This is the set concurrency questions (sprint
-- disjointness) must use — two items that both tolerate @docs/@ still race.
subtreeWriteable :: DevPlan -> [Text]
subtreeWriteable = concatMap (\q -> nodeBoundary q <> nodeTolerated q) . planSubtree
