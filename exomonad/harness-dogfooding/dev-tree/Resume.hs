{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Crash recovery for the recursive-development-tree dogfood.
--
-- This module owns the interpretation of the folded run journal, retained
-- worktree adoption, and the resume typestates.  The small 'ResumeHooks'
-- record is the stable seam to the coalgebra and algebra: Resume can replay
-- or adopt work without importing their future Unfold/Fold homes, so the
-- dependency graph remains acyclic while the facade keeps the old API.
module Resume
  ( NodeWork (..)
  , ResumedFold (..)
  , ResumePlan (..)
  , SplitRecord (..)
  , ResumeHooks (..)
  , resumed
  , resumePlanFor
  , descendantAmendPending
  , rescuePending
  , newestEntry
  , substantiveAmendment
  , amendPlan
  , amendPlanChecked
  , recordedPlanFor
  , verdictTag
  , splitRecordOf
  , adoptOrUnfold
  , replaySplit
  , integrationComplete
  , recordedDone
  , RetainedWorktree
  , HeadChanged (..)
  , VerifiedOrphan
  , checkHeadChanged
  , verifyOrphan
  , adopt
  , retainWorktree
  , retainedHandle
  , rootBranchOf
  , replayedWork
  , adoptedWork
  ) where

import qualified Data.Text as T
import DevTreeJournal
  ( JournalEvent (..)
  , JournalKey (..)
  , JournalKind (..)
  , eventsOfKind
  , lookupEvent
  , undecodableEntries
  )
import DevSwarmTypes
  ( LeafStrategy (..)
  , NodeSeed (..)
  , PlanShape (..)
  , planShape
  )
import HarnessTypes
import Tidepool.Aeson (object, (.=))
import Tidepool.Effects (WorktreeHandle (..))
import Tidepool.Journal (trace)
import Tidepool.Harness (Harness)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Resume (ResumeFold (..), isResumed)
import qualified Tidepool.Swarm as Swarm
import Tidepool.Worktree
import Git (isAncestor, renderGitFailure, statusEntries)
import Workers (boundaryViolations, branchOf, runChecks)

-- | What the coalgebra decided, handed to the algebra unchanged.  The
-- resumed/adopted terminal form lives here because Resume is its only owner.
data NodeWork
  = WorkReady
      { workSeed     :: NodeSeed
      , workScaffold :: Maybe WorkerResult
      , workKids     :: [NodeSeed]
      , workDenied   :: [Text]
      }
  | WorkRefused
      { workSeed    :: NodeSeed
      , workFailure :: Failure
      }
  | WorkResumed
      { workSeed    :: NodeSeed
      , workOutcome :: ResumedFold
      }
  | WorkFailed
      { workSeed    :: NodeSeed
      , workFailure :: Failure
      }

-- | Where a resumed node's outcome came from — and therefore whether it still
-- needs to be journaled.
data ResumedFold
  = ReplayedOutcome Outcome
  | AdoptedOutcome Outcome

-- | The stable integration seam to the still-authored coalgebra and algebra.
-- Resume owns the protocol; the facade supplies only constructors, child
-- allocation, retained-child rebinding, and the common fold ladder.
data ResumeHooks = ResumeHooks
  { resumeSplitWork      :: NodeSeed -> Maybe WorkerResult -> [NodeSeed] -> [Text] -> NodeWork
  , resumeRefusalWork    :: NodeSeed -> Failure -> NodeWork
  , resumeAdoptedWork    :: NodeSeed -> Outcome -> NodeWork
  , resumeAllocate       :: JournalKey -> DevPlan -> Text -> [(Text, Text)] -> NodeSeed -> [DevPlan] -> (DevPlan -> Harness (Either Text WorktreeHandle)) -> Harness ([NodeSeed], [Text])
  , resumeRetainedChild  :: NodeSeed -> [(Text, Text)] -> DevPlan -> Harness (Either Text WorktreeHandle)
  , resumeFoldLadder     :: Outcome -> Outcome
  }

-- ---------------------------------------------------------------------------
-- Resume — consuming the folded run journal
--
-- @record@ stays WRITE-ONLY on this side: nothing below opens a file, and
-- there is no read verb anywhere in the row.  The driver folds this run's
-- journal at boot and hands the result to 'resumeLoop'; everything here is
-- interpretation of that value, plus the orchestrator's OWN git reads and
-- checks against what it finds on disk.
--
-- Three locks shape all of it.  Decomposition is cognition, so a recorded
-- split REPLAYS rather than being re-derived.  Retained worktrees REBIND,
-- never recreate.  A commit found in a retained worktree is adopted only
-- after this orchestrator's own checks pass at that sha.
-- ---------------------------------------------------------------------------

-- | What the fold says about one branch.  Decided by 'resumePlanFor' BEFORE
-- any git runs, so the whole precedence question is a pure function.
data ResumePlan
  = ResumeFresh
  | ResumeSkip Outcome
  | ResumeReplay SplitRecord
  | ResumeAmend ReplanDecision DevPlan
  deriving (Show, Eq)

-- | The @split@ payload, read back from a decoded 'SplitEvent'.
data SplitRecord = SplitRecord
  { splitNode         :: Text
  , splitScaffoldHead :: Text
  , splitPlan         :: DevPlan
  , splitChildTrees   :: [(Text, Text)]
  }
  deriving (Show, Eq)

-- | The resume wrapper over the composed coalgebra.  An empty fold is the
-- identity, preserving the fresh-run path byte-for-byte.
resumed
  :: ResumeHooks
  -> ResumeFold
  -> Swarm.Coalg Harness NodeWork NodeSeed
  -> Swarm.Coalg Harness NodeWork NodeSeed
resumed hooks fold inner
  | not (isResumed fold) = inner
  | otherwise = go
  where
    go seed = do
      let branch = branchOf seed.seedTree
          verdict = resumePlanFor hooks.resumeFoldLadder fold branch seed.seedPlan
      -- Decision narration is DATA on the trace stream, not prose: run 24
      -- burned three relaunches because a skipping resume said nothing.
      trace
        "resume-verdict"
        branch
        (object ["node" .= nodeName seed.seedPlan, "verdict" .= verdictTag hooks.resumeFoldLadder verdict])
      -- "Recorded but unreadable" is NOT "never recorded": the decode still
      -- degrades to fresh work, but the degradation is said — a version-skewed
      -- entry silently tagged @fresh@ is a redo nobody can diagnose.
      traverse_
        (\(kind, k, sq) -> trace "resume-undecodable" branch (object ["kind" .= kind, "key" .= k, "seq" .= sq]))
        [e | e@(_, k, _) <- undecodableEntries fold, k == branch || k == nodeName seed.seedPlan]
      case verdict of
        ResumeAmend d _
          | Just why <- snd (amendPlanChecked d (recordedPlanFor fold branch seed.seedPlan)) ->
              trace "amend-subtree-rejected" branch (object ["why" .= why])
        _ -> pure ()
      dispatch seed verdict
    dispatch seed = \case
      ResumeSkip o -> pure (Swarm.PlanF (replayedWork seed o) [])
      ResumeReplay sp -> replaySplit hooks fold seed sp
      ResumeFresh -> adoptOrUnfold hooks fold inner seed
      ResumeAmend d amended
        | d.abandonSubtree ->
            pure
              ( Swarm.PlanF
                  ( hooks.resumeRefusalWork
                      seed
                      ( Failure
                          ChildrenFailed
                          [fmt|{nodeName seed.seedPlan}: a journaled replan abandoned this subtree — {d.rationale}|]
                          []
                      )
                  )
                  []
              )
        | otherwise -> inner seed {seedPlan = amended}

-- | The fold's verdict for one branch.  Pure: no effects, git, or I/O.
-- @ladder@ is the fold ladder ('ResumeHooks.resumeFoldLadder'): the journal's
-- outcome wire stores the PRE-ladder receipt (a bare receipt decodes 'Done'
-- even when its own evidence — @receiptHeadMoved@, @receiptOutside@, failed
-- checks — fails the ladder), so doneness here must be judged on the
-- LADDERED outcome, exactly as 'recordedDone' already does.  Run 24: the
-- root's journaled receipt decoded 'Done', raw-doneness said skip, and the
-- rescue below never fired.
--
-- A recorded interior failure is terminal on resume EXCEPT when a
-- descendant branch still holds an unconsumed amendment (its own newest
-- entry is a 'ReplanEvent').  The fold journals the child's replan BEFORE
-- the parent's outcome, so the comparison must be per-descendant-branch
-- newest-wins, never replan-seq-vs-this-outcome-seq (run 24b: the root's
-- failed outcome shadowed the panel child's pending amendment and the
-- resumed turn did nothing).
resumePlanFor :: (Outcome -> Outcome) -> ResumeFold -> Text -> DevPlan -> ResumePlan
resumePlanFor ladder fold branch p = case newestEntry replanEntry splitEntry outcomeEntry of
  Just (_, OutcomeEvent {evOutcome = o})
    | not (outcomeIsDone (ladder o)) && descendantAmendPending ladder fold recordedPlan ->
        maybe ResumeFresh ResumeReplay recordedSplit
    | otherwise -> ResumeSkip o
  Just (_, SplitEvent {}) -> case recordedSplit of
    Just sp -> ResumeReplay sp
    Nothing -> ResumeFresh
  Just (_, ReplanEvent {evDecision = d}) ->
    ResumeAmend d (amendPlan d (maybe p (.splitPlan) recordedSplit))
  -- Only the three kinds looked up below can reach here; any other event
  -- as "newest" would mean a lookup bug, and fresh work is the safe verdict.
  Just _ -> ResumeFresh
  Nothing -> ResumeFresh
  where
    splitEntry = lookupEvent SplitKind branch fold
    -- An INSUBSTANTIAL amendment — nothing to say, nothing to restructure,
    -- not an abandonment — is ignored wholesale: consuming it would burn
    -- the one rescue re-entry re-running the identical failing plan.
    replanEntry = case lookupEvent ReplanKind branch fold of
      e@(Just (_, ReplanEvent {evDecision = d}))
        | substantiveAmendment d -> e
      _ -> Nothing
    -- Both keys can legitimately hold outcomes (receipted ones under the
    -- branch, receiptless failures/skips under the node name), so they
    -- compete BY SEQUENCE like everything else here — 'orElse' precedence
    -- let a stale branch-keyed Done shadow a later node-keyed failure.
    outcomeEntry =
      newestOf
        (lookupEvent OutcomeKind branch fold)
        (lookupEvent OutcomeKind (nodeName p) fold)
    recordedSplit = splitEntry >>= (splitRecordOf . snd)
    recordedPlan = maybe p (.splitPlan) recordedSplit

-- | Is a journaled amendment worth consuming?  An abandonment always is; an
-- amendment that neither restructures nor says anything is not.
substantiveAmendment :: ReplanDecision -> Bool
substantiveAmendment d =
  d.abandonSubtree
    || isJust d.amendedSubtree
    || not (T.null (T.strip d.amendedInstruction))

-- | The newer of two optional @(seq, event)@ entries.
newestOf :: Maybe (Int, JournalEvent) -> Maybe (Int, JournalEvent) -> Maybe (Int, JournalEvent)
newestOf a b = newestEntry a b Nothing

-- | The trace-stream tag for one resume verdict.  A skip is tagged by its
-- LADDERED doneness — the raw journaled outcome under-reports (bare-receipt
-- wire), and a trace that repeated the wire's optimism would mislead the
-- exact investigation it exists to serve.
verdictTag :: (Outcome -> Outcome) -> ResumePlan -> Text
verdictTag ladder = \case
  ResumeSkip o -> if outcomeIsDone (ladder o) then "skip-done" else "skip-failed"
  ResumeReplay {} -> "replay"
  ResumeFresh -> "fresh"
  ResumeAmend d _ -> if d.abandonSubtree then "amend-abandon" else "amend"

-- | Should a COMPLETED run re-enter?  True exactly when the fold's verdict
-- for the root is rescue-shaped: a pending (non-abandoning) amendment, or
-- the replan-rescue re-entry itself.  A Done root skips ('ResumeSkip'), an
-- un-amended failure stays terminal ('ResumeSkip'), and an empty fold is
-- never a reason to re-run ('ResumeFresh').
rescuePending :: (Outcome -> Outcome) -> ResumeFold -> Text -> DevPlan -> Bool
rescuePending ladder fold branch p = case resumePlanFor ladder fold branch p of
  ResumeSkip _ -> False
  ResumeFresh -> False
  ResumeReplay {} -> True
  ResumeAmend d _ -> not d.abandonSubtree

-- | Does any descendant branch's own resume verdict come out 'ResumeAmend'?
-- Walks the recorded plan tree (a journaled split plan carries its whole
-- subtree), resolving each child's branch through the fold's recorded
-- child-tree tables and falling back to the node name — the same keying
-- 'resumePlanFor' itself accepts.
descendantAmendPending :: (Outcome -> Outcome) -> ResumeFold -> DevPlan -> Bool
descendantAmendPending ladder fold p = any pending (childPlans p)
  where
    -- An ABANDONING amendment is not pending work (sol review, run 24: it
    -- previously re-entered a completed run only to re-refuse), and the
    -- recursion descends each child's RECORDED split plan when one exists —
    -- a formerly-leaf child that was re-split carries its amendment under
    -- topology only the journal knows.
    pending k = case resumePlanFor ladder fold (branchFor k) k of
      ResumeAmend d _ -> not d.abandonSubtree
      _ -> descendantAmendPending ladder fold (recordedPlanFor fold (branchFor k) k)
    branchFor k =
      fromMaybe
        (nodeName k)
        ( listToMaybe
            [ b
            | (_, _, SplitEvent {evChildTrees = Just ts}) <- eventsOfKind SplitKind fold
            , (n, b) <- ts
            , n == nodeName k
            ]
        )

-- | A branch's plan as the journal recorded it at split time, falling back
-- to the caller's copy when no split was journaled.
recordedPlanFor :: ResumeFold -> Text -> DevPlan -> DevPlan
recordedPlanFor fold branch p =
  fromMaybe p ((.splitPlan) <$> (lookupEvent SplitKind branch fold >>= splitRecordOf . snd))

-- | Select the highest-sequence journal entry from the three resume namespaces.
newestEntry
  :: Maybe (Int, JournalEvent)
  -> Maybe (Int, JournalEvent)
  -> Maybe (Int, JournalEvent)
  -> Maybe (Int, JournalEvent)
newestEntry replan split outcome = foldr newest Nothing (catMaybes [replan, split, outcome])
  where
    newest candidate Nothing = Just candidate
    newest candidate@(candidateSeq, _) current@(Just (currentSeq, _))
      | candidateSeq > currentSeq = Just candidate
      | otherwise = current

-- | Rebuild the higher-level 'SplitRecord' from a decoded 'SplitEvent'.
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

-- | Apply a journaled amendment to a node's plan.  A replacement subtree
-- (replan-as-decompose) wins over a rephrased instruction; either way the
-- node's own name survives, because retained worktrees rebind by name and a
-- renamed root would orphan the tree the failed attempt left behind.
--
-- The replacement is model-produced and VALIDATED here, never trusted
-- wholesale (sol review): every boundary path in the subtree must stay
-- within the failed node's own boundary (an unbounded original accepts
-- any), and the subtree's names must be unique.  A violating subtree
-- degrades to the instruction amendment — the amendment still applies,
-- just not the restructure — and the violation is reported so the trace
-- can carry it.  Depth/width/cycles need no check here: the coalgebra
-- middleware re-guards them at unfold.
amendPlan :: ReplanDecision -> DevPlan -> DevPlan
amendPlan d p = fst (amendPlanChecked d p)

amendPlanChecked :: ReplanDecision -> DevPlan -> (DevPlan, Maybe Text)
amendPlanChecked d p = case d.amendedSubtree of
  -- Validation runs on the RENAMED subtree — the name overwrite is part of
  -- what gets consumed, so validating before it would let the forced root
  -- name collide with a child the model happened to name after the failed
  -- node itself.
  Just sub ->
    let renamed = sub {nodeName = nodeName p}
     in case subtreeViolation renamed of
          Nothing -> (renamed, Nothing)
          Just why -> (instructionOnly, Just why)
  Nothing -> (instructionOnly, Nothing)
  where
    instructionOnly
      | T.null (T.strip d.amendedInstruction) = p
      | otherwise = p {nodeTask = d.amendedInstruction}
    subtreeViolation sub =
      boundaryEscape sub `orElse` duplicateName sub `orElse` badName sub
    -- The subtree's write set must stay inside what the FAILED NODE was
    -- allowed to write — its product boundary plus its tolerated hygiene
    -- paths.  (Tolerated entries like README.md legitimately live outside
    -- the product boundary; requiring them inside it rejected honest
    -- restructures.)
    boundaryEscape sub
      | null parentEntries = Nothing
      | otherwise = case traverse parseRepoPath parentEntries of
          Left why -> Just [fmt|the failed node's own boundary is unparseable: {why}|]
          Right parents -> listToMaybe (concatMap (escapes parents) (subtreeWriteable sub))
      where
        parentEntries = nodeBoundary p <> nodeTolerated p
    escapes parents raw = case parseRepoPath raw of
      Left why -> [[fmt|replacement subtree path {raw} is unparseable: {why}|]]
      Right b
        | any (pathWithin b) parents -> []
        | otherwise -> [[fmt|replacement subtree path {raw} escapes the failed node's boundary|]]
    duplicateName sub =
      listToMaybe [[fmt|replacement subtree repeats node name {n}|] | n <- duplicateNames sub]
    -- The initial proposal gate ('Harness.proposalViolation') refuses a
    -- name that cannot become a git ref; a replan-supplied subtree names
    -- new nodes through a completely different path and was not previously
    -- held to the same bar — it could pass here and only fail later, as an
    -- opaque worktree denial mid-unfold.
    badName sub =
      listToMaybe [[fmt|replacement subtree node name {n} is not a safe branch segment|] | n <- unsafeNames sub]

-- | A split that already happened: replay its recorded plan and child trees.
replaySplit
  :: ResumeHooks
  -> ResumeFold
  -> NodeSeed
  -> SplitRecord
  -> Harness (Swarm.PlanF NodeWork NodeSeed)
replaySplit hooks fold seed sp = do
  changed <- checkHeadChanged seed.seedTree sp.splitPlan sp.splitScaffoldHead
  case changed of
    Nothing -> unfoldChildren
    Just hc -> do
      finished <- integrationComplete hooks fold seed sp
      if finished
        then do
          vo <- verifyOrphan fold hc
          let o = adopt vo
          -- Adoption is the resume branch most worth narrating: it is the
          -- one that turns found commits into a verdict.
          trace
            "resume-adopt"
            (branchOf seed.seedTree)
            (object ["node" .= nodeName sp.splitPlan, "head" .= renderGitOid hc.hcFound, "verified" .= outcomeIsDone (hooks.resumeFoldLadder o)])
          pure (Swarm.PlanF (adoptedWork recorded o) [])
        else unfoldChildren
  where
    recorded = seed {seedPlan = sp.splitPlan}
    -- 'sp.splitChildTrees' is whatever was already durably bound (possibly
    -- from a prior process's incremental writes) — passed as the replay's
    -- OWN starting point, so a resumed allocation continues the same
    -- incremental-journaling discipline a fresh split gets, instead of
    -- silently batching every remaining child into one deferred write.
    unfoldChildren = do
      (childSeeds, denied) <-
        hooks.resumeAllocate
          (JournalKey (branchOf seed.seedTree))
          sp.splitPlan
          sp.splitScaffoldHead
          sp.splitChildTrees
          recorded
          (childPlans sp.splitPlan)
          (hooks.resumeRetainedChild recorded sp.splitChildTrees)
      pure (Swarm.PlanF (hooks.resumeSplitWork recorded Nothing childSeeds denied) childSeeds)

-- | Did the crashed process finish folding this node's children into it?
integrationComplete :: ResumeHooks -> ResumeFold -> NodeSeed -> SplitRecord -> Harness Bool
integrationComplete hooks fold seed sp
  | length doneBranches /= length (childPlans sp.splitPlan) = pure False
  | otherwise = and <$> traverse merged doneBranches
  where
    doneBranches =
      [ branch
      | k <- childPlans sp.splitPlan
      , Just branch <- [lookup (nodeName k) sp.splitChildTrees]
      , recordedDone hooks fold branch (nodeName k)
      ]
    -- 'isAncestor' keeps git's three answers apart; only an OBSERVED
    -- not-an-ancestor (exit 1) means unmerged.  An infra failure degrades to
    -- "not complete" — re-unfolding is the recoverable direction — but never
    -- silently equates a bad object with an honest no.
    merged b =
      isAncestor seed.seedTree b "HEAD" >>= \case
        Right r -> pure r
        Left _ -> pure False

-- | A child is complete only when its recorded outcome passes the same ladder
-- used by the current fold.
recordedDone :: ResumeHooks -> ResumeFold -> Text -> Text -> Bool
recordedDone hooks fold branch node =
  -- Branch-keyed and node-keyed outcomes compete by SEQUENCE, exactly as in
  -- 'resumePlanFor': a stale branch-keyed Done must not shadow a later
  -- node-keyed failure into "already integrated".
  case newestOf (lookupEvent OutcomeKind branch fold) (lookupEvent OutcomeKind node fold) of
    Just (_, OutcomeEvent {evOutcome = o}) -> outcomeIsDone (hooks.resumeFoldLadder o)
    _ -> False

-- | Adopt verified work in a retained tree, or continue through the ordinary
-- coalgebra when the tree is genuinely unstarted or is an interior scaffold.
adoptOrUnfold
  :: ResumeHooks
  -> ResumeFold
  -> Swarm.Coalg Harness NodeWork NodeSeed
  -> NodeSeed
  -> Harness (Swarm.PlanF NodeWork NodeSeed)
adoptOrUnfold hooks fold inner seed = do
  changed <- checkHeadChanged seed.seedTree seed.seedPlan baseline
  case changed of
    Nothing -> inner seed
    Just hc -> case planShape seed.seedPlan of
      LeafPlan SplitLeaf {}
        | not (microSequenceComplete fold (branchOf seed.seedTree)) -> inner seed
      LeafPlan _ -> do
          vo <- verifyOrphan fold hc
          let o = adopt vo
          trace
            "resume-adopt"
            (branchOf seed.seedTree)
            (object ["node" .= nodeName seed.seedPlan, "head" .= renderGitOid hc.hcFound, "verified" .= outcomeIsDone (hooks.resumeFoldLadder o)])
          pure (Swarm.PlanF (adoptedWork seed o) [])
      BranchPlan {} ->
          -- An INTERIOR node's scaffold commit is not the final
          -- deliverable — its own nodeChecks validate the MERGED tree, not
          -- a pre-split scaffold, so judging a scaffold orphan by them
          -- (via 'verifyOrphan') would reject exactly the valid commit
          -- adoption exists to rescue.  'verifyScaffoldOrphan' holds it to
          -- the bar that actually applies at this point: did the scaffold
          -- stay inside its boundary, and did it leave the tree clean.
          verifyScaffoldOrphan hc >>= \case
            Right () -> do
              trace
                "resume-adopt"
                (branchOf seed.seedTree)
                (object ["node" .= nodeName seed.seedPlan, "head" .= renderGitOid hc.hcFound, "verified" .= True, "scaffold" .= True])
              inner seed {seedAdopted = Just hc.hcFound}
            Left why -> do
              trace
                "resume-adopt"
                (branchOf seed.seedTree)
                (object ["node" .= nodeName seed.seedPlan, "head" .= renderGitOid hc.hcFound, "verified" .= False, "scaffold" .= True, "why" .= why])
              pure
                ( Swarm.PlanF
                    (adoptedWork seed (failedOutcome (nodeName seed.seedPlan) (Failure InfraFailure [fmt|scaffold orphan could not be verified — {why}|] []) Nothing))
                    []
                )
  where
    baseline = renderGitOid seed.seedTree.handleReceipt.sourceHead

-- | The bar a SCAFFOLD orphan (an interior node's committed-but-unsplit
-- scaffold work) must clear: it stayed inside its declared boundary, and
-- the worktree is clean.  Deliberately NOT the node's own 'nodeChecks' —
-- see 'adoptOrUnfold'.
verifyScaffoldOrphan :: HeadChanged -> Harness (Either Text ())
verifyScaffoldOrphan hc =
  statusEntries tree >>= \case
    Left f -> pure (Left [fmt|status check could not run — {renderGitFailure f}|])
    Right (_ : _) -> pure (Left "the retained worktree is dirty at adoption")
    Right [] ->
      boundaryViolations tree (nodeBoundary p) (nodeTolerated p) >>= \case
        Left why -> pure (Left [fmt|boundary could not be checked — {why}|])
        Right ([], _) -> pure (Right ())
        Right (outside, _) -> pure (Left [fmt|scaffold strayed outside its boundary: {T.intercalate ", " outside}|])
  where
    tree = hc.hcTree
    p = hc.hcPlan

-- | A micro-split orphan is adoptable only after its accepted and completion
-- journal events agree on the exact names.
microSequenceComplete :: ResumeFold -> Text -> Bool
microSequenceComplete fold branch = case (acceptedEntry, completeEntry) of
  (Just (_, MicroSplitEvent {evMicrotaskNames = accepted}), Just (_, MicroCompleteEvent {evMicrotaskNames = completed})) ->
    accepted == completed
  _ -> False
  where
    acceptedEntry = lookupEvent MicroSplitKind branch fold
    completeEntry = lookupEvent MicroCompleteKind branch fold

-- ---------------------------------------------------------------------------
-- The worktree-adoption typestate
-- ---------------------------------------------------------------------------

-- | A worktree handle proven to have come back through the retained-worktree
-- registry's own rebind.  The one mint point is 'retainWorktree'.
newtype RetainedWorktree = RetainedWorktree WorktreeHandle

retainedHandle :: RetainedWorktree -> WorktreeHandle
retainedHandle (RetainedWorktree h) = h

-- | Look a retained worktree up by branch.  Rebind, never recreate.
retainWorktree :: Text -> Harness (Either Text RetainedWorktree)
retainWorktree branch = do
  listed <- listWorktrees
  case listed of
    Left err -> pure (Left (renderWorktreeError err))
    Right trees -> retainListed trees
  where
    retainListed trees =
      case [s | s <- trees, renderBranchName s.summaryReceipt.branch == branch] of
        [] -> pure (Left [fmt|no retained worktree is registered for branch {branch}|])
        -- Two registered worktrees claiming one branch is a real corrupted
        -- state, and rebinding to whichever listed first would silently work on
        -- a coin flip.
        (_ : _ : _) ->
          pure (Left [fmt|more than one retained worktree claims branch {branch} — refusing to guess which to rebind|])
        [s]
          | not s.present ->
              pure
                ( Left
                    [fmt|retained worktree {renderWorktreeId s.summaryReceipt.treeId} for {branch} is gone from disk (WorktreeLost); it is never recreated|]
                )
          | otherwise ->
              lookupWorktree s.summaryReceipt.treeId >>= \case
                Left err -> pure (Left (renderWorktreeError err))
                Right h -> pure (Right (RetainedWorktree h))

-- | A retained tree whose HEAD differs from its baseline: an orphan candidate,
-- not yet trusted.
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

-- | A candidate checked by the orchestrator at the found sha.  Only 'adopt'
-- can unwrap this capability.
newtype VerifiedOrphan = VerifiedOrphan Outcome

verifyOrphan :: ResumeFold -> HeadChanged -> Harness VerifiedOrphan
verifyOrphan fold hc = do
  dirty <- statusEntries tree
  checks <- runChecks tree p
  boundary <- boundaryViolations tree (nodeBoundary p) (nodeTolerated p)
  case (dirty, boundary) of
    -- "Verified" means clean: a crash can leave committed progress PLUS
    -- uncommitted residue on top of it, and adopting the committed part
    -- while silently carrying the residue forward is exactly the kind of
    -- unearned trust this whole ladder exists to refuse.
    (Left f, _) ->
      pure (VerifiedOrphan (failedOutcome name (Failure InfraFailure [fmt|{name}: adoption status check could not run — {renderGitFailure f}|] []) Nothing))
    (Right (_ : _), _) ->
      pure (VerifiedOrphan (failedOutcome name (Failure InfraFailure [fmt|{name}: retained worktree is dirty at adoption — refusing to adopt residue alongside the committed work|] []) Nothing))
    -- An adoption whose boundary cannot be CHECKED is not verified — the
    -- word means something.  Loud typed failure; resume surfaces it.
    (Right [], Left why) ->
      pure
        ( VerifiedOrphan
            ( failedOutcome
                name
                (Failure InfraFailure [fmt|{name}: adoption boundary check could not run — {why}|] [])
                Nothing
            )
        )
    (Right [], Right (outside, tolerated)) ->
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
                  , -- The WHOLE journaled rebase history for this branch —
                    -- a branch that escalated at step 1 and rebased cleanly
                    -- at step 3 adopts with both facts, not just the last.
                    receiptRebases =
                      [n | (k, _, RebaseEvent {evNote = n}) <- eventsOfKind RebaseKind fold, k == branch]
                  , receiptOutside = outside
                  , receiptCycles = 0
                  , -- THIS process ran no agent; the crashed one presumably
                    -- did, but a receipt records observations, not
                    -- presumptions.  The commits themselves are the
                    -- evidence, verified by the checks above.
                    receiptAgentRan = False
                  , receiptReviewed = False
                  , receiptNoOp = Nothing
                  , receiptSummary =
                      [fmt|Adopted work found in this retained worktree at {renderGitOid hc.hcFound}: run {fold.resumeRunId} left it there and crashed before recording an outcome.|]
                  , receiptEvidence =
                      [fmt|orphaned commits {hc.hcBaseline}..{renderGitOid hc.hcFound}, verified by this orchestrator at that sha|]
                        : priorEscalationsFor fold branch
                        <> map ("tolerated: " <>) tolerated
                  }
            )
        )
  where
    tree = hc.hcTree
    p = hc.hcPlan
    name = nodeName p
    branch = branchOf tree

-- | Deliver a verified orphan's outcome — the only function that unwraps it.
adopt :: VerifiedOrphan -> Outcome
adopt (VerifiedOrphan o) = o

-- | EVERY journaled escalation for this branch, not just the last one under
-- the key.
priorEscalationsFor :: ResumeFold -> Text -> [Text]
priorEscalationsFor fold branch =
  [ [fmt|prior escalation: {why}|]
  | (k, _, EscalationEvent {evEscDetail = why}) <- eventsOfKind EscalationKind fold
  , k == branch
  ]

-- | The retained root worktree's branch, named structurally by the fold.
-- The 'RootTreeEvent' written at worktree CREATION is the primary source —
-- it exists even when a crash landed inside the root scaffold cycle, before
-- any split or outcome was recorded; the split/outcome fallbacks keep
-- journals from before that event decodable in spirit.
rootBranchOf :: ResumeFold -> Text -> Maybe Text
rootBranchOf fold node =
  listToMaybe
    ( [key | (key, _, RootTreeEvent {}) <- eventsOfKind RootTreeKind fold]
        <> [key | (key, _, SplitEvent {evSplitPlan = pl}) <- eventsOfKind SplitKind fold, nodeName pl == node]
        <> [ key
           | (key, _, OutcomeEvent {evOutcome = o}) <- eventsOfKind OutcomeKind fold
           , outcomeNodeName o == node
           , key /= node
           ]
    )

replayedWork :: NodeSeed -> Outcome -> NodeWork
replayedWork seed o = WorkResumed {workSeed = seed, workOutcome = ReplayedOutcome o}

adoptedWork :: NodeSeed -> Outcome -> NodeWork
adoptedWork seed o = WorkResumed {workSeed = seed, workOutcome = AdoptedOutcome o}

orElse :: Maybe a -> Maybe a -> Maybe a
orElse (Just a) _ = Just a
orElse Nothing b = b
