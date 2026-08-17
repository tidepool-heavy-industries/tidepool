{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Forward dogfood v2: the recursive development tree as a monadic
-- hylomorphism over "Tidepool.Swarm"'s @PlanF@.
--
-- v1 hand-rolled the scheme — @runNode@ ran its own worker, then its children,
-- then integrated, and recursed.  v2 names the shape instead of re-deriving
-- it, and gets four things the hand-rolled version could not have: lazy
-- layer-by-layer decomposition (a child is planned only after its parent's
-- scaffold landed), failure that accumulates rather than short-circuits,
-- policy as middleware over two function seams, and a recursive step a later
-- lane can replace wholesale.
--
-- __The two seams.__ Cognition enters at exactly two typed functions.
-- 'decompose' is HOW TO SPLIT: the parent-first scaffold worker, then one child
-- worktree per child plan seeded from the scaffold HEAD.  'integrate' is HOW TO
-- COMBINE: leaf implementation when there are no children, or the eager rebase
-- cascade plus the merge when there are.  Everything else in this file is
-- compiled coordination and costs zero tokens.
--
-- __Assumed row.__ @Harness@ is an alias for @M@, and this file needs
-- @RunLLMTurn@, @AskUser@, @Console@, @Worktree@, @RepoEvent@, @Exec@,
-- @Subagent@, and @Journal@ — which is exactly the driver's widened outer
-- session (@selfharness::driver::outer_decls@).
-- @tidepool-harness\/tests\/dogfood_harness_typecheck.rs@ compiles it against
-- that row.
--
-- __Why there IS rebase propagation now.__ v1 argued depth-first ordering
-- answered the whole problem: a child worktree was created only when it was
-- that child's turn, so it was always seeded from a parent HEAD that could no
-- longer move.  v2's coalgebra creates EVERY sibling worktree at once — that is
-- what emitting @PlanF task childSeeds@ means — and its algebra lands child
-- folds one at a time, so the parent HEAD genuinely moves under live sibling
-- tips.  The drift v1 designed around now exists, and 'cascade' is the answer:
-- mechanical git first, an ephemeral resolution agent second, escalation as
-- data third.  See @plans\/self-iterating-harness\/20-s1-l3-dev-tree-v2.md@.
--
-- __Resume.__ The run journal this file writes at every split, outcome,
-- replan, rebase, and escalation is read back by 'resumeLoop' — the opt-in
-- second entry point PRD 20 S1-L5 gives a harness that wants to survive a
-- crash.  'loop' IS @resumeLoop emptyResume@, so a fresh run and a resumed run
-- are one spelling of the run rather than two that can drift, and a fold with
-- no entries takes the ordinary path by construction ('resumed' is @id@ when
-- 'isResumed' is false).  See @plans\/self-iterating-harness\/20-s1-l5-resume.md@.
module Harness
  ( State (..)
  , Phase (..)
  , DevPlan (..)
  , WorkerResult (..)
  , Outcome (..)
  , RunSummary (..)
  , initialState
  , render
  , loop
  , resumeLoop
    -- * Resume decisions (pure — the fold's verdict before any git runs)
  , ResumePlan (..)
  , SplitRecord (..)
  , resumePlanFor
  , amendmentIsNewest
  , amendPlan
    -- * Agent-cycle budget (pure — exercised directly by the overspend pin)
  , NodeSeed (..)
  , requiredCycles
  , childAllowance
  ) where

import qualified Data.Text as T
import DevTreeJournal
  ( JournalEvent (..)
  , JournalKey (..)
  , JournalKind (..)
  , eventsOfKind
  , lookupEvent
  , recordEvent
  )
import HarnessTypes
import Tidepool.Agent.Spawn
  ( AgentHandle
  , awaitAgent
  , cancelAgent
  , renderSpawnError
  , spawnAgent
  , spawnAsync
  )
-- `SpawnError`/`spawnSpecIn`, the Console `say`, the Exec verbs, and the
-- worktree receipt's own fields are generated into `Tidepool.Effects`; the
-- curated modules re-export only their own vocabulary. `renderSpawnError`
-- moved to `Tidepool.Agent.Spawn` above (PRD 22 lane 3): it calls
-- `renderWorktreeError`, which is authored library code the generated module
-- cannot reach.
import Tidepool.Effects
  ( ExecError (..)
  , SpawnError
  , WorktreeHandle (..)
  , WorktreeReceipt (..)
  , WorktreeSummary (..)
  , runIn
  , say
  , spawnSpecIn
  )
import Tidepool.Event
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Resume
  ( ResumeFold (..)
  , emptyResume
  , isResumed
  )
import qualified Tidepool.Swarm as Swarm
import Tidepool.Worktree

-- ---------------------------------------------------------------------------
-- Runtime-only vocabulary
--
-- Live handles never enter checkpointed 'State' (PRD 19), so the seed and the
-- task — both of which carry a 'WorktreeHandle' — live here rather than in
-- "HarnessTypes".
-- ---------------------------------------------------------------------------

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

-- | What the coalgebra decided, handed to the algebra unchanged.  The hylo's
-- @t@.
--
-- A plain sum of the three mutually-exclusive things a coalgebra step can
-- decide — ordinary work, a pre-algebra refusal, or a resumed/adopted
-- terminal outcome — rather than three 'Maybe' fields on one product whose
-- precedence 'integrate' had to establish by nested inspection.  Every
-- construction site below (this module's four smart constructors:
-- 'splitWork', 'refusalWork', 'replayedWork', 'adoptedWork') names, by its own
-- constructor choice, whether children may exist — 'WorkReady' is the only
-- one that carries any.  This type is module-private; only those four sites
-- (plus their two match sites, 'integrate' and 'stampFold') construct or take
-- one apart.
--
-- @workKids@ is the parent's own record of the seeds it unfolded, in plan
-- order.  It is what lets the algebra zip its @[Outcome]@ back against the
-- worktrees those outcomes came from — @traverse@ preserves order, and plan
-- order is the ONLY order any policy here reads.
data NodeWork
  = WorkReady
      { workSeed     :: NodeSeed
      , workScaffold :: Maybe WorkerResult
      , workKids     :: [NodeSeed]
      , workDenied   :: [Text]
      }
  | -- | A veto before any work happened — a coalgebra cannot produce an
    -- outcome directly (its result type is @PlanF@), so every refusal in
    -- this file expresses itself this way instead.
    WorkRefused
      { workSeed    :: NodeSeed
      , workFailure :: Failure
      }
  | -- | An outcome a RESUMED run already has in hand, so this node is not
    -- worked at all.  'integrate' returns it verbatim.
    WorkResumed
      { workSeed    :: NodeSeed
      , workOutcome :: ResumedFold
      }

-- | Where a resumed node's outcome came from — and therefore whether it still
-- needs to be journaled.
--
-- The journal is append-only, so "already recorded" and "recorded by a process
-- that then crashed" are different obligations: re-appending the first would
-- be duplicate noise on every resume, and NOT appending the second would lose
-- the outcome the crash swallowed.
data ResumedFold
  = -- | Read straight out of the fold.  Already in the journal; do not append.
    ReplayedOutcome Outcome
  | -- | Synthesized here from work found in a retained worktree and verified
    -- at that sha.  APPEND it — the crashed process never got to.
    AdoptedOutcome Outcome

-- | What a failure policy decided about one escalation.
data PolicyOutcome
  = PolicyResolved Int
  | PolicyEscalated Text Int
  | PolicyAbandoned Text Int

-- | The interior fold's accumulator, threaded in plan order.
data FoldAcc = FoldAcc
  { accNotes   :: [RebaseNote]
  , accEsc     :: [Text]
  , accCycles  :: Int
  , accMerged  :: Int
  , accAbandon :: Maybe Text
  }

emptyAcc :: FoldAcc
emptyAcc = FoldAcc {accNotes = [], accEsc = [], accCycles = 0, accMerged = 0, accAbandon = Nothing}

-- ---------------------------------------------------------------------------
-- The resident cycle
-- ---------------------------------------------------------------------------

-- | One resident cycle unfolds a development tree into isolated agents and
-- folds their branches back upward.  Haskell never runs a git WORKFLOW verb
-- from the runtime (PRD 19's freeze): the mechanical tier below is authored
-- policy running plain git through 'Exec' in a worktree this node owns, and
-- everything cognitive is a coding agent with its own native tools.
loop :: State -> Harness State
loop = resumeLoop emptyResume

-- | The RESUMED entry (PRD 20 S1-L5).  The driver folds this run's journal at
-- boot and injects it here; 'loop' is this function at 'emptyResume', so there
-- is ONE spelling of the run.
--
-- A fresh run is byte-for-byte the old path: 'resumed' is the identity when
-- the fold carries nothing, no worktree is looked up rather than created, and
-- no adopt-and-verify git read happens.  Everything below the guard is what a
-- non-empty fold buys.
resumeLoop :: ResumeFold -> State -> Harness State
resumeLoop fold st
  | phase st /= Ready = pure st
  | otherwise =
      rootTree fold st >>= \case
        Left why -> pure (blocked st why)
        Right rootHandle -> do
          let seed =
                NodeSeed
                  { seedPlan = plan st
                  , seedTree = rootHandle
                  , seedDepth = 0
                  , seedCycles = (budget st).maxAgentCycles
                  , seedAdopted = Nothing
                  }
          -- SEAM (residency, PRD 20 S1-L4).  `hyloM`'s recursive step is
          -- `coalg a >>= traverse go >>= alg`, and `traverse go` is the ONE
          -- place residency changes: today a node is a stack frame that runs
          -- its children to completion in plan order and holds no state
          -- between them; under green threads it becomes `forkNode` per child
          -- plus a fold over `folded` handles, with the node's body a select
          -- loop over `childFolded <|> inbox <|> agentDone <|> headChanged`.
          -- Nothing below reads completion order, holds node-local mutable
          -- state across children, or threads anything through the traversal
          -- that is not the plan's own data — those three are what would make
          -- the swap expensive, so they are deliberately absent.
          --
          -- POLICY IS MIDDLEWARE, composed by ordinary function application
          -- (PRD 20, "The hylo core").  Read the coalgebra outside-in: the
          -- layer gate sees the produced layer, the depth cap and the cycle
          -- budget refuse BEFORE `decompose` spawns anything, and each one is
          -- a `Coalg -> Coalg` that a test can exercise against a pure
          -- coalgebra with no agent process anywhere.
          --
          -- RESUME IS THE OUTERMOST WRAPPER, deliberately.  A subtree the
          -- journal already accounts for must not be re-gated, re-capped, or
          -- re-budgeted: the operator approved that layer and those cycles
          -- were already spent, in a process that is gone.  Refusing finished
          -- work on a budget would discard it.
          let b = budget st
              coalg =
                resumed fold
                  ( Swarm.gated (layerGate b)
                      (Swarm.capped seedDepth b.maxDepth depthRefusal
                         (Swarm.budgeted cycleRefusal decompose))
                  )
              alg = Swarm.receipted stampFold integrate
          outcome <- Swarm.hyloM alg coalg seed
          summary <- summarize fold outcome
          pure
            st
              { phase = Completed
              , cycleCount = cycleCount st + 1
              , lastRun = Just summary
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

-- | The run's root worktree: REBOUND when the fold names it, created when it
-- does not.
--
-- PRD 19's retain-first rule is what makes this the first thing resume does —
-- creating a second root worktree beside the retained one would orphan every
-- commit under it, which is precisely the outcome the journal exists to
-- prevent.  The fold names the root branch structurally (the @split@ or
-- @outcome@ entry whose payload node is the root plan's name), so nothing
-- here guesses a branch from a label.
rootTree :: ResumeFold -> State -> Harness (Either Text WorktreeHandle)
rootTree fold st = case rootBranchOf fold (nodeName (plan st)) of
  Just branch ->
    retainWorktree branch >>= \case
      Left why -> pure (Left [fmt|Could not rebind the retained root worktree ({branch}): {why}|])
      Right rt -> pure (Right (retainedHandle rt))
  Nothing ->
    createWorktree (rootWorktreeSpec st) >>= \case
      -- Matching the SPECIFIC Left is what earns a better message than the
      -- generic one: this is the only failure the operator can act on
      -- directly, so it says how much is uncommitted and names the flag that
      -- drops the requirement.
      Left (SourceDirty summary) ->
        let dirtyFiles =
              length summary.staged + length summary.unstaged + length summary.untracked
         in pure
              ( Left
                  [fmt|Source repository is dirty ({dirtyFiles} uncommitted paths). Commit them, or set snapshotDirtySource to run against a hidden snapshot.|]
              )
      Left err -> pure (Left [fmt|Could not create root worktree: {renderWorktreeError err}|])
      Right h -> pure (Right h)

-- ---------------------------------------------------------------------------
-- The coalgebra — how to split
-- ---------------------------------------------------------------------------

-- | Unfold one node.  The BARE split — every policy that could refuse it is
-- middleware wrapped around it at the 'loop' call site, so what is left here
-- is only what splitting means.
--
-- Order is load-bearing:
--
-- 1. The scaffold worker runs for a node that HAS children — v1's
--    @spawnWorker@, unmoved.  It is why children seed from a parent HEAD that
--    is already final.  A LEAF spawns nothing here: for a leaf, "how to
--    combine nothing" IS "implement it", so its worker is the algebra's.
-- 2. The split is journaled.  Decomposition is cognition, so it is recorded
--    rather than re-derived; a resumed run replays it instead of re-asking
--    (PRD 20 S1-L5).
-- 3. Child worktrees are allocated from the scaffold HEAD.  A worktree that
--    cannot be created is not a split failure — that child is dropped and the
--    denial rides in 'workDenied' for the algebra to fold as an escalation.
decompose :: NodeSeed -> Harness (Swarm.PlanF NodeWork NodeSeed)
decompose seed
  | null kids = pure (Swarm.PlanF (splitWork seed Nothing [] []) [])
  | otherwise = case seed.seedAdopted of
      -- A resumed run already found this node's scaffold commit sitting in its
      -- retained worktree and VERIFIED it (checks + boundary, at that sha).
      -- Re-running the scaffold worker over it would be exactly the blind redo
      -- the journal exists to prevent.
      Just adopted -> emitSplit seed Nothing adopted
      Nothing ->
        runWorker seed.seedTree name (scaffoldPrompt p kids) >>= \case
          Left err ->
            pure
              ( Swarm.PlanF
                  (refusalWork seed (Failure SpawnDenied [fmt|{name} scaffold: {renderSpawnError err}|] []))
                  []
              )
          Right scaffold -> do
            scaffoldHead <- worktreeHead seed.seedTree
            emitSplit seed (Just scaffold) scaffoldHead
  where
    p = seed.seedPlan
    name = nodeName p
    kids = childPlans p

-- | Journal the split and allocate the children from the scaffold head.
--
-- The split is appended TWICE under the same @(kind, key)@, and the fold keeps
-- the later one (max seq).  That is not redundancy: the two appends close two
-- different crash windows.  The first records the DECISION, so a crash during
-- child allocation replays the plan instead of re-running the scaffold worker.
-- The second adds the child worktrees, which is the only durable record of
-- WHICH retained tree belongs to which child — without it a resumed run would
-- create a second worktree beside a child's orphaned commits and redo its work
-- blind.  Append-only, last-one-wins, no rewrite.
emitSplit :: NodeSeed -> Maybe WorkerResult -> GitOid -> Harness (Swarm.PlanF NodeWork NodeSeed)
emitSplit seed scaffold scaffoldHead = do
  recordEvent (SplitEvent (JournalKey branch) p scaffoldHeadText Nothing)
  (childSeeds, denied) <- allocateChildren seed kids (freshChild seed)
  recordEvent (SplitEvent (JournalKey branch) p scaffoldHeadText (Just (map childTreeEntry childSeeds)))
  pure (Swarm.PlanF (splitWork seed scaffold childSeeds denied) childSeeds)
  where
    p = seed.seedPlan
    kids = childPlans p
    branch = branchOf seed.seedTree
    scaffoldHeadText = renderGitOid scaffoldHead
    childTreeEntry s = (nodeName s.seedPlan, branchOf s.seedTree)

splitWork :: NodeSeed -> Maybe WorkerResult -> [NodeSeed] -> [Text] -> NodeWork
splitWork seed scaffold childSeeds denied =
  WorkReady
    { workSeed = seed
    , workScaffold = scaffold
    , workKids = childSeeds
    , workDenied = denied
    }

-- | The task a truncated node carries.  A coalgebra cannot produce an
-- outcome — its result type is @PlanF@ — so every veto in this file expresses
-- itself by handing the algebra a childless, refused node instead.
refusalWork :: NodeSeed -> Failure -> NodeWork
refusalWork seed f = WorkRefused {workSeed = seed, workFailure = f}

-- ---------------------------------------------------------------------------
-- The coalgebra's policy slots
--
-- Each is an ordinary function the middleware calls; each is effectful
-- (@a -> M (Maybe NodeWork)@) so it can tier — a deterministic heuristic
-- first, the operator past that — inside one function with ordinary
-- branching.  The two below that CAN be pure are pure, deliberately: a pure
-- slot is a slot a test can call directly.
-- ---------------------------------------------------------------------------

-- | The agent-cycle cost a node's own work requires: one for a leaf's
-- implementation, two for a node that splits (its own scaffold plus one
-- integration cycle).  Shared with 'childAllowance', which reserves the same
-- amount before dividing what remains among children — so the two can never
-- disagree about what "this node's own reservation" means.
requiredCycles :: DevPlan -> Int
requiredCycles p = if null (childPlans p) then 1 else 2

-- | 'Swarm.budgeted''s slot.  A node reserves its own scaffold plus one
-- integration cycle; a leaf reserves its implementation.  Resolution agents
-- are drawn from the children's shares, which is where the conflicts are.
cycleRefusal :: NodeSeed -> Harness (Maybe NodeWork)
cycleRefusal seed
  | seed.seedCycles >= required = pure Nothing
  | otherwise =
      pure
        ( Just
            ( refusalWork
                seed
                ( Failure
                    BudgetSpent
                    [fmt|{nodeName seed.seedPlan} needs {required} agent cycles, {seed.seedCycles} left in this subtree|]
                    []
                )
            )
        )
  where
    required = requiredCycles seed.seedPlan

-- | 'Swarm.capped''s slot.  A leaf at the depth limit is not capped — there
-- was nothing to unfold — which is exactly why the slot returns a 'Maybe'
-- rather than the wrapper deciding on depth alone.
depthRefusal :: NodeSeed -> Harness (Maybe NodeWork)
depthRefusal seed
  | null (childPlans seed.seedPlan) = pure Nothing
  | otherwise =
      pure
        ( Just
            ( refusalWork
                seed
                (Failure DepthCapped [fmt|depth cap reached at {nodeName seed.seedPlan}|] [])
            )
        )

-- | 'Swarm.gated''s slot: TIERED, and the reason the slots are effectful.
-- Tier 1 is a deterministic width heuristic and costs nothing.  Tier 2 hands
-- the operator a typed form with this layer's real child names attached — the
-- parent's scaffold has already landed by the time it runs, so the approval is
-- about work that exists rather than a speculative whole-tree sign-off.
layerGate :: Budget -> Swarm.PlanF NodeWork NodeSeed -> Harness (Maybe NodeWork)
layerGate b layer
  | length childSeeds <= b.gateWiderThan = pure Nothing
  | otherwise = do
      say [fmt|{nodeName parent.seedPlan} proposes {length childSeeds} children (gate is {b.gateWiderThan})|]
      approval <- askUser @LayerApproval
      pure $
        if approval.layerApproved
          then Nothing
          else
            Just
              ( refusalWork
                  parent
                  (Failure LayerRefused approval.approvalNote (map (nodeName . seedPlan) childSeeds))
              )
  where
    childSeeds = Swarm.kids layer
    parent = (Swarm.task layer).workSeed

-- | Seed one child per plan, in plan order.
--
-- HOW a child's worktree is obtained is a parameter, and that is the whole of
-- what resume changes here: a fresh unfold creates one from the parent's
-- CURRENT state (which by the ordering above is the scaffold worker's final
-- commit), while a replayed split REBINDS the retained tree the journal names.
-- Everything else — the allowance division, the plan ordering, a denial riding
-- on as data rather than failing the split — is one implementation either way.
allocateChildren
  :: NodeSeed
  -> [DevPlan]
  -> (DevPlan -> Harness (Either Text WorktreeHandle))
  -> Harness ([NodeSeed], [Text])
allocateChildren parent kids obtain = go kids
  where
    allowance = childAllowance parent (length kids)
    go [] = pure ([], [])
    go (k : rest) =
      obtain k >>= \case
        Left why -> do
          (seeds, denied) <- go rest
          pure (seeds, [fmt|{nodeName k}: {why}|] : denied)
        Right childTree -> do
          (seeds, denied) <- go rest
          let s =
                NodeSeed
                  { seedPlan = k
                  , seedTree = childTree
                  , seedDepth = parent.seedDepth + 1
                  , seedCycles = allowance
                  , seedAdopted = Nothing
                  }
          pure (s : seeds, denied)

-- | A child that has never existed: a new worktree off the parent's HEAD.
freshChild :: NodeSeed -> DevPlan -> Harness (Either Text WorktreeHandle)
freshChild parent k =
  createWorktree (fromWorktree parent.seedTree (nodeName k)) >>= \case
    Left err -> pure (Left (renderWorktreeError err))
    Right h -> pure (Right h)

-- | A child of a REPLAYED split: rebind the retained worktree the journal
-- names for it, and fall back to creating one only for a child the crash
-- caught before it was ever allocated.  Nothing is recreated and nothing is
-- deleted (PRD 19).
retainedChild :: NodeSeed -> [(Text, Text)] -> DevPlan -> Harness (Either Text WorktreeHandle)
retainedChild parent trees k = case lookup (nodeName k) trees of
  Nothing -> freshChild parent k
  Just branch -> fmap retainedHandle <$> retainWorktree branch

-- | Divide what is left after this node's own reservation among its children.
--
-- Conservative on purpose: a subtree that finishes under its share does not
-- return the remainder to its siblings.  That is the honest cost of enforcing
-- a budget with no shared mutable state in the row — and the division is
-- deterministic, so no scheduling order can change it.  NEVER clamped
-- upward: a share that floors to zero stays zero, so a node that cannot fund
-- every child hands the underfunded ones nothing rather than minting cycles
-- the parent doesn't have — 'cycleRefusal' turns that zero into a typed
-- budget refusal for that child instead of an overspend.
--
-- Delegates the arithmetic to 'Swarm.splitAllowance' (operator's type-level
-- review, 2026-08-17): every child gets the same floor share that combinator
-- computes, and its conservation law — property-tested in the
-- @thought-driver-test@ suite's @SwarmSpec@, not re-derived here — is what
-- now guarantees no call site can mint a cycle from nothing, in place of the
-- old hand-rolled @max 0 (... ) \`div\` n@ this function used to carry
-- directly. `Swarm.mkCycles`/`Swarm.cyclesToInt` are the boundary: dev-tree's
-- own budget vocabulary ('NodeSeed.seedCycles', 'requiredCycles') stays
-- plain 'Int' — only this one call site speaks 'Swarm.Cycles'.
childAllowance :: NodeSeed -> Int -> Int
childAllowance parent n =
  case Swarm.splitAllowance (Swarm.mkCycles parent.seedCycles) (Swarm.mkCycles (requiredCycles parent.seedPlan)) n of
    (_, s : _) -> Swarm.cyclesToInt s
    (_, []) -> 0

-- ---------------------------------------------------------------------------
-- The algebra — how to combine
-- ---------------------------------------------------------------------------

-- | Fold one node.  Its children's outcomes arrive in PLAN order (never
-- completion order), and a failed child arrives as an ordinary value: nothing
-- here short-circuits, because @traverse@ already visited every sibling.
--
-- Every outcome this function returns has an EMPTY trail and an unjudged
-- receipt.  Filling the trail and applying the trust ladder both belong to
-- 'stampFold' — the 'Swarm.receipted' middleware — which is the one place with
-- this node's line and its children's trails in hand, and therefore the one
-- place a leaf fold and an interior fold cannot drift apart.
integrate :: Swarm.PlanF NodeWork Outcome -> Harness Outcome
integrate (Swarm.PlanF w kids) = case w of
  -- A subtree the journal already accounts for is not re-entered: the recorded
  -- (or adopted-and-verified) receipt IS this fold's input.
  WorkResumed {workOutcome = ReplayedOutcome o} -> pure o
  WorkResumed {workOutcome = AdoptedOutcome o} -> pure o
  WorkRefused {workSeed = seed, workFailure = f} ->
    pure Skipped {outcomeNode = nodeName seed.seedPlan, outcomeTrail = [], skipReason = renderFailure f}
  WorkReady {workSeed = seed, workKids = wkids, workDenied = denied} -> case kids of
    [] -> leafFold seed
    _ -> interiorFold seed wkids denied kids

-- | 'Swarm.receipted''s slot, and the whole trust ladder in one place.
--
-- A HIGHER RUNG NEVER OVERRIDES A FAILING LOWER RUNG: 'foldLadder' is an
-- ordered case over the receipt, so an agent's green summary over a red check
-- is a red node.  Evidence is journaled either way — a fold that failed its
-- ladder is exactly the fold whose evidence someone will want.
--
-- The ladder is applied UNIFORMLY, including to a replayed outcome: the
-- journal records the fold's raw evidence (@journalOutcome@ runs before
-- @foldLadder@), so a resumed run re-judges the same receipt by the same rungs
-- rather than trusting a verdict it did not compute.
stampFold :: Swarm.PlanF NodeWork Outcome -> Outcome -> Harness Outcome
stampFold node folded = do
  case Swarm.task node of
    -- Already in the journal.  The log is append-only, so re-appending an
    -- outcome a prior process recorded would be duplicate noise on every
    -- resume — and idempotence is the point: resuming a finished run does
    -- nothing at all.
    WorkResumed {workOutcome = ReplayedOutcome _} -> pure ()
    -- ADOPTION APPENDS.  The crashed process never got to record this one, so
    -- writing it now is what makes the next resume skip the subtree.
    _ -> journalOutcome folded
  pure (withTrail (concatMap outcomeTrailOf (Swarm.kids node)) (foldLadder folded))

journalOutcome :: Outcome -> Harness ()
journalOutcome o = recordEvent (OutcomeEvent (JournalKey (outcomeJournalKey o)) o)

-- | The key 'journalOutcome' files an outcome under: the branch when a
-- receipt is in hand (a 'Done', or a 'Failed' that still carries a partial
-- one), the plan node name otherwise. Mirrors "HarnessTypes"'s
-- @outcomeNodeName@ exactly, except that a receipted outcome prefers its
-- receipt's own branch over its node name — the same precedence
-- 'journalOutcome' always applied, now named.
outcomeJournalKey :: Outcome -> Text
outcomeJournalKey o = case o of
  Done {doneReceipt = r} -> r.receiptBranch
  Failed {partialReceipt = Just r} -> r.receiptBranch
  Failed {outcomeNode = n, partialReceipt = Nothing} -> n
  Skipped {outcomeNode = n} -> n

-- | The ladder, computed from the receipt rather than claimed by the folder.
-- Rung 1 is the repository (an agent cycle that moved no HEAD; a diff outside
-- the declared boundary), rung 2 is the orchestrator's own checks at the fold
-- sha.  Rung 3 has its slot ('receiptReviewed') and is honestly 'False'.
foldLadder :: Outcome -> Outcome
foldLadder o = case o of
  Done {outcomeNode = n, doneReceipt = r}
    | r.receiptAgentRan && not r.receiptHeadMoved ->
        failedOutcome n (Failure NoHeadMove [fmt|{n} ran an agent cycle but HEAD never moved|] []) (Just r)
    | not (null r.receiptOutside) ->
        failedOutcome n (Failure BoundaryViolated [fmt|{n} changed paths outside its boundary|] r.receiptOutside) (Just r)
    | not (null (failing r)) ->
        failedOutcome
          n
          ( Failure
              ChecksFailed
              [fmt|{length (failing r)} of {length r.receiptChecks} checks failed at {r.receiptHead}|]
              (map checkCommand (failing r))
          )
          (Just r)
    | otherwise -> o
  Failed {} -> o
  Skipped {} -> o
  where
    failing r = filter checkFailed r.receiptChecks

-- | A leaf: one implementation worker, then the ladder.
--
-- Rung 1 is the HEAD read either side of the cycle — a worker that claims
-- completion without committing is caught here and never reaches rung 2.  The
-- 'withHandler' scope is observation of the same fact as it happens; the pair
-- of 'worktreeHead' reads is what closes the window a subscription
-- deliberately will not (no replay, cycle-scoped lifetime).
leafFold :: NodeSeed -> Harness Outcome
leafFold seed = do
  before <- worktreeHead tree
  runWorker tree name (workerPrompt p) >>= \case
    Left err ->
      pure (failedOutcome name (Failure SpawnDenied (renderSpawnError err) []) Nothing)
    Right wr -> do
      after <- worktreeHead tree
      checks <- runChecks tree p
      finishFold seed wr (before, after) [] [] 1 True checks
  where
    tree = seed.seedTree
    p = seed.seedPlan
    name = nodeName p

-- | An interior node: the eager rebase cascade, the merges, then the ladder.
--
-- A conflict-free fold spends ZERO agent cycles — mechanical git IS the
-- integration tier, so the fast-forward question dissolves into it.  The
-- integration agent is spawned only when the mechanical tier left something
-- for it: an escalation, or a check that fails at the merged head.
interiorFold :: NodeSeed -> [NodeSeed] -> [Text] -> [Outcome] -> Harness Outcome
interiorFold seed workKids denied kids = do
  before <- worktreeHead tree
  acc <- foldChildren tree p (zip workKids kids) emptyAcc {accEsc = deniedEsc}
  checks0 <- runChecks tree p
  let needsAgent = not (null acc.accEsc) || any checkFailed checks0
  (wr, agentCycles, agentRan) <-
    if not needsAgent
      then pure (mechanicalResult acc, 0, False)
      else
        spawnIntegration tree p acc checks0 >>= \case
          Left err ->
            pure
              ( mechanicalResult acc
                  {accEsc = acc.accEsc <> [[fmt|integration spawn failed: {renderSpawnError err}|]]}
              , 0
              , False
              )
          Right merged -> pure (merged, 1, True)
  checks <- if agentRan then runChecks tree p else pure checks0
  after <- worktreeHead tree
  folded <-
    finishFold
      seed
      wr
      (before, after)
      acc.accNotes
      (maybeToList acc.accAbandon <> acc.accEsc)
      (acc.accCycles + agentCycles)
      agentRan
      checks
  -- An abandoned subtree is the one node-local verdict the receipt cannot
  -- carry: the evidence is fine as far as it goes, and what failed is that a
  -- policy chose to stop.  Everything else this fold is worth is 'foldLadder''s.
  pure $ case (acc.accAbandon, folded) of
    (Just why, Done {doneReceipt = r}) ->
      failedOutcome (nodeName p) (Failure ChildrenFailed why []) (Just r)
    _ -> folded
  where
    tree = seed.seedTree
    p = seed.seedPlan
    deniedEsc = map ("child worktree denied — " <>) denied

-- | Walk the children in PLAN order: merge the ones that are done, cascade the
-- new parent tip to every sibling still ahead of us, and carry everything else
-- forward as data.
foldChildren
  :: WorktreeHandle
  -> DevPlan
  -> [(NodeSeed, Outcome)]
  -> FoldAcc
  -> Harness FoldAcc
foldChildren _ _ [] acc = pure acc
foldChildren tree p ((s, o) : rest) acc = case acc.accAbandon of
  Just _ ->
    -- Abandoned: the remaining siblings are not merged, and saying so is the
    -- record.  Their branches survive in retained worktrees either way.
    foldChildren tree p rest acc {accEsc = acc.accEsc <> [[fmt|{childName}: not merged (subtree abandoned)|]]}
  Nothing
    | not (outcomeIsDone o) -> do
        next <- onChildFailure tree p s o acc
        foldChildren tree p rest next
    | otherwise ->
        mergeChild tree p s >>= \case
          Left why -> do
            next <- escalate p s why acc
            foldChildren tree p rest next
          Right note -> do
            newHead <- worktreeHead tree
            -- EAGER: the fold just moved this node's HEAD, so every sibling
            -- tip ahead of us is stale RIGHT NOW, not at integration time.
            -- Only the siblings that will actually be merged — rebasing a
            -- branch this node has already decided not to fold would spend a
            -- resolution cycle on work nobody is going to use.
            let ahead = [sib | (sib, out) <- rest, outcomeIsDone out]
            cascaded <- cascade p newHead ahead acc {accNotes = acc.accNotes <> [note], accMerged = acc.accMerged + 1}
            foldChildren tree p rest cascaded
  where
    childName = nodeName s.seedPlan

-- | A child that failed on its own terms.  The parent's policy decides whether
-- that stops the fold; 'Replan' opens a planning window and JOURNALS the
-- amendment, because re-unfolding a subtree means re-entering the coalgebra —
-- which is resume's job (PRD 20 S1-L5), not this fold's.
onChildFailure :: WorktreeHandle -> DevPlan -> NodeSeed -> Outcome -> FoldAcc -> Harness FoldAcc
onChildFailure _ p s o acc = case nodeOnFailure p of
  Abandon -> pure acc {accAbandon = Just why, accEsc = acc.accEsc <> [why]}
  Replan -> do
    decision <- runLLMTurn @ReplanDecision (replanPrompt p s why)
    recordEvent (ReplanEvent (JournalKey (branchOf s.seedTree)) decision)
    pure $
      if decision.abandonSubtree
        then acc {accAbandon = Just (why <> " — replan abandoned"), accEsc = acc.accEsc <> [why]}
        else acc {accEsc = acc.accEsc <> [[fmt|{why} — replanned: {decision.amendedInstruction}|]]}
  AskOperator ->
    askUser @Triage >>= \t -> case t.triageAction of
      TriageAbandon -> pure acc {accAbandon = Just (why <> " — operator abandoned"), accEsc = acc.accEsc <> [why]}
      _ -> pure acc {accEsc = acc.accEsc <> [[fmt|{why} — operator: {t.triageNote}|]]}
  Retry -> pure acc {accEsc = acc.accEsc <> [why]}
  where
    why = [fmt|{outcomeNodeName o}: {failureText o}|]

failureText :: Outcome -> Text
failureText o = case o of
  Done {} -> "done"
  Failed {outcomeFailure = f} -> renderFailure f
  Skipped {skipReason = r} -> r

-- ---------------------------------------------------------------------------
-- The eager rebase cascade: mechanical, then cognition, then escalation
-- ---------------------------------------------------------------------------

-- | Bring every still-live sibling tip onto this node's new HEAD.
--
-- Tier 1 runs for all of them first and costs nothing when it works.  Tier 2
-- then spawns a resolution agent per CONFLICTED tip — all of them at once,
-- through 'spawnAsync', because they are independent worktrees and there is no
-- reason to serialize inference.  They are awaited in PLAN order, so
-- completion order is not an input to any decision here.  Tier 3 is
-- 'escalate': a typed value the parent's policy reads, never an exception.
--
-- Convergence: the task is always "rebase onto the parent's CURRENT tip", so
-- arrival order changes how much work a rebase does, never where it ends up.
cascade :: DevPlan -> GitOid -> [NodeSeed] -> FoldAcc -> Harness FoldAcc
cascade p onto seeds acc0 = do
  attempts <- traverse (mechanicalRebase onto) seeds
  let clean = [note | (_, Right note) <- attempts]
      conflicted = [s | (s, Left _) <- attempts]
  handles <- traverse (spawnResolution onto) conflicted
  awaitResolutions p onto (zip conflicted handles) acc0 {accNotes = acc0.accNotes <> clean}

-- | Tier 1.  A tip the new head is already an ancestor of needs nothing at all
-- ('RebaseCurrent'); otherwise plain @git rebase@, aborted on any nonzero exit
-- so a conflicted worktree is never left mid-rebase for the next tier.
mechanicalRebase :: GitOid -> NodeSeed -> Harness (NodeSeed, Either Text RebaseNote)
mechanicalRebase onto s =
  gitIn tree [fmt|merge-base --is-ancestor {ontoText} HEAD|] >>= \case
    Left e -> pure (s, Left e)
    Right ancestry
      | ok ancestry -> pure (s, Right (RebaseNote branch ontoText RebaseCurrent))
      | otherwise ->
          gitIn tree [fmt|rebase {ontoText}|] >>= \case
            Left e -> pure (s, Left e)
            Right pr
              | ok pr -> do
                  recordEvent (RebaseEvent (JournalKey branch) (RebaseNote branch ontoText RebaseClean))
                  pure (s, Right (RebaseNote branch ontoText RebaseClean))
              | otherwise -> do
                  _ <- gitIn tree "rebase --abort"
                  pure (s, Left (firstLine pr.stderr))
  where
    tree = s.seedTree
    branch = branchOf tree
    ontoText = renderGitOid onto

-- | Tier 2, started.  One ephemeral agent per conflicted tip, in the worktree
-- that tip owns — isolation is unchanged, one agent per worktree.
spawnResolution :: GitOid -> NodeSeed -> Harness (Either SpawnError (AgentHandle ResolutionResult))
spawnResolution onto s =
  spawnAsync @ResolutionResult
    ( spawnSpecIn
        (worktreeId s.seedTree)
        (nodeName s.seedPlan <> "-rebase")
        (resolutionPrompt s (renderGitOid onto) Nothing)
    )

-- | Tier 2, awaited — in PLAN order, whatever order they finish in.
--
-- An abandonment reaps the tips still in flight: 'cancelAgent' is total, so a
-- handle that already finished is a no-op rather than an ordering bug.
awaitResolutions
  :: DevPlan
  -> GitOid
  -> [(NodeSeed, Either SpawnError (AgentHandle ResolutionResult))]
  -> FoldAcc
  -> Harness FoldAcc
awaitResolutions _ _ [] acc = pure acc
awaitResolutions p onto ((s, h) : rest) acc = case acc.accAbandon of
  Just _ -> do
    reapRest
    pure acc {accEsc = acc.accEsc <> [[fmt|{name}: rebase abandoned before it was awaited|]]}
  Nothing -> case h of
    Left err -> step (Left [fmt|resolution spawn failed: {renderSpawnError err}|]) 0
    Right handle ->
      awaitAgent handle >>= \case
        Left err -> step (Left [fmt|resolution cycle failed: {renderSpawnError err}|]) 1
        Right (_, rr)
          | rr.resolved -> step (Right ()) 1
          | otherwise -> step (Left [fmt|unresolved: {rr.resolutionNotes}|]) 1
  where
    name = nodeName s.seedPlan
    reapRest = traverse_ (\(_, hh) -> either (const (pure ())) cancelAgent hh) rest
    step verdict spent = do
      next <- case verdict of
        Right () -> do
          let note = RebaseNote (branchOf s.seedTree) (renderGitOid onto) RebaseResolved
          recordEvent (RebaseEvent (JournalKey (branchOf s.seedTree)) note)
          pure acc {accNotes = acc.accNotes <> [note], accCycles = acc.accCycles + spent}
        Left why -> do
          escalated <- escalate p s why acc {accCycles = acc.accCycles + spent}
          pure escalated
      case next.accAbandon of
        Just _ -> reapRest >> awaitResolutions p onto rest next
        Nothing -> awaitResolutions p onto rest next

-- | Tier 3.  An unresolved conflict is not an exception and does not stop the
-- fold: the parent's failure policy — an exhaustive case the compiler audits —
-- turns it into a retry, a planning window, an operator form, or an
-- abandonment, and whatever it decides rides on as data.
escalate :: DevPlan -> NodeSeed -> Text -> FoldAcc -> Harness FoldAcc
escalate p s why acc = do
  let note = RebaseNote branch "-" RebaseEscalation
      base = acc {accNotes = acc.accNotes <> [note]}
  recordEvent (EscalationEvent (JournalKey branch) name why)
  applyPolicy p s why >>= \case
    PolicyResolved spent ->
      pure base {accCycles = base.accCycles + spent, accEsc = base.accEsc <> [[fmt|{name}: {why} (resolved on policy retry)|]]}
    PolicyEscalated detail spent ->
      pure base {accCycles = base.accCycles + spent, accEsc = base.accEsc <> [[fmt|{name}: {detail}|]]}
    PolicyAbandoned detail spent ->
      pure
        base
          { accCycles = base.accCycles + spent
          , accEsc = base.accEsc <> [[fmt|{name}: {detail}|]]
          , accAbandon = Just [fmt|{name}: {detail}|]
          }
  where
    name = nodeName s.seedPlan
    branch = branchOf s.seedTree

-- | PRD 20's failure-policy sum, applied by deterministic code.  Cognition
-- enters through exactly two constructors: 'Replan' opens a planning window
-- scoped to the failure, 'AskOperator' presents a typed triage form.
applyPolicy :: DevPlan -> NodeSeed -> Text -> Harness PolicyOutcome
applyPolicy p s why = case nodeOnFailure p of
  Abandon -> pure (PolicyAbandoned [fmt|{why} — abandoned by policy|] 0)
  Retry -> retryOnce "Try again; the previous resolution round did not converge."
  Replan -> do
    decision <- runLLMTurn @ReplanDecision (replanPrompt p s why)
    recordEvent (ReplanEvent (JournalKey (branchOf s.seedTree)) decision)
    if decision.abandonSubtree
      then pure (PolicyAbandoned [fmt|{why} — replan abandoned: {decision.rationale}|] 0)
      else retryOnce decision.amendedInstruction
  AskOperator ->
    askUser @Triage >>= \t -> case t.triageAction of
      TriageRetry -> retryOnce t.triageNote
      TriageSkip -> pure (PolicyEscalated [fmt|{why} — operator skipped: {t.triageNote}|] 0)
      TriageAbandon -> pure (PolicyAbandoned [fmt|{why} — operator abandoned: {t.triageNote}|] 0)
  where
    retryOnce instruction =
      worktreeHead s.seedTree >>= \h ->
        spawnAgent @ResolutionResult
          ( spawnSpecIn
              (worktreeId s.seedTree)
              (nodeName s.seedPlan <> "-rebase-retry")
              (resolutionPrompt s (renderGitOid h) (Just instruction))
          )
          >>= \case
            Left err -> pure (PolicyEscalated [fmt|{why} — retry spawn failed: {renderSpawnError err}|] 1)
            Right (_, rr)
              | rr.resolved -> pure (PolicyResolved 1)
              | otherwise -> pure (PolicyEscalated [fmt|{why} — retry unresolved: {rr.resolutionNotes}|] 1)

-- | Merge one child branch into this node.  Mechanical first — a clean merge
-- is the whole integration tier at zero tokens — and aborted rather than left
-- half-applied, so the conflict is handed on with the worktree intact.
mergeChild :: WorktreeHandle -> DevPlan -> NodeSeed -> Harness (Either Text RebaseNote)
mergeChild tree p s =
  gitIn tree [fmt|merge --no-ff -m "fold {childBranch} into {nodeName p}" {childBranch}|] >>= \case
    Left e -> pure (Left e)
    Right pr
      | ok pr -> pure (Right (RebaseNote childBranch (renderBranchName tree.handleReceipt.branch) RebaseClean))
      | otherwise -> do
          _ <- gitIn tree "merge --abort"
          pure (Left [fmt|merge conflict: {firstLine pr.stderr}|])
  where
    childBranch = branchOf s.seedTree

-- ---------------------------------------------------------------------------
-- The ladder, the receipt, the journal
-- ---------------------------------------------------------------------------

-- | Gather this fold's evidence into one 'FoldReceipt' and return the fold.
--
-- Deliberately NOT the judge: it observes (HEAD either side of the cycle, the
-- boundary diff against the seed, the checks the orchestrator ran itself) and
-- records what it observed.  Whether that evidence adds up to a 'Done' is
-- 'foldLadder''s, applied uniformly by the 'Swarm.receipted' middleware — so
-- there is no path on which a fold judges its own receipt.
finishFold
  :: NodeSeed
  -> WorkerResult
  -> (GitOid, GitOid)
  -> [RebaseNote]
  -> [Text]
  -> Int
  -> Bool
  -> [CheckResult]
  -> Harness Outcome
finishFold seed wr (before, after) notes escalations cycles agentRan checks = do
  outside <- boundaryViolations tree (nodeBoundary p)
  pure
    ( Done
        name
        []
        FoldReceipt
          { receiptNode = name
          , receiptBranch = branchOf tree
          , receiptSeedHead = renderGitOid before
          , receiptHead = renderGitOid after
          , receiptHeadMoved = renderGitOid before /= renderGitOid after
          , receiptChecks = checks
          , receiptRebases = notes
          , receiptOutside = outside
          , receiptCycles = cycles
          , receiptAgentRan = agentRan
          , receiptReviewed = False
          , receiptSummary = wr.workSummary
          , receiptEvidence = wr.evidence <> escalations
          }
    )
  where
    tree = seed.seedTree
    p = seed.seedPlan
    name = nodeName p

runChecks :: WorktreeHandle -> DevPlan -> Harness [CheckResult]
runChecks tree p = traverse one (nodeChecks p)
  where
    one cmd =
      runIn tree.handleReceipt.cwd cmd >>= \case
        Left e -> pure (CheckResult cmd 127 (renderExecError e))
        Right pr -> pure (CheckResult cmd pr.exitCode (firstLine pr.stderr))

checkFailed :: CheckResult -> Bool
checkFailed c = c.checkExit /= 0

-- | The boundary is data on the plan and is checked against what git actually
-- shows, exact-or-directory-prefix.  An empty boundary means unrestricted.
boundaryViolations :: WorktreeHandle -> [Text] -> Harness [Text]
boundaryViolations _ [] = pure []
boundaryViolations tree prefixes =
  gitIn tree [fmt|diff --name-only {seedHead}..HEAD|] >>= \case
    -- A boundary check that could not RUN is a failing boundary check, never
    -- a clean one: reporting [] here would silently convert "git is broken in
    -- this worktree" into "this node stayed inside its boundary".
    Left e -> pure [[fmt|<boundary check could not run: {e}>|]]
    Right pr -> pure (filter (not . inside) (filter (not . T.null) (T.lines pr.stdout)))
  where
    seedHead = renderGitOid tree.handleReceipt.sourceHead
    inside f = any (\pre -> f == pre || (pre <> "/") `T.isPrefixOf` f) prefixes

-- ---------------------------------------------------------------------------
-- Agents and git, both through their own seam
-- ---------------------------------------------------------------------------

-- | The typed spawn: @\@WorkerResult@ is what fixes the schema the worker is
-- held to AND the type its terminal payload decodes into.  A payload that does
-- not fit comes back as @Left (SpawnResultMalformed …)@, never as a success
-- with a defaulted field.  The worktree already exists, so the spec names it by
-- id ('spawnSpecIn') rather than asking for a new one.
runWorker :: WorktreeHandle -> Text -> Text -> Harness (Either SpawnError WorkerResult)
runWorker tree name prompt =
  withHandler (headChanged tree) (noteHeadMove name) $
    spawnAgent @WorkerResult (spawnSpecIn (worktreeId tree) name prompt) <&> fmap snd

spawnIntegration
  :: WorktreeHandle -> DevPlan -> FoldAcc -> [CheckResult] -> Harness (Either SpawnError WorkerResult)
spawnIntegration tree p acc checks =
  runWorker tree (nodeName p <> "-integration") (integrationPrompt p acc checks)

-- | Repository events are authoritative; agent summaries are not.
noteHeadMove :: Text -> Observed HeadChangeReceipt -> Harness ()
noteHeadMove name change = say (name <> " HEAD -> " <> renderGitOid receipt.newHead)
  where
    receipt = value change

-- | Plain git in a worktree this node owns — authored policy, not a runtime
-- workflow verb.  PRD 19's freeze is about what the RUNTIME crates expose;
-- this is Exec.
gitIn :: WorktreeHandle -> Text -> Harness (Either Text Proc)
gitIn tree args =
  runIn tree.handleReceipt.cwd ("git " <> args) >>= \case
    Left e -> pure (Left (renderExecError e))
    Right pr -> pure (Right pr)

renderExecError :: ExecError -> Text
renderExecError e = case e of
  ExecSpawn detail -> "could not spawn: " <> detail
  ExecBadDir detail -> "bad working directory: " <> detail

branchOf :: WorktreeHandle -> Text
branchOf tree = renderBranchName tree.handleReceipt.branch

firstLine :: Text -> Text
firstLine t = case T.lines t of
  [] -> ""
  (l : _) -> l

-- | The orchestrator's OWN account of a mechanical fold.  Not a model claim
-- dressed as one: nothing here was asked of an agent, and the receipt says so.
mechanicalResult :: FoldAcc -> WorkerResult
mechanicalResult acc =
  WorkerResult
    { workSummary =
        [fmt|Mechanical fold: {acc.accMerged} child branches merged with no conflicts, {length acc.accNotes} rebase steps, 0 agent cycles.|]
    , evidence = map renderNote acc.accNotes
    , readyForIntegration = True
    }
  where
    renderNote n = [fmt|{n.rebaseBranch} onto {n.rebaseOnto}: {show n.rebaseTier}|]

-- ---------------------------------------------------------------------------
-- Resume — consuming the folded run journal (PRD 20 S1-L5)
--
-- @record@ stays WRITE-ONLY on this side: nothing below opens a file, and
-- there is no read verb anywhere in the row.  The driver folds this run's
-- journal at boot and hands the result to 'resumeLoop'; everything here is
-- interpretation of that value, plus the orchestrator's OWN git reads and
-- checks against what it finds on disk.
--
-- Three locks shape all of it.  Decomposition is cognition, so a recorded
-- split REPLAYS rather than being re-derived (a nondeterministic re-plan would
-- orphan every completed child beneath it).  Retained worktrees REBIND, never
-- recreate.  And a commit found in a retained worktree is neither redone blind
-- nor trusted blind: it is adopted only after this orchestrator's own checks
-- pass at that sha, and a failure rides on as typed data through the same
-- 'Outcome'\/'Failure' machinery the parent's 'OnFailure' policy already reads.
-- ---------------------------------------------------------------------------

-- | What the fold says about one branch.  Decided by 'resumePlanFor' BEFORE
-- any git runs, so the whole precedence question is a pure function.
data ResumePlan
  = -- | Nothing recorded for this branch: ordinary work — after
    -- adopt-and-verify on whatever its retained worktree turns out to hold.
    ResumeFresh
  | -- | An outcome stands.  This subtree is not re-entered at all; the
    -- recorded receipt IS the algebra's input.
    ResumeSkip Outcome
  | -- | A split stands.  Replay the recorded plan and its child worktrees;
    -- never re-ask the planner, never spawn the scaffold worker again.
    ResumeReplay SplitRecord
  | -- | A journaled @replan@ is the NEWEST word about this branch, so the
    -- subtree re-unfolds under the AMENDED plan rather than the original one.
    ResumeAmend ReplanDecision DevPlan
  deriving (Show, Eq)

-- | The @split@ payload, read back — built from a decoded 'SplitEvent' by
-- 'splitRecordOf'.  This schema never appears in Rust.
data SplitRecord = SplitRecord
  { splitNode         :: Text
  , splitScaffoldHead :: Text
  , splitPlan         :: DevPlan
  , -- | Child node name to the branch of the worktree it was allocated.  Empty
    -- when the crash landed between the split's two appends (see 'emitSplit').
    splitChildTrees   :: [(Text, Text)]
  }
  deriving (Show, Eq)

-- | The resume wrapper over the composed coalgebra — one @Coalg -> Coalg@, the
-- same shape every other policy in this file takes.
--
-- IDENTITY ON A FRESH RUN, structurally: an empty fold can decide nothing, so
-- the guard hands back the wrapped coalgebra untouched and a fresh run
-- performs exactly the git reads and spawns it performed before this lane
-- existed.  That is what keeps 'loop' and 'resumeLoop' one spelling of the run
-- rather than two that drift.
resumed
  :: ResumeFold
  -> Swarm.Coalg Harness NodeWork NodeSeed
  -> Swarm.Coalg Harness NodeWork NodeSeed
resumed fold inner
  | not (isResumed fold) = inner
  | otherwise = go
  where
    go seed = case resumePlanFor fold (branchOf seed.seedTree) seed.seedPlan of
      ResumeSkip o -> pure (Swarm.PlanF (replayedWork seed o) [])
      ResumeReplay sp -> replaySplit fold seed sp
      ResumeFresh -> adoptOrUnfold fold inner seed
      ResumeAmend d amended
        | d.abandonSubtree ->
            pure
              ( Swarm.PlanF
                  ( refusalWork
                      seed
                      ( Failure
                          ChildrenFailed
                          [fmt|{nodeName seed.seedPlan}: a journaled replan abandoned this subtree — {d.rationale}|]
                          []
                      )
                  )
                  []
              )
        -- Re-unfold under the amendment, through the FULL policy stack: this
        -- is fresh work, so the depth cap, the cycle budget, and the layer
        -- gate all apply to it exactly as they would on a first run.
        | otherwise -> inner seed {seedPlan = amended}

-- | The fold's verdict for one branch.  PURE — no effects, no git, no I\/O —
-- so the precedence between a recorded outcome, a recorded split, and a
-- journaled amendment is one function a test can call directly.
--
-- An @outcome@ is looked up under the branch AND under the node name because
-- 'journalOutcome' keys it either way: a fold with a receipt is keyed by its
-- branch, a receiptless failure or a skip by the node it concerns.  The two
-- namespaces do not collide (a branch carries the worktree id), so consulting
-- both is unambiguous and costs the write side nothing.
resumePlanFor :: ResumeFold -> Text -> DevPlan -> ResumePlan
resumePlanFor fold branch p
  | amendmentIsNewest (seqOf replanEntry) (seqOf splitEntry) (seqOf outcomeEntry)
  , Just (_, ReplanEvent {evDecision = d}) <- replanEntry =
      ResumeAmend d (amendPlan d (maybe p (.splitPlan) recordedSplit))
  | Just (_, OutcomeEvent {evOutcome = o}) <- outcomeEntry = ResumeSkip o
  | Just sp <- recordedSplit = ResumeReplay sp
  | otherwise = ResumeFresh
  where
    splitEntry = lookupEvent SplitKind branch fold
    replanEntry = lookupEvent ReplanKind branch fold
    outcomeEntry =
      lookupEvent OutcomeKind branch fold
        `orElse` lookupEvent OutcomeKind (nodeName p) fold
    recordedSplit = splitEntry >>= (splitRecordOf . snd)
    seqOf = fmap fst

-- | Rebuild the higher-level 'SplitRecord' 'resumePlanFor' and 'replaySplit'
-- consume from a decoded 'SplitEvent'. An absent second-append ('Nothing')
-- collapses to '[]' here, same as the pre-refactor @decodeSplit@ — the two
-- cases ("not yet allocated" and "allocated, all children denied") both mean
-- "look nothing up", so 'SplitRecord' does not need to distinguish them.
splitRecordOf :: JournalEvent -> Maybe SplitRecord
splitRecordOf SplitEvent {evSplitPlan = pl, evScaffoldHead = h, evChildTrees = childTrees} =
  Just
    SplitRecord
      { splitNode = nodeName pl
      , splitScaffoldHead = h
      , splitPlan = pl
      , splitChildTrees = fromMaybe [] childTrees
      }
splitRecordOf _ = Nothing

-- | Is the journaled amendment the newest word about this branch?
--
-- PRD 20 S1-L5's contract is "a branch with a @replan@ NEWER than its @split@
-- re-unfolds under the amended plan".  It is compared against the outcome as
-- well, for the same reason: a replan is recorded AFTER the failure it
-- answers, so an outcome older than it is not the last word either.  An
-- amended subtree therefore re-runs exactly once — its own fold records a
-- newer outcome, which is what makes the NEXT resume skip it.
amendmentIsNewest :: Maybe Int -> Maybe Int -> Maybe Int -> Bool
amendmentIsNewest replanSeq splitSeq outcomeSeq = case replanSeq of
  Nothing -> False
  Just r -> r > absent splitSeq && r > absent outcomeSeq
  where
    absent = fromMaybe (-1)

-- | Apply a journaled amendment to a node's plan.  The amendment is what a
-- fresh worker should be told INSTEAD, so it replaces the task and nothing
-- else: the checks, the boundary, the failure policy, and the children are
-- structure, not instruction.  An empty instruction amends nothing rather than
-- blanking the task.
amendPlan :: ReplanDecision -> DevPlan -> DevPlan
amendPlan d p
  | T.null (T.strip d.amendedInstruction) = p
  | otherwise = p {nodeTask = d.amendedInstruction}

-- | A split that already happened: replay it.
--
-- The ladder from the lane doc's §4 runs first, on this node's OWN worktree.
-- A HEAD still on the recorded scaffold means the integration never started.
-- A HEAD past it means the crashed process got partway — and only a provably
-- COMPLETE integration is adopted (see 'integrationComplete'); a partial one
-- is replayed, which is safe because merging an already-merged branch is a
-- no-op.
replaySplit :: ResumeFold -> NodeSeed -> SplitRecord -> Harness (Swarm.PlanF NodeWork NodeSeed)
replaySplit fold seed sp = do
  changed <- checkHeadChanged seed.seedTree sp.splitPlan sp.splitScaffoldHead
  case changed of
    Nothing -> unfoldChildren
    Just hc -> do
      finished <- integrationComplete fold seed sp
      if finished
        then do
          vo <- verifyOrphan fold hc
          pure (Swarm.PlanF (adoptedWork recorded (adopt vo)) [])
        else unfoldChildren
  where
    -- The RECORDED plan is the authority from here down, not the authored one:
    -- replaying a split means replaying what was decided, including whatever
    -- amendment produced it.
    recorded = seed {seedPlan = sp.splitPlan}
    unfoldChildren = do
      (childSeeds, denied) <-
        allocateChildren recorded (childPlans sp.splitPlan) (retainedChild recorded sp.splitChildTrees)
      pure (Swarm.PlanF (splitWork recorded Nothing childSeeds denied) childSeeds)

-- | Did the crashed process finish folding this node's children into it?
--
-- True only when every child in the recorded plan recorded a DONE outcome and
-- every one of those branches is an ancestor of this node's HEAD.  Anything
-- less — a child still unaccounted for, a merge that never landed — is a
-- PARTIAL integration, and adopting one as a completed fold would be precisely
-- the blind trust this lane refuses.
integrationComplete :: ResumeFold -> NodeSeed -> SplitRecord -> Harness Bool
integrationComplete fold seed sp
  | length doneBranches /= length (childPlans sp.splitPlan) = pure False
  | otherwise = and <$> traverse isAncestor doneBranches
  where
    doneBranches =
      [ branch
      | k <- childPlans sp.splitPlan
      , Just branch <- [lookup (nodeName k) sp.splitChildTrees]
      , recordedDone fold branch (nodeName k)
      ]
    isAncestor b =
      gitIn seed.seedTree [fmt|merge-base --is-ancestor {b} HEAD|] >>= \case
        Left _ -> pure False
        Right pr -> pure (ok pr)

recordedDone :: ResumeFold -> Text -> Text -> Bool
recordedDone fold branch node =
  case lookupEvent OutcomeKind branch fold `orElse` lookupEvent OutcomeKind node fold of
    Just (_, OutcomeEvent {evOutcome = o}) -> outcomeIsDone o
    _ -> False

-- | Nothing is recorded for this branch — but its worktree need not be empty.
-- A crash between an agent's commit and the record of it leaves work with no
-- journal entry at all, and that is the case this exists for.
--
-- A HEAD still on the tree's seed means genuinely unstarted, and the ordinary
-- coalgebra runs.  A HEAD past it means orphaned work, and what the work MEANS
-- depends on the plan:
--
-- * A LEAF's commit is its whole fold, so the receipt goes to the algebra
--   UNJUDGED and 'foldLadder' — applied by the 'Swarm.receipted' stamp —
--   decides, on the same rungs as a fold this process performed.
-- * An INTERIOR node's commit is an orphaned SCAFFOLD, not an orphaned
--   outcome: its children still have to run.  A verified scaffold is unfolded
--   FROM (through the full policy stack, so the layer gate still sees this
--   layer); a rejected one rides on as data.
adoptOrUnfold
  :: ResumeFold
  -> Swarm.Coalg Harness NodeWork NodeSeed
  -> NodeSeed
  -> Harness (Swarm.PlanF NodeWork NodeSeed)
adoptOrUnfold fold inner seed = do
  changed <- checkHeadChanged seed.seedTree seed.seedPlan baseline
  case changed of
    Nothing -> inner seed
    Just hc -> do
      vo <- verifyOrphan fold hc
      let o = adopt vo
      if null (childPlans seed.seedPlan)
        then pure (Swarm.PlanF (adoptedWork seed o) [])
        else case foldLadder o of
          Done {} -> inner seed {seedAdopted = Just hc.hcFound}
          rejected -> pure (Swarm.PlanF (adoptedWork seed rejected) [])
  where
    baseline = renderGitOid seed.seedTree.handleReceipt.sourceHead

-- ---------------------------------------------------------------------------
-- The worktree-adoption typestate
--
-- Three steps, three types, and only the last one can be adopted:
--
-- 1. 'RetainedWorktree' — a handle PROVEN to have come back through the
--    registry's own rebind (PRD 19's rebind-never-recreate rule), minted
--    only by 'retainWorktree'.
-- 2. 'HeadChanged' — a retained tree whose HEAD has been read and found to
--    differ from a baseline sha, i.e. an orphan CANDIDATE. Minted only by
--    'checkHeadChanged'.
-- 3. 'VerifiedOrphan' — a 'HeadChanged' candidate run through this
--    orchestrator's OWN checks and boundary diff at its found sha. Minted
--    only by 'verifyOrphan', and the ONLY thing 'adopt' accepts.
--
-- "Never adopt without verification" is therefore not a discipline this file
-- has to remember at every call site — an 'Outcome' from orphaned work has no
-- expression here except by way of 'adopt', and 'adopt' has no other input.
-- ---------------------------------------------------------------------------

-- | A worktree handle proven to have come back through the retained-worktree
-- registry's own rebind, not freshly created and not conjured. The ONE mint
-- point is 'retainWorktree'.
newtype RetainedWorktree = RetainedWorktree WorktreeHandle

retainedHandle :: RetainedWorktree -> WorktreeHandle
retainedHandle (RetainedWorktree h) = h

-- | Look a retained worktree up by BRANCH — its durable identity in the
-- journal — and hand back a proof-carrying handle.
--
-- REBIND, NEVER RECREATE (PRD 19).  A tree a human removed by hand comes back
-- as a failure here and is surfaced as data by every caller; it is never
-- silently replaced, and nothing on this path deletes anything.
retainWorktree :: Text -> Harness (Either Text RetainedWorktree)
retainWorktree branch = do
  trees <- listWorktrees
  case [s | s <- trees, renderBranchName s.summaryReceipt.branch == branch] of
    [] -> pure (Left [fmt|no retained worktree is registered for branch {branch}|])
    (s : _)
      | not s.present ->
          pure
            ( Left
                [fmt|retained worktree {renderWorktreeId s.summaryReceipt.treeId} for {branch} is gone from disk (WorktreeLost); it is never recreated|]
            )
      | otherwise ->
          lookupWorktree s.summaryReceipt.treeId >>= \case
            Left err -> pure (Left (renderWorktreeError err))
            Right h -> pure (Right (RetainedWorktree h))

-- | A retained tree whose HEAD has been read and found to differ from a
-- baseline sha — an orphan CANDIDATE, not yet trusted at all. Minted only by
-- 'checkHeadChanged'; 'Nothing' when the tree's HEAD is still on the
-- baseline, i.e. genuinely unstarted rather than orphaned.
data HeadChanged = HeadChanged
  { hcTree     :: WorktreeHandle
  , hcPlan     :: DevPlan
  , hcBaseline :: Text
  , hcFound    :: GitOid
  }

checkHeadChanged :: WorktreeHandle -> DevPlan -> Text -> Harness (Maybe HeadChanged)
checkHeadChanged tree p baseline = do
  found <- worktreeHead tree
  pure $
    if renderGitOid found == baseline
      then Nothing
      else Just HeadChanged {hcTree = tree, hcPlan = p, hcBaseline = baseline, hcFound = found}

-- | A 'HeadChanged' candidate run through this orchestrator's OWN checks and
-- boundary diff at its found sha. Deliberately not a judgement on its own —
-- it observes and stamps a 'FoldReceipt'; 'foldLadder' (applied uniformly by
-- 'stampFold') decides what the evidence is worth. The checks themselves are
-- byte-identical to what this function ran before the typestate existed;
-- only the RESULT is now a capability rather than a plain 'Outcome'.
newtype VerifiedOrphan = VerifiedOrphan Outcome

verifyOrphan :: ResumeFold -> HeadChanged -> Harness VerifiedOrphan
verifyOrphan fold hc = do
  checks <- runChecks tree p
  outside <- boundaryViolations tree (nodeBoundary p)
  pure
    ( VerifiedOrphan
        ( Done
            name
            []
            FoldReceipt
              { receiptNode = name
              , receiptBranch = branch
              , receiptSeedHead = hc.hcBaseline
              , receiptHead = renderGitOid hc.hcFound
              , receiptHeadMoved = True
              , receiptChecks = checks
              , receiptRebases = case lookupEvent RebaseKind branch fold of
                  Just (_, RebaseEvent {evNote = n}) -> [n]
                  _ -> []
              , receiptOutside = outside
              , -- The cycles were spent by a process that is gone; this run
                -- did not spend them and does not claim them.
                receiptCycles = 0
              , receiptAgentRan = True
              , receiptReviewed = False
              , receiptSummary =
                  [fmt|Adopted work found in this retained worktree at {renderGitOid hc.hcFound}: run {fold.resumeRunId} left it there and crashed before recording an outcome.|]
              , receiptEvidence =
                  [fmt|orphaned commits {hc.hcBaseline}..{renderGitOid hc.hcFound}, verified by this orchestrator at that sha|]
                    : priorEscalationsFor fold branch
              }
        )
    )
  where
    tree = hc.hcTree
    p = hc.hcPlan
    name = nodeName p
    branch = branchOf tree

-- | Deliver a verified orphan's outcome — the ONLY function that unwraps
-- 'VerifiedOrphan'.
adopt :: VerifiedOrphan -> Outcome
adopt (VerifiedOrphan o) = o

priorEscalationsFor :: ResumeFold -> Text -> [Text]
priorEscalationsFor fold branch = case lookupEvent EscalationKind branch fold of
  Just (_, EscalationEvent {evEscDetail = why}) -> [[fmt|prior escalation: {why}|]]
  _ -> []

-- | The retained root worktree's branch, named structurally by the fold.
--
-- A @split@ is keyed by the branch and carries the node name in its payload,
-- and an @outcome@ that has a receipt is keyed by the branch too — so the root
-- is whichever entry claims the root plan's node name.  Nothing here
-- reconstructs a branch from a worktree label.
rootBranchOf :: ResumeFold -> Text -> Maybe Text
rootBranchOf fold node =
  listToMaybe
    ( [key | (key, _, SplitEvent {evSplitPlan = pl}) <- eventsOfKind SplitKind fold, nodeName pl == node]
        <> [ key
           | (key, _, OutcomeEvent {evOutcome = o}) <- eventsOfKind OutcomeKind fold
           , outcomeNodeName o == node
           , -- A receiptless failure or a skip is keyed by the NODE name, and
             -- that is not a branch.
             key /= node
           ]
    )

replayedWork :: NodeSeed -> Outcome -> NodeWork
replayedWork seed o = WorkResumed {workSeed = seed, workOutcome = ReplayedOutcome o}

adoptedWork :: NodeSeed -> Outcome -> NodeWork
adoptedWork seed o = WorkResumed {workSeed = seed, workOutcome = AdoptedOutcome o}

orElse :: Maybe a -> Maybe a -> Maybe a
orElse (Just a) _ = Just a
orElse Nothing b = b

-- ---------------------------------------------------------------------------
-- Run summary
-- ---------------------------------------------------------------------------

-- | The run's account of itself.  On a RESUMED run the prior process's
-- journaled rebase steps and escalations are carried in ahead of this
-- process's own trail, so the summary describes the whole RUN rather than
-- only the process that happened to finish it.
summarize :: ResumeFold -> Outcome -> Harness RunSummary
summarize fold root = do
  trees <- listWorktrees
  pure
    RunSummary
      { runRoot = outcomeNodeName root
      , runStatus = if outcomeIsDone root then "done" else failureText root
      , runTrail = priorTrail fold <> outcomeTrailOf root
      , runEscalations = priorEscalations fold <> escalationsOf root
      , retainedWorktrees =
          [renderWorktreeId s.summaryReceipt.treeId | s <- trees, s.present]
      }

-- | What the journal says a prior process of this run already did.
priorTrail :: ResumeFold -> [Text]
priorTrail fold
  | not (isResumed fold) = []
  | otherwise =
      [fmt|resumed run {fold.resumeRunId}: {length fold.resumeEntries} journaled steps folded in|]
        : [rebaseLine n | (_, _, RebaseEvent {evNote = n}) <- eventsOfKind RebaseKind fold]
  where
    rebaseLine n = [fmt|prior process: rebased {n.rebaseBranch} onto {n.rebaseOnto} ({show n.rebaseTier})|]

priorEscalations :: ResumeFold -> [Text]
priorEscalations fold =
  [ [fmt|prior process — {n}: {why}|]
  | (_, _, EscalationEvent {evEscNode = n, evEscDetail = why}) <- eventsOfKind EscalationKind fold
  ]

escalationsOf :: Outcome -> [Text]
escalationsOf o = case o of
  Done {doneReceipt = r} -> escalationLines r
  Failed {partialReceipt = Just r} -> escalationLines r
  Failed {partialReceipt = Nothing} -> []
  Skipped {} -> []
  where
    escalationLines r =
      [ [fmt|{n.rebaseBranch}: escalated|]
      | n <- r.receiptRebases
      , n.rebaseTier == RebaseEscalation
      ]

-- ---------------------------------------------------------------------------
-- Prompts — the one place typed orchestration becomes prose
-- ---------------------------------------------------------------------------

workerPrompt :: DevPlan -> Text
workerPrompt p = [fmt|
  You are the implementation worker for leaf node {nodeName p}.
  Work only in the assigned worktree, using your native edit, shell, test, and
  Git tools.

  Task: {nodeTask p}

  Your orchestrator will run these checks itself, in this worktree, at whatever
  commit you leave HEAD on — they are the record, not your summary of them:
{checkLines p}

  Your diff must stay inside these paths (empty means unrestricted); the
  orchestrator diffs your branch against its seed and refuses a fold that
  strays:
{boundaryLines p}

  Inspect the repository before editing. Keep your branch buildable and commit
  coherent progress — the commits are what your parent integrates, and the
  repository events they raise are the authoritative record of your work.
  Do not merely claim Git work: perform it, and cite the evidence.

  Finish your turn with a WorkerResult: a one-paragraph workSummary, an
  evidence list (commands run, checks passed, commits made), and
  readyForIntegration.
|]

scaffoldPrompt :: DevPlan -> [DevPlan] -> Text
scaffoldPrompt p kids = [fmt|
  You are the SCAFFOLD worker for node {nodeName p}. Your commit is the seam
  every child below you will be seeded from, so it lands before any child
  worktree exists.

  Task: {nodeTask p}

  These children will fork from the HEAD you leave behind. Write the shared
  types, stubs, and module boundaries they will need; do not implement their
  work:
{childLines}

  The orchestrator runs these checks in this worktree afterwards:
{checkLines p}

  Finish your turn with a WorkerResult describing the seam you left.
|]
  where
    childLines =
      T.intercalate "\n" ["  - " <> nodeName k <> ": " <> nodeTask k | k <- kids]

integrationPrompt :: DevPlan -> FoldAcc -> [CheckResult] -> Text
integrationPrompt p acc checks = [fmt|
  You are the integration worker for node {nodeName p}.

  The orchestrator already merged what it could MECHANICALLY: {acc.accMerged}
  child branches folded cleanly. It is calling you because the mechanical tier
  left something behind.

  Escalations:
{escLines}

  Checks failing at the current HEAD:
{failLines}

  Inspect every child diff and its test evidence, finish the integration with
  your native Git tools, resolve remaining conflicts by understanding both
  implementations, run the combined checks, and commit the integrated result.
  Never discard a child's work merely to make the merge easy.

  The orchestrator re-runs the checks itself after your cycle, at whatever
  commit you leave HEAD on.

  Finish your turn with a WorkerResult describing what you merged and what you
  ran.
|]
  where
    escLines = bulletLines acc.accEsc
    failLines = bulletLines [c.checkCommand <> " (exit " <> show c.checkExit <> ")" | c <- checks, checkFailed c]

resolutionPrompt :: NodeSeed -> Text -> Maybe Text -> Text
resolutionPrompt s onto amendment = [fmt|
  You are an ephemeral rebase-resolution worker for node {nodeName s.seedPlan}.

  Rebase this worktree's branch onto {onto} with your native Git tools. The
  mechanical attempt conflicted and was aborted, so the worktree is clean and
  the rebase is yours to drive from the start.

  Preserve both sides' intent: the base moved because a sibling's work landed,
  and this branch's own commits are not negotiable either. Resolve by
  understanding both, not by taking one side wholesale.
{amendmentBlock}
  Finish your turn with a ResolutionResult: whether you resolved it, what you
  did, and the paths that conflicted.
|]
  where
    amendmentBlock = case amendment of
      Nothing -> "" :: Text
      Just a -> "\n  Additional instruction from the orchestrator: " <> a <> "\n"

replanPrompt :: DevPlan -> NodeSeed -> Text -> Text
replanPrompt p s why = [fmt|
  A child of node {nodeName p} failed and its failure policy is Replan.

  Child: {nodeName s.seedPlan}
  Child task: {nodeTask s.seedPlan}
  Failure: {why}

  Decide what the orchestrator should do about this ONE subtree. Answer with a
  ReplanDecision: an amendedInstruction (what a fresh worker should be told
  instead), abandonSubtree (true when no instruction would help), and a short
  rationale. The amendment is journaled either way — a resumed run reads it
  rather than re-asking you.
|]

checkLines :: DevPlan -> Text
checkLines p = bulletLines (nodeChecks p)

boundaryLines :: DevPlan -> Text
boundaryLines p = bulletLines (nodeBoundary p)

bulletLines :: [Text] -> Text
bulletLines [] = "  (none)"
bulletLines xs = T.intercalate "\n" (map ("  - " <>) xs)
