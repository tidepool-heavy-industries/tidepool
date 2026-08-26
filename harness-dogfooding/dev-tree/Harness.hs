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
        Left why -> blockedLoud entrySt why
        Right proposedPlan -> runEffective entrySt {plan = proposedPlan}
    runEffective effective =
      rootTree fold effective >>= \case
        Left why -> blockedLoud effective why
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
      let revision =
            maybe
              ""
              ( \note ->
                  "\nThe operator reviewed the previously COMPOSED sprint (all items) and noted: "
                    <> note
                    <> "\nApply only what concerns THIS item's goal; where the note targets a different item, propose THIS item exactly as you otherwise would."
              )
              priorNote
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
          checksBaseline <- checkAudit composed
          say (audit <> checksBaseline <> renderPlan 0 composed)
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
  for this entire item: {item.itemCycles}.

  SIBLING DEPENDENCIES: your item's worktree forks BEFORE any sibling item's
  work exists, so nothing your tasks or checks rely on may come from another
  item's deliverable (sprint 25: a campaign whose acceptance check was the
  script a sibling item was busy fixing stalled on the unfixed copy). If the
  goal has such a dependency, SAY SO in the node task — the operator
  serializes it into a later sprint instead.

  THIS ITEM ONLY: your session may contain an earlier item's planning
  exchange — that item is DONE and none of your business. Propose a plan for
  THE GOAL ABOVE and nothing else; re-finalizing a previously produced plan
  is always wrong (sprint 25b: an item planner re-emitted its session's
  prior item verbatim, and the composed sprint died on duplicate names).|]

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
          checksBaseline <- checkAudit proposed
          say (audit <> checksBaseline <> renderPlan 0 proposed)
          approval <- askUser @PlanApproval
          if approval.planApproved
            then do
              recordEvent ProposeEvent {evKey = JournalKey proposalJournalKey, evProposePlan = proposed}
              pure (Right proposed)
            else
              if attempt == 1
                then proposeAttempt grounding 2 (Just approval.revisionNote)
                else pure (Left ("Plan proposal rejected: " <> approval.revisionNote))

-- | The first structural breach in a proposed plan, if any. Root depth is
-- zero.  Refuses: budget shape (depth, width), duplicate node names anywhere
-- in the tree (names key retained worktrees and journal branch lookups), and
-- any boundary or tolerated entry 'parseRepoPath' rejects — model-authored
-- paths are validated at ACCEPTANCE, so an unparseable entry is a refusal
-- the planner can fix, never a match-nothing boundary that fails the node's
-- own honest work at fold time.
proposalViolation :: Budget -> DevPlan -> Maybe Text
proposalViolation b plan = go 0 plan `orElseMaybe` dupName `orElseMaybe` badPath
  where
    orElseMaybe (Just a) _ = Just a
    orElseMaybe Nothing y = y
    dupName =
      listToMaybe [[fmt|node name {n} appears more than once in the plan|] | n <- duplicateNames plan]
    badPath =
      listToMaybe
        [ [fmt|node {nodeName q} has an unusable path entry — {why}|]
        | q <- planSubtree plan
        , entry <- nodeBoundary q <> nodeTolerated q
        , Left why <- [parseRepoPath entry]
        ]
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

-- | The first reason a sprint's items cannot safely run CONCURRENTLY, if
-- any.  Overlap is judged on each item's whole-subtree WRITE set — product
-- boundaries plus tolerated paths (two items that both tolerate @docs/@
-- still race) — and an item whose subtree declares no boundary at all is
-- refused outright: unbounded write scope cannot be proven disjoint from
-- anything.
sprintOverlap :: [DevPlan] -> Maybe Text
sprintOverlap subtrees = unbounded `orElseMaybe` pairOverlap
  where
    orElseMaybe (Just a) _ = Just a
    orElseMaybe Nothing y = y
    unbounded =
      listToMaybe
        [ [fmt|sprint item {a.nodeName} declares no boundary anywhere in its subtree — an unbounded item cannot run concurrently with siblings|]
        | length subtrees > 1
        , a <- subtrees
        , null (subtreeBoundaries a)
        ]
    pairOverlap =
      listToMaybe
        [ [fmt|sprint items {a.nodeName} and {b.nodeName} overlap on paths {x} and {y}|]
        | (a, b) <- pairs subtrees
        , x <- subtreeWriteable a
        , y <- subtreeWriteable b
        , overlaps x y
        ]
    -- An entry that fails to parse cannot PROVE disjointness, so it counts
    -- as overlapping ('proposalViolation' refuses it with the sharper
    -- message first — this is the backstop for authored item plans that
    -- skip that gate).
    overlaps x y = case (parseRepoPath x, parseRepoPath y) of
      (Right px, Right py) -> pathsOverlap px py
      _ -> True
    pairs (a : rest) = [(a, b) | b <- rest] <> pairs rest
    pairs [] = []

-- | Deterministic repository grounding for the propose turn: the proposer is
-- a runLLMTurn session with no repo access of its own, so CODE assembles what
-- it needs — the tracked-file shape and the top of the root docs.  Assembled
-- via Exec in the source checkout; a command that fails degrades to a note
-- rather than blocking the proposal.
groundingPack :: Harness Text
groundingPack = do
  files <- groundingCmd 250 "git -C \"${TIDEPOOL_SOURCE_REPO:-.}\" ls-files"
  docs <- groundingCmd 40 "cat \"${TIDEPOOL_SOURCE_REPO:-.}/CLAUDE.md\""
  pure [fmt|  Tracked files (first 250):
{files}
  Root CLAUDE.md (first 40 lines):
{docs}|]
  where
    -- Exec's cwd is the managed-worktrees root, NOT the source repo — every
    -- command must address the repo via $TIDEPOOL_SOURCE_REPO explicitly.
    -- Truncation happens HERE, not via `| head -n`: a pipeline's exit code
    -- is its last stage's, so `git ... | head` reports success (and empty
    -- stdout) when git itself failed — the planner then silently gets a
    -- blank grounding instead of this loud note.
    groundingCmd n cmd =
      runInTry "." cmd <&> \case
        Left err -> [fmt|  ({cmd} unavailable: {err})|]
        Right pr
          | pr.exitCode == 0 -> T.unlines (take n (T.lines pr.stdout))
          | otherwise -> [fmt|  ({cmd} failed (exit {pr.exitCode}): {pr.stderr})|]

-- | Baseline-run every distinct proposed check in the SOURCE repo before the
-- operator sees the approval form.  Sprint 25 shipped two checks that could
-- never pass (bare invocations of a script that requires a file argument) and
-- one that cannot finish inside the in-run Exec budget — all three visible in
-- one baseline line each, none visible in the rendered plan text.  An AUDIT,
-- not a refusal (pathAudit precedent): a discriminating check is EXPECTED to
-- fail at baseline (the work does not exist yet), so results render as
-- approval notes and judgment stays with the operator.  Usage-class exits
-- (2/126/127) get the sharper flag; a 60s timeout here warns the check may
-- not fit the 600s in-run budget either.
checkAudit :: DevPlan -> Harness Text
checkAudit p = do
  reports <- traverse probeCheck (nub (collectChecks p))
  pure (T.concat (catMaybes reports))
  where
    collectChecks q = nodeChecks q <> concatMap collectChecks (childPlans q)
    quoted cmd = "'" <> T.replace "'" "'\\''" cmd <> "'"
    firstLineOf t = T.takeWhile (/= '\n') (T.strip t)
    probeCheck cmd =
      let wrapped = "cd \"${TIDEPOOL_SOURCE_REPO:-.}\" && timeout 60 sh -c " <> quoted cmd
       in runInTry "." wrapped <&> \case
            Left _ -> Nothing
            Right pr
              | pr.exitCode == 0 -> Nothing
              | pr.exitCode == 124 ->
                  Just [fmt|CHECK BASELINE: `{cmd}` exceeded 60s at baseline — may not fit the in-run budget
|]
              | pr.exitCode `elem` [2, 126, 127] ->
                  Just [fmt|CHECK BASELINE: `{cmd}` exits {pr.exitCode} (usage/not-found class — likely malformed): {firstLineOf pr.stderr}
|]
              | otherwise ->
                  Just [fmt|CHECK BASELINE: `{cmd}` exits {pr.exitCode} at baseline: {firstLineOf pr.stderr}
|]

-- | Untracked file-shaped boundary/tolerated entries across a whole plan:
-- probably stale paths inherited from goal text.  Verification is CODE's job
-- — the prompt's "paths are verified" line is guidance, this is the check
-- (sol review: a model reading a truncated grounding list cannot verify
-- anything).  A directory entry or a genuinely-new file is legitimate, so
-- this audits — a loud note the operator sees at approval — rather than
-- refuses.
pathAudit :: DevPlan -> Harness Text
pathAudit p =
  -- Same cwd caveat as groundingPack: Exec roots at the worktrees dir, so
  -- git must be pointed at the source repo. A failed or empty listing
  -- degrades to NO audit — an empty tracked list would otherwise flag every
  -- file-shaped entry as stale.
  runInTry "." "git -C \"${TIDEPOOL_SOURCE_REPO:-.}\" ls-files" <&> \case
    Left _ -> ""
    Right pr | pr.exitCode /= 0 || T.null (T.strip pr.stdout) -> ""
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

-- | Every blocked transition is LOUD: said to the operator and traced as
-- data (the park rule — sprint 25b's first attempt died at proposal
-- validation and the only externally visible sign was a quiet BetweenTurns
-- gate).  Callers use 'blockedLoud'; the pure record update stays for tests.
blockedLoud :: State -> Text -> Harness State
blockedLoud st reason = do
  say [fmt|BLOCKED: {reason}|]
  trace "loop" "blocked" (object ["reason" .= reason])
  pure (blocked st reason)

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
        : [[fmt|prior process: {renderRebaseNote n}|] | (_, _, RebaseEvent {evNote = n}) <- eventsOfKind RebaseKind fold]

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
