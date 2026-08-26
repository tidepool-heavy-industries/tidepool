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
-- data third.
--
-- __Resume.__ The run journal this file writes at every split, outcome,
-- replan, rebase, and escalation is read back by 'resumeLoop' — the opt-in
-- second entry point this module gives a harness that wants to survive a
-- crash.  'loop' IS @resumeLoop emptyResume@, so a fresh run and a resumed run
-- are one spelling of the run rather than two that can drift, and a fold with
-- no entries takes the ordinary path by construction ('resumed' is @id@ when
-- 'isResumed' is false).
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
  , proposalViolation
  , sprintOverlap
    -- * Agent-cycle budget (pure — exercised directly by the overspend pin)
  , NodeSeed (..)
  , requiredCycles
  , childAllowance
  ) where

import Chore (choreBudget, choreGoal, choreMode, chorePlan, choreSnapshotDirtySource)
import DevTreeJournal
  ( JournalEvent (..)
  , JournalKey (..)
  , JournalKind (..)
  , eventsOfKind
  , lookupEvent
  , recordEvent
  )
import HarnessTypes
import Tidepool.Effects
  ( WorktreeHandle (..)
  , say
  )
import Tidepool.Aeson (object, (.=))
import Tidepool.Form (askUser)
import Tidepool.Journal (trace)
import Tidepool.Shell (runInTry)
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
import Tidepool.Async (mapConcurrently)
import qualified Data.Text as T
import Prompts (proposePrompt)
import Micro (NodeSeed (..))
import Fold
import Resume
  ( ResumePlan (..)
  , SplitRecord (..)
  , adoptedWork
  , amendPlan
  , amendmentIsNewest
  , rescuePending
  , resumePlanFor
  , resumed
  , retainWorktree
  , retainedHandle
  , rootBranchOf
  , ResumeHooks (..)
  )
import Unfold
  ( allocateChildren
  , childAllowance
  , cycleRefusal
  , decompose
  , depthRefusal
  , layerGate
  , requiredCycles
  , refusalWork
  , retainedChild
  , splitWork
  )



-- ---------------------------------------------------------------------------
-- Initial state
-- ---------------------------------------------------------------------------

-- | Built from "Chore"'s values, not hardcoded here — the chore is what this
-- run is asked to do, and swapping it is an edit to "Chore" alone.
initialState :: State
initialState =
  State
    { goal = choreGoal
    , plan = case choreMode of
        Authored {authoredPlan = p} -> p
        ProposeFromGoal -> chorePlan
        SprintBacklog {} -> chorePlan
    , phase = Ready
    , cycleCount = 0
    , snapshotDirtySource = choreSnapshotDirtySource
    , budget = choreBudget
    , lastRun = Nothing
    , rescueCount = Nothing
    }

-- ---------------------------------------------------------------------------
-- The resident cycle
-- ---------------------------------------------------------------------------

-- | One resident cycle unfolds a development tree into isolated agents and
-- folds their branches back upward.  Haskell never runs a git WORKFLOW verb
-- from the runtime: the mechanical tier below is authored
-- policy running plain git through 'Exec' in a worktree this node owns, and
-- everything cognitive is a coding agent with its own native tools.
loop :: State -> Harness State
loop = resumeLoop emptyResume

-- | The RESUMED entry.  The driver folds this run's journal at
-- boot and injects it here; 'loop' is this function at 'emptyResume', so there
-- is ONE spelling of the run.
--
-- A fresh run is byte-for-byte the old path: 'resumed' is the identity when
-- the fold carries nothing, no worktree is looked up rather than created, and
-- no adopt-and-verify git read happens.  Everything below the guard is what a
-- non-empty fold buys.
resumeLoop :: ResumeFold -> State -> Harness State
resumeLoop fold st
  | phase st == Ready = enter st
  -- A COMPLETED run re-enters (once) when the journal still holds a pending
  -- amendment for a failed subtree — without this, a Replan decision
  -- journaled during a finished turn is unreachable forever: the phase
  -- guard blocks every later turn before resume can consume it (run 24:
  -- turns 2 and 3 no-oped on exactly this).  Bounded by 'rescueCount' so a
  -- rescue that fails again does not re-enter every turn until restart.
  | phase st == Completed
  , Just branch <- rootBranchOf fold (nodeName (plan st))
  , rescuePending foldLadder fold branch (plan st)
  , fromMaybe 0 (rescueCount st) < 1 = do
      trace "rescue-reenter" (nodeName (plan st)) (object ["rescueCount" .= (fromMaybe 0 (rescueCount st) + 1)])
      enter st {rescueCount = Just (fromMaybe 0 (rescueCount st) + 1)}
  -- ANY turn that changes nothing says WHY, as data — a silent no-op cost
  -- run 24 three relaunches.
  | otherwise = do
      trace
        "park"
        "loop"
        ( object
            [ "phase" .= show (phase st)
            , "rescueCount" .= fromMaybe 0 (rescueCount st)
            , "rescuePending"
                .= case rootBranchOf fold (nodeName (plan st)) of
                  Just b -> rescuePending foldLadder fold b (plan st)
                  Nothing -> False
            ]
        )
      pure st
  where
    enter entrySt =
      effectivePlan fold entrySt >>= \case
        Left why -> pure (blocked entrySt why)
        Right proposedPlan -> runEffective entrySt {plan = proposedPlan}
    runEffective effective =
      rootTree fold effective >>= \case
        Left why -> pure (blocked effective why)
        Right rootHandle -> do
          let seed =
                NodeSeed
                  { seedPlan = plan effective
                  , seedTree = rootHandle
                  , seedDepth = 0
                  , seedCycles = (budget effective).maxAgentCycles
                  , seedAdopted = Nothing
                  }
          -- SEAM (residency).  `hyloM`'s recursive step is
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
          -- POLICY IS MIDDLEWARE, composed by ordinary function application.
          -- Read the coalgebra outside-in: the
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
          let b = budget effective
              coalg =
                resumed
                  ResumeHooks
                    { resumeSplitWork = splitWork
                    , resumeRefusalWork = refusalWork
                    , resumeAdoptedWork = adoptedWork
                    , resumeAllocate = allocateChildren
                    , resumeRetainedChild = retainedChild
                    , resumeFoldLadder = foldLadder
                    }
                  fold
                  ( Swarm.gated (layerGate b)
                      (Swarm.capped seedDepth b.maxDepth depthRefusal
                         (Swarm.budgeted cycleRefusal decompose))
                  )
              alg = Swarm.receipted stampFold integrate
          -- Siblings execute CONCURRENTLY (sprint items, multi-leaf chores).
          -- Safe because nothing in the coalgebra/algebra reads completion
          -- order (the SEAM note above); plan-order reassembly is
          -- hyloConcurrentM's own contract.
          outcome <- Swarm.hyloConcurrentM mapConcurrently alg coalg seed
          summary <- summarize fold outcome
          pure
            effective
              { phase = Completed
              , cycleCount = cycleCount effective + 1
              , lastRun = Just summary
              }

proposalJournalKey :: Text
proposalJournalKey = "root-plan"

-- | Resolve the plan before root worktree lookup. A recorded proposal always
-- wins on resume, preserving the root name that retained work is keyed by.
effectivePlan :: ResumeFold -> State -> Harness (Either Text DevPlan)
effectivePlan fold st = case choreMode of
  Authored {authoredPlan = p} -> pure (Right p)
  ProposeFromGoal -> case lookupEvent ProposeKind proposalJournalKey fold of
    Just (_, ProposeEvent {evProposePlan = p}) -> pure (Right p)
    _ -> do
      grounding <- groundingPack
      proposeAttempt grounding 1 Nothing
  SprintBacklog {sprintItems = items} -> case lookupEvent ProposeKind proposalJournalKey fold of
    Just (_, ProposeEvent {evProposePlan = p}) -> pure (Right p)
    _ -> do
      grounding <- groundingPack
      sprintAttempt items grounding 1 Nothing
  where
    -- A sprint is a task-SET made to parallelize: one subtree per backlog
    -- item, planned independently (or taken from an authored override),
    -- composed under an integration-only root.  Disjoint item boundaries are
    -- validated in code before the operator ever sees the approval form.
    sprintAttempt items grounding attempt priorNote = do
      let revision = maybe "" ("\nRevise the prior proposal in response to: " <>) priorNote
      subtrees <- traverse (resolveItem grounding revision) items
      let composed = sprintRoot subtrees
      case proposalViolation st.budget composed
        `orMaybe` sprintOverlap subtrees
        `orMaybe` sprintShortfall of
        Just why
          | attempt == (1 :: Int) -> sprintAttempt items grounding 2 (Just why)
          | otherwise -> pure (Left ("Sprint proposal remained invalid: " <> why))
        Nothing -> do
          audit <- pathAudit composed
          say (audit <> renderPlan 0 composed)
          approval <- askUser @PlanApproval
          if approval.planApproved
            then do
              recordEvent ProposeEvent {evKey = JournalKey proposalJournalKey, evProposePlan = composed}
              pure (Right composed)
            else
              if attempt == 1
                then sprintAttempt items grounding 2 (Just approval.revisionNote)
                else pure (Left ("Sprint proposal rejected: " <> approval.revisionNote))

    -- The item's cycle allowance is stamped INTO the plan ('nodeCycles'), so
    -- runtime allocation honors it — a prompt-only budget is advice the
    -- scheduler ignores (sol review, run 24).
    resolveItem grounding revision item = do
      sub <- case item.itemPlan of
        Just p -> pure p
        Nothing ->
          runLLMTurn @DevPlan
            (proposePrompt (sprintItemGoal item) st.budget {maxAgentCycles = item.itemCycles} grounding <> revision)
      pure sub {nodeCycles = Just item.itemCycles}

    sprintItemGoal item =
      [fmt|{item.itemGoal}

  This item is ONE SUBTREE of a sprint whose items run CONCURRENTLY in
  sibling worktrees.  Its boundary paths must not overlap any other sprint
  item's — prefer tight directory prefixes over broad ones.  Cycle allowance
  for this entire item: {item.itemCycles}.|]

    -- Integration-only root: no direct edits, no checks of its own — every
    -- verdict comes from the item subtrees' own receipts.  Replan keeps the
    -- isolate-and-report contract: a failed item journals its amendment and
    -- the fold continues folding its siblings.
    -- EMPTY task on purpose: an empty-task interior node is the structural
    -- "no scaffold worker" form (Unfold seeds children straight from HEAD) —
    -- a prose "make no edits" brief demonstrably cannot be trusted.
    sprintRoot subtrees =
      DevPlan
        { nodeName = "sprint"
        , nodeTask = ""
        , nodeChecks = []
        , nodeBoundary = concatMap (.nodeBoundary) subtrees
        , nodeTolerated = concatMap (.nodeTolerated) subtrees
        , nodeOnFailure = Replan
        , nodeSplit = Nothing
        , childPlans = subtrees
        , nodeCycles = Nothing
        }

    orMaybe (Just a) _ = Just a
    orMaybe Nothing b = b

    -- The sheet's arithmetic must be fundable: root reserves two cycles,
    -- and the items' summed asks must fit what remains.
    sprintShortfall = case choreMode of
      SprintBacklog {sprintItems = items} ->
        let total = 2 + sum (map (.itemCycles) items)
         in if total > st.budget.maxAgentCycles
              then Just [fmt|sprint needs {total} cycles (2 root + item asks) but maxAgentCycles is {st.budget.maxAgentCycles}|]
              else Nothing
      _ -> Nothing
    proposeAttempt grounding attempt priorNote = do
      let revision = maybe "" ("\nRevise the prior proposal in response to: " <>) priorNote
      proposed <- runLLMTurn @DevPlan (proposePrompt st.goal st.budget grounding <> revision)
      case proposalViolation st.budget proposed of
        Just why
          | attempt == 1 -> proposeAttempt grounding 2 (Just why)
          | otherwise -> pure (Left ("Proposed plan remained outside the budget: " <> why))
        Nothing -> do
          audit <- pathAudit proposed
          say (audit <> renderPlan 0 proposed)
          approval <- askUser @PlanApproval
          if approval.planApproved
            then do
              recordEvent ProposeEvent {evKey = JournalKey proposalJournalKey, evProposePlan = proposed}
              pure (Right proposed)
            else
              if attempt == 1
                then proposeAttempt grounding 2 (Just approval.revisionNote)
                else pure (Left ("Plan proposal rejected: " <> approval.revisionNote))

-- | The first structural budget breach, if any. Root depth is zero.  Also
-- refuses duplicate node names anywhere in the tree: names key retained
-- worktrees and journal branch lookups, and uniqueness was previously only
-- prompt advice (sol review).
proposalViolation :: Budget -> DevPlan -> Maybe Text
proposalViolation b plan = go 0 plan `orElseMaybe` dupName
  where
    orElseMaybe (Just a) _ = Just a
    orElseMaybe Nothing y = y
    dupName =
      let names = allNames plan
          dupes = [n | n <- names, length (filter (== n) names) > 1]
       in listToMaybe [[fmt|node name {n} appears more than once in the plan|] | n <- dupes]
    allNames q = nodeName q : concatMap allNames (childPlans q)
    go :: Int -> DevPlan -> Maybe Text
    go depth p
      | depth > b.maxDepth = Just [fmt|node {p.nodeName} is at depth {depth}, above maxDepth {b.maxDepth}|]
      | length p.childPlans > b.gateWiderThan =
          Just [fmt|node {p.nodeName} has {length p.childPlans} children, above gateWiderThan {b.gateWiderThan}|]
      | otherwise = firstJust (map (go (depth + 1)) p.childPlans)

    firstJust :: [Maybe Text] -> Maybe Text
    firstJust [] = Nothing
    firstJust (Nothing : xs) = firstJust xs
    firstJust (found : _) = found

-- | The first cross-item boundary overlap in a sprint, if any.  Two boundary
-- entries overlap when equal or when one is a directory prefix of the other
-- — overlapping items would race in concurrent sibling worktrees.  Each
-- item's boundary is its whole SUBTREE's boundary set, not just its root's
-- (sol review: a root-only check misses every descendant path).
sprintOverlap :: [DevPlan] -> Maybe Text
sprintOverlap subtrees =
  listToMaybe
    [ [fmt|sprint items {a.nodeName} and {b.nodeName} overlap on boundary paths {x} and {y}|]
    | (a, b) <- pairs subtrees
    , x <- subtreeBoundaries a
    , y <- subtreeBoundaries b
    , pathsOverlap x y
    ]
  where
    subtreeBoundaries q = nodeBoundary q <> concatMap subtreeBoundaries (childPlans q)
    pairs (a : rest) = [(a, b) | b <- rest] <> pairs rest
    pairs [] = []
    pathsOverlap x y =
      let nx = T.dropWhileEnd (== '/') x
          ny = T.dropWhileEnd (== '/') y
       in nx == ny || (nx <> "/") `T.isPrefixOf` ny || (ny <> "/") `T.isPrefixOf` nx

-- | Deterministic repository grounding for the propose turn: the proposer is
-- a runLLMTurn session with no repo access of its own, so CODE assembles what
-- it needs — the tracked-file shape and the top of the root docs.  Assembled
-- via Exec in the source checkout; a command that fails degrades to a note
-- rather than blocking the proposal.
groundingPack :: Harness Text
groundingPack = do
  files <- groundingCmd "git ls-files | head -n 250"
  docs <- groundingCmd "sed -n '1,40p' CLAUDE.md"
  pure [fmt|  Tracked files (first 250):
{files}
  Root CLAUDE.md (first 40 lines):
{docs}|]
  where
    groundingCmd cmd =
      runInTry "." cmd <&> \case
        Left err -> [fmt|  ({cmd} unavailable: {err})|]
        Right pr -> pr.stdout

-- | Untracked file-shaped boundary/tolerated entries across a whole plan:
-- probably stale paths inherited from goal text.  Verification is CODE's job
-- — the prompt's "paths are verified" line is guidance, this is the check
-- (sol review: a model reading a truncated grounding list cannot verify
-- anything).  A directory entry or a genuinely-new file is legitimate, so
-- this audits — a loud note the operator sees at approval — rather than
-- refuses.
pathAudit :: DevPlan -> Harness Text
pathAudit p =
  runInTry "." "git ls-files" <&> \case
    Left _ -> ""
    Right pr ->
      let tracked = T.lines pr.stdout
          fileShaped e = not ("/" `T.isSuffixOf` e) && "." `T.isInfixOf` lastSegment e
          suspect e = fileShaped e && e `notElem` tracked
          bad = filter suspect (collect p)
       in if null bad
            then ""
            else [fmt|POSSIBLY-STALE PATHS (file-shaped, not tracked in this tree): {T.intercalate ", " bad}
|]
  where
    collect q = nodeBoundary q <> nodeTolerated q <> concatMap collect (childPlans q)
    lastSegment e = case reverse (T.splitOn "/" e) of
      (x : _) -> x
      [] -> ""

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
-- The retain-first rule is what makes this the first thing resume does —
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
