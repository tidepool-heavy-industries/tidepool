{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | Fold-side algebra for the recursive-development-tree dogfood.
--
-- This module owns integration, receipt stamping, the trust ladder, child
-- folding, eager rebase escalation, retry policy, and fold-owned runtime
-- accumulators.  It uses "Micro" through a finish callback and never imports
-- the facade, keeping the execution graph acyclic.
module Fold
  ( PolicyOutcome (..)
  , FoldAcc (..)
  , integrate
  , stampFold
  , journalOutcome
  , foldLadder
  , leafFold
  , interiorFold
  , foldChildren
  , cascade
  , escalate
  , applyPolicy
  , mergeChild
  , finishFold
  , spawnIntegration
  , mechanicalResult
  , failureText
  ) where

import qualified Data.Text as T
import DevTreeJournal
  ( JournalEvent (..)
  , JournalKey (..)
  , recordEvent
  )
import HarnessTypes
import qualified Micro
import Micro (NodeSeed (..))
import Prompts
import Resume
  ( NodeWork (..)
  , ResumedFold (..)
  )
import Tidepool.Agent.Spawn
  ( AgentHandle
  , awaitAgent
  , cancelAgent
  , renderSpawnError
  , spawnAgent
  , spawnAsync
  )
import Tidepool.Effects
  ( SpawnError
  , WorktreeHandle (..)
  , spawnSpecIn
  )
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import qualified Tidepool.Swarm as Swarm
import Tidepool.Worktree
import Workers
  ( SnapshotResult (..)
  , boundaryViolations
  , branchOf
  , checkFailed
  , firstLine
  , gitIn
  , runChecks
  , runWorker
  , snapshotFailureMaybe
  )

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
  WorkFailed {workSeed = seed, workFailure = f} ->
    pure (failedOutcome (nodeName seed.seedPlan) f Nothing)
  WorkReady {workSeed = seed, workKids = wkids, workDenied = denied}
    | not (null (childPlans seed.seedPlan)) && null wkids ->
        pure
          ( failedOutcome
              (nodeName seed.seedPlan)
              (Failure WorktreeDenied [fmt|{nodeName seed.seedPlan} has child plans but no child worktrees were allocated|] denied)
              Nothing
          )
    | null kids -> leafFold seed
    | otherwise -> interiorFold seed wkids denied kids

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

-- | A leaf: one implementation worker (or an on-the-fly micro-split when the
-- plan asks for one), then the ladder.
leafFold :: NodeSeed -> Harness Outcome
leafFold seed = case seed.seedPlan.nodeSplit of
  Nothing -> directLeaf seed
  Just spec -> microLeaf seed spec

-- | The ordinary leaf: one worker cycle, whole task.
--
-- Rung 1 is the HEAD read either side of the cycle — a worker that claims
-- completion without committing is caught here and never reaches rung 2.  The
-- 'withHandler' scope is observation of the same fact as it happens; the pair
-- of 'worktreeHead' reads is what closes the gap a subscription
-- deliberately will not (no replay, loop-iteration-scoped lifetime).
directLeaf :: NodeSeed -> Harness Outcome
directLeaf seed = do
  before <- worktreeHead tree
  runWorker tree name (workerPrompt p) >>= \case
    Left err ->
      pure (failedOutcome name (Failure SpawnDenied (renderSpawnError err) []) Nothing)
    Right (wr, snapshot) -> do
      after <- worktreeHead tree
      checks <- runChecks tree p
      finishFold seed wr (before, after) [] [] 1 True checks (snapshotFailureMaybe name snapshot)
  where
    tree = seed.seedTree
    p = seed.seedPlan
    name = nodeName p

microLeaf = Micro.microLeaf finishFold

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
  (wr, agentCycles, agentRan, snapshotFailure) <-
    if not needsAgent
      then pure (mechanicalResult acc, 0, False, Nothing)
      else
        spawnIntegration tree p acc checks0 >>= \case
          Left err ->
            pure
              ( mechanicalResult acc
                  {accEsc = acc.accEsc <> [[fmt|integration spawn failed: {renderSpawnError err}|]]}
              , 0
              , False
              , Nothing
              )
          Right (merged, snapshot) ->
            pure (merged, 1, True, snapshotFailureMaybe (nodeName p <> "-integration") snapshot)
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
      snapshotFailure
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
        next <- onChildFailure tree p s o rest acc
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
-- that stops the fold; a leaf 'Retry' gets one fresh worker cycle and is merged
-- only when that cycle passes the same receipt ladder.  'Replan' opens a
-- planning agent session and JOURNALS the amendment, because re-unfolding a
-- subtree means re-entering the coalgebra — which is resume's job, not this
-- fold's.  A non-leaf retry remains the rebase-policy-shaped escalation it was.
onChildFailure :: WorktreeHandle -> DevPlan -> NodeSeed -> Outcome -> [(NodeSeed, Outcome)] -> FoldAcc -> Harness FoldAcc
onChildFailure tree p s o rest acc = case nodeOnFailure p of
  Abandon -> pure acc {accAbandon = Just why, accEsc = acc.accEsc <> [why]}
  Replan -> do
    decision <- runLLMTurn @ReplanDecision (replanPrompt p s.seedPlan why)
    recordEvent (ReplanEvent (JournalKey (branchOf s.seedTree)) decision)
    pure $
      if decision.abandonSubtree
        then acc {accAbandon = Just (why <> " — replan abandoned"), accEsc = acc.accEsc <> [why]}
        else acc {accEsc = acc.accEsc <> [[fmt|{why} — replanned: {decision.amendedInstruction}|]]}
  AskOperator ->
    askUser @Triage >>= \t -> case t.triageAction of
      TriageAbandon -> pure acc {accAbandon = Just (why <> " — operator abandoned"), accEsc = acc.accEsc <> [why]}
      TriageRetry
        | retryableLeaf -> retryLeaf t.triageNote
        | otherwise -> pure acc {accEsc = acc.accEsc <> [[fmt|{why} — operator: {t.triageNote}|]]}
      TriageSkip -> pure acc {accEsc = acc.accEsc <> [[fmt|{why} — operator: {t.triageNote}|]]}
  Retry
    | retryableLeaf -> retryLeaf ""
    | otherwise -> pure acc {accEsc = acc.accEsc <> [why]}
  where
    why = [fmt|{outcomeNodeName o}: {failureText o}|]
    retryableLeaf = null (childPlans s.seedPlan) && case o of
      Failed {} -> True
      _ -> False
    retryLeaf operatorNote = do
      before <- worktreeHead s.seedTree
      let child = nodeName s.seedPlan
          retryName = child <> "-retry"
          retryPrompt =
            workerPrompt s.seedPlan
              <> [fmt|\n\nPrevious attempt failed for {child}: {failureText o}. Re-check that failure and complete the task.|]
              <> [fmt|{operatorAmendment}|]
          spent = acc {accCycles = acc.accCycles + 1}
          operatorAmendment
            | T.null (T.strip operatorNote) = ""
            | otherwise = [fmt| Operator guidance from triage: {operatorNote}|]
      runWorker s.seedTree retryName retryPrompt >>= \case
        Left _err -> pure spent {accEsc = spent.accEsc <> [why]}
        Right (wr, snapshot) -> do
          after <- worktreeHead s.seedTree
          checks <- runChecks s.seedTree s.seedPlan
          retried <-
            finishFold
              s
              wr
              (before, after)
              []
              []
              1
              True
              checks
              (snapshotFailureMaybe retryName snapshot)
          -- Match the normal 'stampFold' path: persist the raw receipt before
          -- applying the ladder, so a later resume sees this retry as the
          -- latest outcome rather than resurrecting the original failure.
          journalOutcome retried
          let judged = foldLadder retried
          case judged of
            Done {} ->
              mergeChild tree p s >>= \case
                Left mergeWhy -> escalate p s mergeWhy spent
                Right note -> do
                  newHead <- worktreeHead tree
                  let ahead = [sib | (sib, out) <- rest, outcomeIsDone out]
                  cascade
                    p
                    newHead
                    ahead
                    spent
                      { accNotes = spent.accNotes <> [note]
                      , accMerged = spent.accMerged + 1
                      }
            _ -> pure spent {accEsc = spent.accEsc <> [why]}

failureText :: Outcome -> Text
failureText o = case o of
  Done {} -> "done"
  Failed {outcomeFailure = f} -> case f.failureKind of
    -- Keep the durable payload legible to failure-policy prompts instead of
    -- asking them to recover the count or snapshot identity from prose.
    MicrotasksIncomplete {acceptedMicrotasksRan = ran} ->
      [fmt|{renderFailure f} — {ran} accepted microtasks ran|]
    SnapshotFailed {snapshotName = snapshot} ->
      [fmt|{renderFailure f} — failed snapshot {snapshot}|]
    _ -> renderFailure f
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
        (resolutionPrompt s.seedPlan (renderGitOid onto) Nothing)
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
-- turns it into a retry, a planning agent session, an operator form, or an
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

-- | The failure-policy sum, applied by deterministic code.  Cognition
-- enters through exactly two constructors: 'Replan' opens a planning agent session
-- scoped to the failure, 'AskOperator' presents a typed triage form.
applyPolicy :: DevPlan -> NodeSeed -> Text -> Harness PolicyOutcome
applyPolicy p s why = case nodeOnFailure p of
  Abandon -> pure (PolicyAbandoned [fmt|{why} — abandoned by policy|] 0)
  Retry -> retryOnce "Try again; the previous resolution round did not converge."
  Replan -> do
    decision <- runLLMTurn @ReplanDecision (replanPrompt p s.seedPlan why)
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
              (resolutionPrompt s.seedPlan (renderGitOid h) (Just instruction))
          )
          >>= \case
            Left err -> pure (PolicyEscalated [fmt|{why} — retry spawn failed: {renderSpawnError err}|] 1)
            Right (_, rr)
              | rr.resolved -> pure (PolicyResolved 1)
              | otherwise -> pure (PolicyEscalated [fmt|{why} — retry unresolved: {rr.resolutionNotes}|] 1)

-- | Merge one child branch into this node.  Mechanical first — a clean merge
-- is the whole integration tier at zero tokens — via the canonical typed
-- 'mergeBranchInto' verb: conflict-vs-failure classification and abort
-- discipline are the runtime's, not re-derived here.
mergeChild :: WorktreeHandle -> DevPlan -> NodeSeed -> Harness (Either Text RebaseNote)
mergeChild tree p s =
  mergeBranchInto (worktreeId tree) childBranch message >>= \case
    Left err -> pure (Left (renderWorktreeError err))
    Right (Conflict paths) -> pure (Left [fmt|merge conflict: {T.intercalate ", " paths}|])
    Right (Merged _commit) ->
      pure (Right (RebaseNote (renderBranchName childBranch) (renderBranchName tree.handleReceipt.branch) RebaseClean))
  where
    childBranch = s.seedTree.handleReceipt.branch
    message = [fmt|fold {renderBranchName childBranch} into {nodeName p}|]

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
  -> Maybe Failure
  -> Harness Outcome
finishFold seed wr (before, after) notes escalations cycles agentRan checks forcedFailure = do
  (outside, tolerated) <- boundaryViolations tree (nodeBoundary p) (nodeTolerated p)
  let receipt =
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
          , receiptEvidence =
              wr.evidence
                <> map ("obstacle: " <>) wr.obstacles
                <> map ("friction: " <>) wr.frictionNotes
                <> escalations
                <> map ("tolerated: " <>) tolerated
          }
  pure $ case forcedFailure of
    Just failure -> failedOutcome name failure (Just receipt)
    Nothing -> Done name [] receipt
  where
    tree = seed.seedTree
    p = seed.seedPlan
    name = nodeName p

-- | Spawn the optional integration worker through the shared worker seam.
spawnIntegration
  :: WorktreeHandle -> DevPlan -> FoldAcc -> [CheckResult] -> Harness (Either SpawnError (WorkerResult, SnapshotResult))
spawnIntegration tree p acc checks =
  runWorker tree (nodeName p <> "-integration") (integrationPrompt p acc.accMerged acc.accEsc checks)

-- | The orchestrator's OWN account of a mechanical fold.  Not a model claim
-- dressed as one: nothing here was asked of an agent, and the receipt says so.
mechanicalResult :: FoldAcc -> WorkerResult
mechanicalResult acc =
  WorkerResult
    { workSummary =
        [fmt|Mechanical fold: {acc.accMerged} child branches merged with no conflicts, {length acc.accNotes} rebase steps, 0 agent cycles.|]
    , evidence = map renderNote acc.accNotes
    , readyForIntegration = True
    , obstacles = []
    , frictionNotes = []
    }
  where
    renderNote n = [fmt|{n.rebaseBranch} onto {n.rebaseOnto}: {show n.rebaseTier}|]
