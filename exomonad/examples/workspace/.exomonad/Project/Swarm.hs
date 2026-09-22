{-# LANGUAGE OverloadedStrings #-}

-- | One scaffold/fork/fold batch. A worker may run this same protocol for its
-- children, then submit the folded revision to its own parent. The supervisor
-- supplies obligations; workers never need to author the orchestration code.
--
-- This is an executable policy specification. The driver connects requests to
-- Shoal's existing fork, command, review and worktree owners. It must persist
-- the state and outstanding requests before dispatch, correlate replies, and
-- reconcile uncertain external operations rather than replaying them.
module Project.Swarm
  ( Child, Ticket, Batch, Frontier (..), Limits (..)
  , Request (..), Action (..), Reply (..), Verdict (..), MergeResult (..)
  , Notice (..), Wake (..), View (..), ChildState (..), Problem (..)
  , begin, advance, view, complete
  ) where

import Data.List (find, nub)
import Data.List.NonEmpty (NonEmpty ((:|)))
import Data.Text (Text)
import qualified Data.Text as Text
import Numeric.Natural (Natural)

type Child = Text

-- A ticket identifies one operation within this batch's mailbox. Review
-- replies contain judgments, never a replacement candidate or assignment.
newtype Ticket = Ticket Natural deriving (Show, Eq)

data Frontier = AllChildren | After (NonEmpty Child) deriving (Show, Eq)

data Limits = Limits
  { repairLimit :: Natural
  , rebaseLimit :: Natural
  } deriving (Show, Eq)

data Verdict
  = Accept
  | Fix (NonEmpty Text)
  | ContractQuestion Text
  deriving (Show, Eq)

-- Merge results are observations from the integration owner. Applied means
-- the merge and the specified integration checks succeeded at this revision.
-- A failed check retains the observed head for bounded repair under the merge
-- owner's exclusive custody. Uncertain mutation pauses folding for the owner.
data MergeResult revision
  = Applied revision
  | RebaseRequired revision
  | IntegrationFailed revision Text
  | MergeUncertain Text
  deriving (Show, Eq)

data Action task revision
  = Work task revision
  | CheckAndReview task revision
  | RepairWork task revision (NonEmpty Text)
  | RebaseWork task revision revision
  | Merge revision revision
  | RepairIntegration revision (NonEmpty Text)
  | CheckIntegration revision
  deriving (Show, Eq)

-- RepairIntegration and CheckIntegration use the enclosing node's integration
-- contract, held by the driver. They block further merges until fresh checks
-- and review accept the repaired head. They share this child's repair budget.
-- Like Merge, they run under the integration owner's checkout and authority;
-- the enclosing Run's child identifies the obligation, not execution custody.
-- A repair's Produced reply reports the observed parent head after applying
-- the repair there, not an unintegrated revision in the repair worker's tree.

-- Work/repair/rebase report the observed committed worktree head. CheckAndReview
-- runs the actual configured checks, then a fresh reviewer against the same
-- revision and enclosing invariants. It returns Fix for a bounded code repair,
-- ContractQuestion for an unsettled premise, and Accept only when both passed.
data Reply revision
  = Produced revision
  | Reviewed Verdict
  | Integrated (MergeResult revision)
  | Failed Text
  deriving (Show, Eq)

data Notice revision = ChildMerged
  { mergedChild :: Child
  , candidateRevision :: revision
  , integratedRevision :: revision
  } deriving (Show, Eq)

data Wake
  = FrontierReady
  | BatchSettled
  | ChildNeedsDecision Child Text
  | IntegrationNeedsDecision Child Text
  deriving (Show, Eq)

data Request task revision
  = Run Child Ticket (Action task revision)
  | QueueNotice (Notice revision)
  | BaseAdvanced Child revision
  | WakeParent [Wake]
  deriving (Show, Eq)

-- QueueNotice and BaseAdvanced do not start inference. WakeParent is a
-- coalescible wake request: on activation the parent reads 'view', rather than
-- treating a snapshot embedded in an older notification as the current head.
data ChildState revision
  = InFlight
  | Landed revision revision
  | NeedsDecision Text
  deriving (Show, Eq)

data View revision = View
  { currentHead :: revision
  , children :: [(Child, ChildState revision)]
  , mergeNotices :: [Notice revision]
  , integrationPaused :: Bool
  } deriving (Show, Eq)

data Problem
  = EmptyBatch
  | DuplicateChildren
  | UnknownFrontierChild Child
  | UnknownChild Child
  | StaleReply Child Ticket
  | UnexpectedReply Child Ticket
  deriving (Show, Eq)

data Phase revision
  = Working Ticket
  | Assessing Ticket revision
  | Repairing Ticket revision
  | Rebasing Ticket revision
  | Ready revision
  | Merging Ticket revision
  | FixingIntegration Ticket revision
  | AssessingIntegration Ticket revision revision
  | Done revision revision
  | Stopped Text
  deriving (Show, Eq)

data Entry task revision = Entry
  { childName :: Child
  , childTask :: task
  , phase :: Phase revision
  , repairs :: Natural
  , rebases :: Natural
  } deriving (Show, Eq)

data Batch task revision = Batch
  { headRevision :: revision
  , entries :: [Entry task revision]
  , limits :: Limits
  , frontier :: [Child]
  , frontierWoken :: Bool
  , settledWoken :: Bool
  , paused :: Bool
  , nextTicket :: Natural
  , notices :: [Notice revision]
  } deriving (Show, Eq)

-- Every child starts at the same committed scaffold. Children are launched
-- together; independent completions may arrive in any order. Only one merge
-- is in flight, and ready candidates fold in declaration order among those
-- currently ready, without waiting for a slow unrelated child.
begin
  :: Limits -> Frontier -> revision -> [(Child, task)]
  -> Either Problem (Batch task revision, [Request task revision])
begin policy requested base tasks
  | null tasks = Left EmptyBatch
  | length names /= length (nub names) = Left DuplicateChildren
  | Just unknown <- find (`notElem` names) wanted = Left (UnknownFrontierChild unknown)
  | otherwise = Right
      ( Batch
          { headRevision = base
          , entries = initial
          , limits = policy
          , frontier = wanted
          , frontierWoken = False
          , settledWoken = False
          , paused = False
          , nextTicket = fromIntegral (length tasks)
          , notices = []
          }
      , [Run name (Ticket n) (Work task base) | (n, (name, task)) <- numbered]
      )
  where
    names = map fst tasks
    wanted = case requested of
      AllChildren -> names
      After selected -> foldr (:) [] selected
    numbered = zip [0 ..] tasks
    initial = [Entry name task (Working (Ticket n)) 0 0 | (n, (name, task)) <- numbered]

-- Stale or duplicate replies have no effects. The driver keeps their receipts
-- for diagnosis; it must not turn an old review into approval of a repair.
advance
  :: Child -> Ticket -> Reply revision -> Batch task revision
  -> Either Problem (Batch task revision, [Request task revision])
advance name ticket reply batch = do
  entry <- maybe (Left (UnknownChild name)) Right (find ((== name) . childName) (entries batch))
  if activeTicket (phase entry) /= Just ticket
    then Left (StaleReply name ticket)
    else do
      (updated, actions, reasons) <- settle entry
      let (scheduled, runs) = schedule updated
          (awakened, wakes) = wakeFrontiers scheduled
          allReasons = reasons ++ wakes
      pure (awakened, actions ++ runs ++ [WakeParent allReasons | not (null allReasons)])
  where
    settle entry = case (phase entry, reply) of
      (Working _, Produced revision) -> assess entry revision
      (Repairing _ _, Produced revision) -> assess entry revision
      (Rebasing _ _, Produced revision) -> assess entry revision
      (Assessing _ revision, Reviewed Accept) ->
        pure (replace (entry { phase = Ready revision }) batch, [], [])
      (Assessing _ revision, Reviewed (Fix findings))
        | repairs entry < repairLimit (limits batch) ->
            issue (entry { repairs = repairs entry + 1 })
              (\t -> Repairing t revision) (RepairWork (childTask entry) revision findings)
        | otherwise -> stop entry ("Repair budget exhausted: " <> Text.intercalate "; " (foldr (:) [] findings))
      (Assessing _ _, Reviewed (ContractQuestion reason)) -> stop entry reason
      (Merging _ candidate, Integrated (Applied revision)) -> merged entry candidate revision
      (Merging _ candidate, Integrated (RebaseRequired base))
        | rebases entry < rebaseLimit (limits batch) ->
            let (updated, request) = issueRequest
                  (entry { rebases = rebases entry + 1 })
                  (\t -> Rebasing t candidate) (RebaseWork (childTask entry) candidate base)
                  (batch { headRevision = base })
            in pure (updated, baseChanges name base updated ++ [request], [])
        | otherwise ->
            let stopped = replace (entry { phase = Stopped rebaseExhausted })
                  (batch { headRevision = base })
            in pure (stopped, baseChanges name base stopped, [ChildNeedsDecision name rebaseExhausted])
      (Merging _ candidate, Integrated (IntegrationFailed revision reason)) ->
        repairIntegration entry candidate revision (reason :| [])
      (FixingIntegration _ candidate, Produced revision) ->
        let (updated, request) = issueRequest entry
              (\t -> AssessingIntegration t candidate revision) (CheckIntegration revision)
              (batch { headRevision = revision })
        in pure (updated, [request], [])
      (AssessingIntegration _ candidate revision, Reviewed Accept) -> merged entry candidate revision
      (AssessingIntegration _ candidate revision, Reviewed (Fix findings)) ->
        repairIntegration entry candidate revision findings
      (AssessingIntegration _ _ _, Reviewed (ContractQuestion reason)) -> halt entry batch reason
      (Merging _ _, Integrated (MergeUncertain reason)) -> halt entry batch reason
      (Merging _ _, Failed reason) -> halt entry batch reason
      (FixingIntegration _ _, Failed reason) -> halt entry batch reason
      (AssessingIntegration _ _ _, Failed reason) -> halt entry batch reason
      (_, Failed reason) -> stop entry reason
      _ -> Left (UnexpectedReply name ticket)
    assess entry revision = issue entry (\t -> Assessing t revision)
      (CheckAndReview (childTask entry) revision)
    issue entry next action =
      let (updated, request) = issueRequest entry next action batch
      in pure (updated, [request], [])
    merged entry candidate revision =
      let notice = ChildMerged name candidate revision
          updated = replace (entry { phase = Done candidate revision }) batch
            { headRevision = revision, notices = notices batch ++ [notice], paused = False }
      in pure (updated, QueueNotice notice : baseChanges name revision updated, [])
    repairIntegration entry candidate revision findings
      | repairs entry < repairLimit (limits batch) =
          let (updated, request) = issueRequest (entry { repairs = repairs entry + 1 })
                (\t -> FixingIntegration t candidate) (RepairIntegration revision findings)
                (batch { headRevision = revision, paused = True })
          in pure (updated, [request], [])
      | otherwise = halt entry (batch { headRevision = revision })
          ("Integration repair budget exhausted: " <> Text.intercalate "; " (foldr (:) [] findings))
    stop entry reason =
      pure (replace (entry { phase = Stopped reason }) batch, [], [ChildNeedsDecision name reason])
    halt entry state reason = pure
      (replace (entry { phase = Stopped reason }) (state { paused = True }),
       [], [IntegrationNeedsDecision name reason])
    rebaseExhausted = "Rebase budget exhausted"

activeTicket :: Phase revision -> Maybe Ticket
activeTicket current = case current of
  Working t -> Just t
  Assessing t _ -> Just t
  Repairing t _ -> Just t
  Rebasing t _ -> Just t
  Merging t _ -> Just t
  FixingIntegration t _ -> Just t
  AssessingIntegration t _ _ -> Just t
  _ -> Nothing

replace :: Entry task revision -> Batch task revision -> Batch task revision
replace entry batch = batch { entries =
  [if childName old == childName entry then entry else old | old <- entries batch] }

baseChanges :: Child -> revision -> Batch task revision -> [Request task revision]
baseChanges except revision batch =
  [BaseAdvanced (childName entry) revision
  | entry <- entries batch, childName entry /= except, isOpen (phase entry)]

issueRequest
  :: Entry task revision -> (Ticket -> Phase revision) -> Action task revision
  -> Batch task revision -> (Batch task revision, Request task revision)
issueRequest entry next action batch =
  let ticket = Ticket (nextTicket batch)
      updated = replace (entry { phase = next ticket }) batch
  in (updated { nextTicket = nextTicket batch + 1 }, Run (childName entry) ticket action)

schedule :: Batch task revision -> (Batch task revision, [Request task revision])
schedule batch
  | paused batch || any merging (entries batch) = (batch, [])
  | otherwise = case [(entry, revision) | entry <- entries batch, Ready revision <- [phase entry]] of
      [] -> (batch, [])
      (entry, revision) : _ ->
        let (updated, request) = issueRequest entry (\t -> Merging t revision)
              (Merge (headRevision batch) revision) batch
        in (updated, [request])
  where
    merging entry = case phase entry of { Merging _ _ -> True; _ -> False }

isOpen :: Phase revision -> Bool
isOpen current = case current of { Done _ _ -> False; Stopped _ -> False; _ -> True }

complete :: Batch task revision -> Bool
complete = all (not . isOpen . phase) . entries

wakeFrontiers :: Batch task revision -> (Batch task revision, [Wake])
wakeFrontiers batch
  | paused batch = (batch, [])
  | otherwise =
      let ready = all (not . isOpen . phase)
            [entry | entry <- entries batch, childName entry `elem` frontier batch]
          finished = complete batch
          wakeAll = finished && not (settledWoken batch)
          wakeEarly = ready && not (frontierWoken batch) && not wakeAll
      in (batch { frontierWoken = frontierWoken batch || ready,
                  settledWoken = settledWoken batch || finished },
          [FrontierReady | wakeEarly] ++ [BatchSettled | wakeAll])

view :: Batch task revision -> View revision
view batch = View (headRevision batch)
  [(childName entry, status (phase entry)) | entry <- entries batch]
  (notices batch) (paused batch)
  where
    status current = case current of
      Done candidate revision -> Landed candidate revision
      Stopped reason -> NeedsDecision reason
      _ -> InFlight
