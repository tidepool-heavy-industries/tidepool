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
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Aeson (Value, object, toJSON, (.=))
import Tidepool.Agent.Spawn (AgentHandle, awaitAgent, cancelAgent, spawnAgent, spawnAsync)
-- `SpawnError`/`spawnSpecIn`/`renderSpawnError`, the Console `say`, the Exec
-- verbs, and the worktree receipt's own fields are generated into
-- `Tidepool.Effects`; the curated modules re-export only their own vocabulary.
import Tidepool.Effects
  ( ExecError (..)
  , SpawnError
  , WorktreeHandle (..)
  , WorktreeReceipt (..)
  , WorktreeSummary (..)
  , renderSpawnError
  , runIn
  , say
  , spawnSpecIn
  )
import Tidepool.Event
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.Journal (record)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
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
  }

-- | What the coalgebra decided, handed to the algebra unchanged.  The hylo's
-- @t@.
--
-- @workKids@ is the parent's own record of the seeds it unfolded, in plan
-- order.  It is what lets the algebra zip its @[Outcome]@ back against the
-- worktrees those outcomes came from — @traverse@ preserves order, and plan
-- order is the ONLY order any policy here reads.
data NodeWork = NodeWork
  { workSeed     :: NodeSeed
  , workScaffold :: Maybe WorkerResult
  , workKids     :: [NodeSeed]
  , workDenied   :: [Text]
  , workRefusal  :: Maybe Failure
  }

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
        Right rootTree -> do
          let seed =
                NodeSeed
                  { seedPlan = plan st
                  , seedTree = rootTree
                  , seedDepth = 0
                  , seedCycles = (budget st).maxAgentCycles
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
          let b = budget st
              coalg =
                Swarm.gated (layerGate b)
                  (Swarm.capped seedDepth b.maxDepth depthRefusal
                     (Swarm.budgeted cycleRefusal decompose))
              alg = Swarm.receipted stampFold integrate
          outcome <- Swarm.hyloM alg coalg seed
          summary <- summarize outcome
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
  | otherwise =
      runWorker seed.seedTree name (scaffoldPrompt p kids) >>= \case
        Left err ->
          pure
            ( Swarm.PlanF
                (refusalWork seed (Failure SpawnDenied [fmt|{name} scaffold: {renderSpawnError err}|] []))
                []
            )
        Right scaffold -> do
          scaffoldHead <- worktreeHead seed.seedTree
          record "split" (branchOf seed.seedTree) (splitPayload p kids scaffoldHead)
          (childSeeds, denied) <- allocateChildren seed kids
          pure (Swarm.PlanF (splitWork seed (Just scaffold) childSeeds denied) childSeeds)
  where
    p = seed.seedPlan
    name = nodeName p
    kids = childPlans p

splitWork :: NodeSeed -> Maybe WorkerResult -> [NodeSeed] -> [Text] -> NodeWork
splitWork seed scaffold childSeeds denied =
  NodeWork
    { workSeed = seed
    , workScaffold = scaffold
    , workKids = childSeeds
    , workDenied = denied
    , workRefusal = Nothing
    }

-- | The task a truncated node carries.  A coalgebra cannot produce an
-- outcome — its result type is @PlanF@ — so every veto in this file expresses
-- itself by handing the algebra a childless node whose task says why.
refusalWork :: NodeSeed -> Failure -> NodeWork
refusalWork seed f = (splitWork seed Nothing [] []) {workRefusal = Just f}

-- ---------------------------------------------------------------------------
-- The coalgebra's policy slots
--
-- Each is an ordinary function the middleware calls; each is effectful
-- (@a -> M (Maybe NodeWork)@) so it can tier — a deterministic heuristic
-- first, the operator past that — inside one function with ordinary
-- branching.  The two below that CAN be pure are pure, deliberately: a pure
-- slot is a slot a test can call directly.
-- ---------------------------------------------------------------------------

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
    required = if null (childPlans seed.seedPlan) then 1 else 2

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

-- | Every child worktree is created from the parent's CURRENT state, which by
-- the ordering above is the scaffold worker's final commit.
allocateChildren :: NodeSeed -> [DevPlan] -> Harness ([NodeSeed], [Text])
allocateChildren parent kids = go kids
  where
    allowance = childAllowance parent (length kids)
    go [] = pure ([], [])
    go (k : rest) =
      createWorktree (fromWorktree parent.seedTree (nodeName k)) >>= \case
        Left err -> do
          (seeds, denied) <- go rest
          pure (seeds, [fmt|{nodeName k}: {renderWorktreeError err}|] : denied)
        Right childTree -> do
          (seeds, denied) <- go rest
          let s =
                NodeSeed
                  { seedPlan = k
                  , seedTree = childTree
                  , seedDepth = parent.seedDepth + 1
                  , seedCycles = allowance
                  }
          pure (s : seeds, denied)

-- | Divide what is left after this node's own reservation among its children.
--
-- Conservative on purpose: a subtree that finishes under its share does not
-- return the remainder to its siblings.  That is the honest cost of enforcing
-- a budget with no shared mutable state in the row — and the division is
-- deterministic, so no scheduling order can change it.
childAllowance :: NodeSeed -> Int -> Int
childAllowance parent n
  | n <= 0 = 0
  | otherwise = max 1 ((parent.seedCycles - 2) `div` n)

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
integrate (Swarm.PlanF w kids) = case w.workRefusal of
  Just f -> pure Skipped {outcomeNode = name, outcomeTrail = [], skipReason = renderFailure f}
  Nothing -> case kids of
    [] -> leafFold w
    _ -> interiorFold w kids
  where
    name = nodeName w.workSeed.seedPlan

-- | 'Swarm.receipted''s slot, and the whole trust ladder in one place.
--
-- A HIGHER RUNG NEVER OVERRIDES A FAILING LOWER RUNG: 'foldLadder' is an
-- ordered case over the receipt, so an agent's green summary over a red check
-- is a red node.  Evidence is journaled either way — a fold that failed its
-- ladder is exactly the fold whose evidence someone will want.
stampFold :: Swarm.PlanF NodeWork Outcome -> Outcome -> Harness Outcome
stampFold node folded = do
  journalOutcome folded
  pure (withTrail (concatMap outcomeTrailOf (Swarm.kids node)) (foldLadder folded))

journalOutcome :: Outcome -> Harness ()
journalOutcome o = case o of
  Done {doneReceipt = r} -> record "outcome" r.receiptBranch (toJSON r)
  Failed {outcomeNode = n, outcomeFailure = f, partialReceipt = Just r} ->
    record "outcome" r.receiptBranch (object ["node" .= n, "failure" .= toJSON f, "receipt" .= toJSON r])
  Failed {outcomeNode = n, outcomeFailure = f, partialReceipt = Nothing} ->
    record "outcome" n (object ["node" .= n, "failure" .= toJSON f])
  Skipped {outcomeNode = n, skipReason = why} ->
    record "outcome" n (object ["node" .= n, "skipped" .= why])

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
leafFold :: NodeWork -> Harness Outcome
leafFold w = do
  before <- worktreeHead tree
  runWorker tree name (workerPrompt p) >>= \case
    Left err ->
      pure (failedOutcome name (Failure SpawnDenied (renderSpawnError err) []) Nothing)
    Right wr -> do
      after <- worktreeHead tree
      checks <- runChecks tree p
      finishFold w wr (before, after) [] [] 1 True checks
  where
    tree = w.workSeed.seedTree
    p = w.workSeed.seedPlan
    name = nodeName p

-- | An interior node: the eager rebase cascade, the merges, then the ladder.
--
-- A conflict-free fold spends ZERO agent cycles — mechanical git IS the
-- integration tier, so the fast-forward question dissolves into it.  The
-- integration agent is spawned only when the mechanical tier left something
-- for it: an escalation, or a check that fails at the merged head.
interiorFold :: NodeWork -> [Outcome] -> Harness Outcome
interiorFold w kids = do
  before <- worktreeHead tree
  acc <- foldChildren tree p (zip w.workKids kids) emptyAcc {accEsc = deniedEsc}
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
      w
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
    tree = w.workSeed.seedTree
    p = w.workSeed.seedPlan
    deniedEsc = map ("child worktree denied — " <>) w.workDenied

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
            cascaded <- cascade p newHead (map fst rest) acc {accNotes = acc.accNotes <> [note], accMerged = acc.accMerged + 1}
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
    record "replan" (branchOf s.seedTree) (toJSON decision)
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
                  record "rebase" branch (toJSON (RebaseNote branch ontoText RebaseClean))
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
          record "rebase" (branchOf s.seedTree) (toJSON note)
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
  record "escalation" branch (object ["node" .= name, "detail" .= why])
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
    record "replan" (branchOf s.seedTree) (toJSON decision)
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
  :: NodeWork
  -> WorkerResult
  -> (GitOid, GitOid)
  -> [RebaseNote]
  -> [Text]
  -> Int
  -> Bool
  -> [CheckResult]
  -> Harness Outcome
finishFold w wr (before, after) notes escalations cycles agentRan checks = do
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
    tree = w.workSeed.seedTree
    p = w.workSeed.seedPlan
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
    Left _ -> pure []
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
-- Journal payloads
-- ---------------------------------------------------------------------------

-- | The coalgebra's output.  Decomposition is cognition, so it is recorded and
-- never re-derived; a nondeterministic re-plan on resume would orphan every
-- completed child below it.
splitPayload :: DevPlan -> [DevPlan] -> GitOid -> Value
splitPayload p kids scaffoldHead =
  object
    [ "node" .= nodeName p
    , "scaffoldHead" .= renderGitOid scaffoldHead
    , "children" .= map nodeName kids
    , "plan" .= toJSON p
    ]

-- ---------------------------------------------------------------------------
-- Run summary
-- ---------------------------------------------------------------------------

summarize :: Outcome -> Harness RunSummary
summarize root = do
  trees <- listWorktrees
  pure
    RunSummary
      { runRoot = outcomeNodeName root
      , runStatus = if outcomeIsDone root then "done" else failureText root
      , runTrail = outcomeTrailOf root
      , runEscalations = escalationsOf root
      , retainedWorktrees =
          [renderWorktreeId s.summaryReceipt.treeId | s <- trees, s.present]
      }

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
