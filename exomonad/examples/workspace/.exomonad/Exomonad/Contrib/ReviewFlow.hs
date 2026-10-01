{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- One Task's committed candidate, fresh review, and bounded same-child repair.
-- An optional merge target publishes checked local delivery. The owner retains
-- product acceptance, scope decisions and deliberate resource retirement.
module Exomonad.Contrib.ReviewFlow
  ( ReviewFlow (reviewSnapshot, reviewCleanup)
  , ReviewFlowState (..), flowQuestions
  , ReviewCleanupRequest (..)
  , ReviewCleanupResult (..)
  , ReviewStage (..)
  , ReviewCompletion (..)
  , ReviewStop (..)
  , ReviewChoice (..)
  , ReviewContext (..)
  , ReviewRouteResult (..)
  , ReviewRouteEvidence (..)
  , ReviewSourcePlan (..)
  , ReviewFlowPolicy (..)
  , reviewFlow
  , reviewFlowWith
  , checkedReviewFlow
  , ReviewRun (..), startReviewFlow, startReviewFlowWith
  ) where

import GHC.Generics (Generic)
import Control.Monad.Freer (Eff, Member, raise)
import qualified Control.Monad.Freer.State as S
import Data.List (nub)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (GitRef (..), Jev, WorktreeHandle (..), WorktreeIntegration, ActorLocal, Commands)
import Tidepool.Actors.Worktree (boundWorktree)
import Tidepool.Worktree (WorktreeReceipt (..), renderGitOid, renderWorktreeError, renderWorktreeId)
import qualified Exomonad.Contrib.Merge as Merge
import Tidepool.Agent.Reply (requestIdNumber)
import Exomonad.Contrib.CheckResults
import Exomonad.Contrib.CheckPlan (PlanCheck (..), PlanStart (..), PlanReport (..), startCheckPlanInto, planPassed, planSummary)
import Exomonad.Contrib.Types
import Exomonad.Contrib.Routing (WorkEvent, workChange, workQuestionsMessage)

data ReviewChoice = HonorReview | EscalateReview Text
  deriving (Show, Eq)

data ReviewContext = ReviewContext
  { routeTask :: Task
  , routeCandidate :: Candidate
  , routeDecision :: ReviewDecision
  , routeRepairCount :: Int
  , routeRepairLimit :: Int
  , routeEscalationCriteria :: [Text]
  } deriving (Show)

data ReviewRouteEvidence
  = DeterministicRoute
  | RouteCriteriaMissing
  | JevRouteSelected Text Text Double Double Text
  | JevRouteDoubted Text Text Double Double Text
  | JevRouteUnavailable Text
  deriving (Show)

data ReviewRouteResult = ReviewRouteResult
  { routeChoice :: ReviewChoice
  , routeEvidence :: ReviewRouteEvidence
  } deriving (Show)

-- ComponentReview deliberately reviews only the submitted component. A
-- product review declares the sibling commits that must already be in it.
data ReviewSourcePlan = ComponentReview | RequiresSiblingCommits [GitOid]
  deriving (Show, Eq)

-- Disposition may stop on a project-specific finding, but cannot upgrade a
-- repair request into acceptance. The exact candidate check runs first.
data ReviewFlowPolicy = ReviewFlowPolicy
  { flowRepairLimit :: Int
  , flowSourcePlan :: ReviewSourcePlan
  -- Used by reviewFlow; reviewFlowWith supplies an effectful choice instead.
  , flowReviewChoice :: Candidate -> ReviewDecision -> ReviewChoice
  , flowEscalationCriteria :: [Text]
  , flowNotice :: ReviewStage -> Maybe Text
  -- A supplied continuation receives the exact terminal stage and the
  -- implementer's original request identity for correlation.
  , flowCompleted :: Maybe (R.Send ReviewCompletion)
  , flowIntegration :: Maybe Merge.MergeTarget
  , flowReviewer :: ReviewSourcePlan -> ReviewRequest -> Text -> Branch ResearchLeafEffects ReviewRequest (Outcome ReviewDecision)
  , flowCorrectionInstructions :: Text
  , flowRepairInstructions :: Text
  }

data ReviewStop
  = CandidateUnavailable ResponseFailure
  | CandidateBlocked Text [Text]
  | CandidateSourceRefused Text
  | CandidateReceiptMismatch RequestId RequestId
  | RequiredSiblingMissing GitOid GitOid
  | SourcePreflightUnavailable Text
  | CandidateChecksRefused Text
  | CandidateChecksUnknown Text
  | SourceAttachmentRefused RequestId R.AttachmentError
  | InvalidRepairLimit Int
  | ReviewerUnavailable ResponseFailure
  | ReviewerAdmissionRefused Text
  | ReviewerBlocked Text [Text]
  | ReviewerSourceRefused Text
  | ReviewerReceiptMismatch RequestId RequestId
  | ReviewerWithoutRequest
  | ReviewerSourceMismatch GitOid GitOid
  | ReviewerCandidateMismatch Candidate Candidate
  | ReviewerScopeMismatch
  | ReviewerEvidenceRefused ReviewEvidenceIssue
  | EmptyRepairFindings Candidate
  | ReviewEscalated Text
  | RepairBudgetSpent Int Candidate [Text]
  | IntegrationRefused Merge.MergeResult
  deriving (Show)

data ReviewStage
  = AwaitingCandidate
  | CheckingCandidate Candidate PlanStart
  | ReviewingCandidate Candidate
  | AwaitingReviewCorrection Candidate
  | AwaitingRepair Candidate
  | ReviewAccepted ReviewedCandidate
  | ReviewIntegrated ReviewedCandidate Merge.MergeResult
  | ReviewStopped ReviewStop
  deriving (Show)

-- | The exact reviewer response stays owned by ReviewFlow. Only its admitted
-- proof crosses to the coordinator; a copied Accepted verdict cannot do so.
data ReviewCompletion
  = ReviewApproved RequestId ReviewedCheckpoint
  | ReviewPublished RequestId ReviewedCheckpoint Merge.MergeResult
  | ReviewRefused RequestId ReviewStop
  deriving (Show)

-- Cleanup is requested after the owner has read the terminal result and
-- interviewed reviewers. A receipt records the actual stop outcome, including
-- refusals and resources still being released or retained. StoppedReleasing
-- remains unresolved here; the host's later release notice is observed
-- separately and is not inferred from this state or from R.finish.
data ReviewCleanupResult
  = ReviewCleanupNotOwner ActorInputOrigin
  | ReviewCleanupPending ReviewStage
  | ReviewCleanupNoReviewer
  | ReviewCleanupAttempted [(ForkGroupHandle, [CleanupReceipt])]
  deriving (Show)

data ReviewCleanupRequest = ReviewCleanupOnce | ReviewCleanupRetryRefused
  deriving (Show, Eq)

data ReviewFlowState = ReviewFlowState
  { flowStage :: ReviewStage
  , flowRepairCount :: Int
  , flowCorrectionUsed :: Bool
  , flowCandidateReceipts :: [Either ResponseFailure (ResponseResult (Outcome Candidate))]
  , flowReviewerRequests :: [Response (Outcome ReviewDecision)]
  , flowReviewerUpdates :: [Progress WorkProgress]
  , flowReviewerReceipts :: [(RequestId, Either ResponseFailure (ResponseResult (Outcome ReviewDecision)))]
  , flowRepairRequests :: [Response (Outcome Candidate)]
  , flowRepairUpdates :: [Progress WorkProgress]
  , flowAttachments :: [(RequestId, Either R.AttachmentError ())]
  , flowProgressUpdates :: [(RequestId, ProgressState WorkProgress)]
  , flowProgress :: [(RequestId, WorkProgress)]
  , flowNotices :: [Either NotificationError NotificationReceipt]
  , flowCleanupResult :: Maybe ReviewCleanupResult
  , flowReviewRoutes :: [(ReviewContext, ReviewRouteResult)]
  , flowCheckPlans :: [(Candidate, PlanStart)]
  , flowCheckReports :: [(Candidate, PlanReport)]
  , flowCheckCleanup :: [Actor.ActorExit CheckState]
  , flowReviewedProof :: Maybe ReviewedCheckpoint
  , flowCompletionAdmission :: Maybe (Either Text ())
  }

flowQuestions :: ReviewFlowState -> [(RequestId, Attention)]
flowQuestions = map (\(identity, progress) -> (identity, workQuestions progress)) . flowProgress

instance Show ReviewFlowState where
  show state = "ReviewFlowState " ++ show (flowStage state)
    ++ " repairs=" ++ show (flowRepairCount state)
    ++ " candidates=" ++ show (length (flowCandidateReceipts state))
    ++ " reviewers=" ++ show (length (flowReviewerRequests state))
    ++ " repairRequests=" ++ show (length (flowRepairRequests state))
    ++ " notices=" ++ show (length (flowNotices state))
    ++ " cleanup=" ++ show (flowCleanupResult state)
    ++ " completion=" ++ show (flowCompletionAdmission state)
    ++ " routes=" ++ show (length (flowReviewRoutes state))

-- Dynamic sources carry their original handle, so an older settlement cannot
-- advance a later repair or correction. Attachments are retained before use.
data ReviewEvent
  = ReviewerSettled (Response (Outcome ReviewDecision)) (Either ResponseFailure (ResponseResult (Outcome ReviewDecision)))
  | RepairSettled (Response (Outcome Candidate)) (Either ResponseFailure (ResponseResult (Outcome Candidate)))
  | WorkUpdated RequestId (ProgressState WorkProgress)

data ReviewFlow mode = ReviewFlow
  { flowStateField :: mode :- State ReviewFlowState
  , reviewSnapshot :: mode :- Call () (R.Reply ReviewFlowState)
  , reviewCleanup :: mode :- Call ReviewCleanupRequest (R.Reply ReviewCleanupResult)
  , firstCandidate :: mode :- Event (Either ResponseFailure (ResponseResult (Outcome Candidate)))
  , checksCompleted :: mode :- Call CheckState NoReply
  , reviewEvents :: mode :- Event ReviewEvent
  } deriving Generic

type ReviewFlowEffects = R.LocalEffects ReviewFlow ResearchEffects
type CheckedReviewEffects = R.LocalEffects ReviewFlow (WorktreeIntegration ': ResearchEffects)

data ReviewRun = ReviewRun
  { reviewRunFlow :: R.ActorHandle ReviewFlow
  , reviewRunWorktree :: WorktreeHandle
  }

-- Admission, exact-source check checkout, and result subscription are one
-- reusable operation. It never waits for the implementer in the admission cell.
startReviewFlow
  :: (Member Actor effects, Member WorktreeAllocation effects, Member BoundWorktree effects,
      Member Replies effects)
  => AgentRef -> Task -> ReviewFlowPolicy -> Response (Outcome Candidate) -> [PlanCheck]
  -> Eff effects (Either Text ReviewRun)
startReviewFlow owner task policy = startReviewFlowWith
  (\context -> pure (ReviewRouteResult
    (flowReviewChoice policy (routeCandidate context) (routeDecision context))
    DeterministicRoute)) owner task policy

startReviewFlowWith
  :: (Member Actor effects, Member WorktreeAllocation effects, Member BoundWorktree effects,
      Member Replies effects)
  => (forall selected. Member Jev selected => ReviewContext -> Eff selected ReviewRouteResult)
  -> AgentRef -> Task -> ReviewFlowPolicy -> Response (Outcome Candidate) -> [PlanCheck]
  -> Eff effects (Either Text ReviewRun)
startReviewFlowWith choose owner task policy worker checks
  | null checks = pure (Left "startReviewFlow: declare focused checks")
  | flowRepairLimit policy < 0 = pure (Left "startReviewFlow: negative repair budget")
  | otherwise = do
      bound <- boundWorktree
      case bound of
        Left issue -> pure (Left (Text.pack (show issue)))
        Right handle -> do
          let name = "review-flow-" <> renderWorktreeId (worktreeId handle)
                <> "-" <> Text.pack (show (requestIdNumber (requestId worker)))
          allocated <- createWorktree (fromRef (GitRef (renderGitOid (taskSource task))) name)
          case allocated of
            Left issue -> pure (Left (renderWorktreeError issue))
            Right tree -> do
              flow <- R.start (R.withWorktree (worktreeId tree)
                (checkedReviewFlow owner task policy worker checks choose))
              pure (Right (ReviewRun flow tree))

-- The caller retains the dedicated managed worktree. The definition subscribes
-- to the original implementer, including when its response already settled.
reviewFlow
  :: AgentRef -> Task -> ReviewFlowPolicy -> Response (Outcome Candidate)
  -> ActorSpec ReviewFlow ReviewFlowEffects
reviewFlow owner task policy implementer =
  reviewFlowWith owner task policy implementer $ \context ->
    pure (ReviewRouteResult
      (flowReviewChoice policy (routeCandidate context) (routeDecision context))
      DeterministicRoute)

-- The callback can ask Jev but cannot access the coordinator's private state
-- or exercise its fork, control, command, and notification effects. The
-- coordinator alone records the result and changes the flow stage.
reviewFlowWith
  :: AgentRef -> Task -> ReviewFlowPolicy -> Response (Outcome Candidate)
  -> (forall effects. Member Jev effects => ReviewContext -> Eff effects ReviewRouteResult)
  -> ActorSpec ReviewFlow ReviewFlowEffects
reviewFlowWith owner task policy implementer =
  buildReviewFlow (Actor.Selected knownEffects) (\_ _ -> pure (Right ())) owner task policy implementer Nothing

-- The coordinator holds a dedicated managed checkout, advances it only by
-- fast-forward, and routes original counted check evidence before review.
-- Check failures and review findings share flowRepairLimit.
checkedReviewFlow
  :: AgentRef -> Task -> ReviewFlowPolicy -> Response (Outcome Candidate)
  -> [PlanCheck]
  -> (forall effects. Member Jev effects => ReviewContext -> Eff effects ReviewRouteResult)
  -> ActorSpec ReviewFlow CheckedReviewEffects
checkedReviewFlow owner task policy implementer checks =
  buildReviewFlow (Actor.Selected knownEffects) prepareCheckSource owner task policy implementer (Just checks)

buildReviewFlow
  :: (Member Replies effects, Member Actor effects, Member Notifications effects
     , Member Forks effects, Member AgentInspection effects, Member AgentControl effects
     , Member BoundWorktree effects, Member Commands effects, Member Jev effects
     , Subset ResearchLeafEffects (S.State ReviewFlowState ': effects)
     , Member (ActorLocal (R.Message ReviewFlow)) effects)
  => Actor.EffectProfile (R.Message ReviewFlow) effects
  -> (Candidate -> WorktreeEvidence -> Eff effects (Either Text ()))
  -> AgentRef -> Task -> ReviewFlowPolicy -> Response (Outcome Candidate)
  -> Maybe [PlanCheck]
  -> (forall selected. Member Jev selected => ReviewContext -> Eff selected ReviewRouteResult)
  -> ActorSpec ReviewFlow effects
buildReviewFlow profile prepareSource owner task policy implementer checkPlan choose =
  R.definition "review-flow" profile ReviewFlow
    { flowStateField = ReviewFlowState
        { flowStage = AwaitingCandidate, flowRepairCount = 0, flowCorrectionUsed = False
        , flowCandidateReceipts = [], flowReviewerRequests = [], flowReviewerUpdates = []
        , flowReviewerReceipts = [], flowRepairRequests = []
        , flowRepairUpdates = [], flowAttachments = [], flowProgressUpdates = []
        , flowProgress = [], flowNotices = []
        , flowCleanupResult = Nothing, flowReviewRoutes = [], flowCheckPlans = []
        , flowCheckReports = [], flowCheckCleanup = [], flowReviewedProof = Nothing
        , flowCompletionAdmission = Nothing
        }
    , reviewSnapshot = \() -> R.get
    , reviewCleanup = \request -> do
        origin <- R.sender @ReviewFlow
        if origin /= ActorMessageFrom (agentIdentity owner)
          then pure (ReviewCleanupNotOwner origin)
          else do
            state <- R.get
            case flowStage state of
              ReviewAccepted _ -> cleanupReviewers request state
              ReviewIntegrated _ _ -> cleanupReviewers request state
              ReviewStopped _ -> cleanupReviewers request state
              pending -> pure (ReviewCleanupPending pending)
    , firstCandidate = R.on (R.settlement implementer) $ \result -> do
        stage <- R.gets flowStage
        case stage of
          AwaitingCandidate -> do
            own <- R.self @ReviewFlow
            acceptCandidate own implementer result
          _ -> pure ()
    , checksCompleted = \state -> do
        own <- R.self @ReviewFlow
        stage <- R.gets flowStage
        case stage of
          CheckingCandidate candidate started -> finishCandidateChecks own candidate started state
          _ -> pure ()
    , reviewEvents = R.on mempty $ \event -> case event of
        WorkUpdated identity observation -> retainProgress identity observation
        RepairSettled attempt result -> do
          own <- R.self @ReviewFlow
          state <- R.get
          case (flowStage state, reverse (flowRepairRequests state)) of
            (AwaitingRepair _, current : _) | requestId current == requestId attempt ->
              acceptCandidate own attempt result
            _ -> pure ()
        ReviewerSettled reviewer result -> do
          own <- R.self @ReviewFlow
          state <- R.get
          let identity = requestId reviewer
          R.modify' (\current -> current { flowReviewerReceipts =
            flowReviewerReceipts current ++ [(identity, result)] })
          case (flowStage state, reverse (flowReviewerRequests state)) of
            (ReviewingCandidate selected, current : _) | requestId current == identity ->
              checkReviewer own selected reviewer result
            (AwaitingReviewCorrection selected, current : _) | requestId current == identity ->
              checkReviewer own selected reviewer result
            _ -> pure ()
    }
  where
    -- Retain every publication, including closure after a terminal result.
    -- The same pure delta policy is shared with the work collector.
    retainProgress identity observation = do
      R.modify' (\state -> state { flowProgressUpdates =
        flowProgressUpdates state ++ [(identity, observation)] })
      case observation of
        ProgressUpdate cursor progress -> do
          state <- R.get
          let previous = maybe (WorkProgress [] []) id (lookup identity (flowProgress state))
              current = mergeWorkProgress previous progress
              name = obligation task <> " request " <> Text.pack (show (requestIdNumber identity))
              event = workChange name cursor previous current :: WorkEvent ()
          R.modify' (\old -> old
            { flowProgress = filter ((/= identity) . fst) (flowProgress old) ++ [(identity, current)] })
          case workQuestionsMessage event of
            Nothing -> pure ()
            Just message -> notifyOwner message
        ProgressRejected failure -> notifyOwner
          ("review progress unavailable: " <> Text.pack (show failure))
        _ -> pure ()

    notifyOwner message = do
      sent <- sendMessage owner message
      R.modify' (\state -> state { flowNotices = flowNotices state ++ [sent] })

    attachRequest own identity events = do
      attached <- R.attach (reviewEvents own) events
      R.modify' (\state -> state { flowAttachments = flowAttachments state ++ [(identity, attached)] })
      case attached of
        Left issue -> publish (ReviewStopped (SourceAttachmentRefused identity issue))
        Right () -> pure ()

    retainReviewer own correction candidate reviewer updates = do
      let identity = requestId reviewer
      R.modify' (\state -> state
        { flowStage = if correction then AwaitingReviewCorrection candidate else ReviewingCandidate candidate
        , flowCorrectionUsed = correction
        , flowReviewerRequests = flowReviewerRequests state ++ [reviewer]
        , flowReviewerUpdates = flowReviewerUpdates state ++ [updates] })
      attachRequest own identity
        (fmap (WorkUpdated identity) (R.progress updates)
          <> fmap (ReviewerSettled reviewer) (R.settlement reviewer))

    retainRepair own candidate attempt updates = do
      let identity = requestId attempt
      R.modify' (\state -> state
        { flowStage = AwaitingRepair candidate
        , flowRepairCount = flowRepairCount state + 1
        , flowRepairRequests = flowRepairRequests state ++ [attempt]
        , flowRepairUpdates = flowRepairUpdates state ++ [updates] })
      attachRequest own identity
        (fmap (WorkUpdated identity) (R.progress updates)
          <> fmap (RepairSettled attempt) (R.settlement attempt))

    cleanupReviewers request state = do
      let groups = nub [ group
            | reviewer <- flowReviewerRequests state
            , Just group <- [forkGroupHandle reviewer] ]
          prior = case flowCleanupResult state of
            Just (ReviewCleanupAttempted receipts) -> receipts
            _ -> []
      result <- case groups of
        [] -> pure ReviewCleanupNoReviewer
        _ -> ReviewCleanupAttempted <$> mapM (cleanupGroup request prior) groups
      R.modify' (\current -> current { flowCleanupResult = Just result })
      pure result

    cleanupGroup request prior group = case lookup group prior of
      Just receipts | not (shouldRetry request receipts) -> pure (group, receipts)
      earlier -> do
        receipt <- executeCleanup =<< planCleanup group
        pure (group, maybe [] id earlier ++ [receipt])

    shouldRetry ReviewCleanupOnce _ = False
    shouldRetry ReviewCleanupRetryRefused receipts = case reverse receipts of
      latest : _ -> case cleanupReceiptSteps latest of
        [CleanupBlocked _] -> True
        [CleanupStalePlan] -> True
        _ -> False
      [] -> True

    checkReviewer own selected reviewer result = case result of
      Right receipt
        | executionRequest (responseExecution receipt) /= requestId reviewer ->
            publish (ReviewStopped (ReviewerReceiptMismatch
              (requestId reviewer) (executionRequest (responseExecution receipt))))
      _ -> acceptReview own selected reviewer result

    publish stage = do
      previous <- R.gets flowStage
      if terminalStage previous then pure () else do
        R.modify' (\state -> state { flowStage = stage })
        case (flowCompleted policy, stage) of
          (Just route, ReviewStopped reason) -> do
            admission <- R.trySend route (ReviewRefused (requestId implementer) reason)
            R.modify' (\state -> state { flowCompletionAdmission = Just admission })
          (Just route, ReviewAccepted _) -> do
            proof <- R.gets flowReviewedProof
            case proof of
              Just exact -> do
                admission <- R.trySend route (ReviewApproved (requestId implementer) exact)
                R.modify' (\state -> state { flowCompletionAdmission = Just admission })
              Nothing -> R.modify' (\state -> state
                { flowStage = ReviewStopped (ReviewerEvidenceRefused CheckpointNotReady) })
          (Just route, ReviewIntegrated _ result) -> do
            proof <- R.gets flowReviewedProof
            case proof of
              Just exact -> do
                admission <- R.trySend route (ReviewPublished (requestId implementer) exact result)
                R.modify' (\state -> state { flowCompletionAdmission = Just admission })
              Nothing -> R.modify' (\state -> state
                { flowStage = ReviewStopped (ReviewerEvidenceRefused CheckpointNotReady) })
          _ -> pure ()
        let notice = case stage of
              ReviewStopped (ReviewEscalated reason) ->
                Just ("review escalated to owner: " <> reason
                  <> "; inspect flowReviewRoutes and decide repair or scope")
              _ -> flowNotice policy stage
        case notice of
          Nothing -> pure ()
          Just message -> do
            sent <- sendMessage owner message
            R.modify' (\state -> state { flowNotices = flowNotices state ++ [sent] })

    terminalStage (ReviewAccepted _) = True
    terminalStage (ReviewIntegrated _ _) = True
    terminalStage (ReviewStopped _) = True
    terminalStage _ = False

    finishAccepted reviewed = case flowIntegration policy of
      Nothing -> publish (ReviewAccepted reviewed)
      Just target -> do
        receipts <- R.gets flowCandidateReceipts
        let exact = reviewedCandidate reviewed
            sources = [treeId source | Right receipt <- receipts,
              Produced candidate <- [responseValue receipt], candidate == exact,
              WorktreeObserved source _ _ <- [responseWorktree receipt]]
        case reverse sources of
          source : _ -> do
            merged <- R.call (Merge.publish (R.client (Merge.mergeActor target)))
              (Merge.PublishRequest (obligation task) source (candidateCommit exact)
                "integrate reviewed component")
            case merged of
              Merge.Published {} -> publish (ReviewIntegrated reviewed merged)
              other -> publish (ReviewStopped (IntegrationRefused other))
          [] -> publish (ReviewStopped (SourcePreflightUnavailable
            "accepted candidate has no original worktree receipt"))

    acceptCandidate own expected result = do
      R.modify' (\state -> state
        { flowCandidateReceipts = flowCandidateReceipts state ++ [result] })
      case result of
        Left failure -> publish (ReviewStopped (CandidateUnavailable failure))
        Right receipt
          | executionRequest (responseExecution receipt) /= requestId expected ->
              publish (ReviewStopped (CandidateReceiptMismatch
                (requestId expected) (executionRequest (responseExecution receipt))))
        Right receipt -> case responseValue receipt of
          Blocked reason evidence -> publish (ReviewStopped (CandidateBlocked reason evidence))
          Produced candidate -> case candidateAtSubmission candidate (responseWorktree receipt) of
            Left reason -> publish (ReviewStopped (CandidateSourceRefused reason))
            Right exact
              | flowRepairLimit policy < 0 ->
                  publish (ReviewStopped (InvalidRepairLimit (flowRepairLimit policy)))
              | otherwise -> do
                  preflight <- sourcePreflight exact
                  case preflight of
                    Left reason -> publish (ReviewStopped reason)
                    Right () -> case checkPlan of
                      Nothing -> startReviewer own exact
                      Just checks -> startCandidateChecks own exact (responseWorktree receipt) checks

    sourcePreflight exact = case flowSourcePlan policy of
      ComponentReview -> pure (Right ())
      RequiresSiblingCommits required -> do
        bound <- boundWorktree
        case bound of
          Left failure -> pure (Left (SourcePreflightUnavailable (Text.pack (show failure))))
          Right handle -> checkRequired (cwd (handleReceipt handle)) required
      where
        checkRequired _ [] = pure (Right ())
        checkRequired directory (sibling : rest) = do
          result <- Cmd.run (Cmd.inDirectory directory (Cmd.argv
            ["git", "merge-base", "--is-ancestor", renderGitOid sibling
            , renderGitOid (candidateCommit exact)]))
          let completion = Cmd.commandResult result
          case (Cmd.commandOutcome completion, Cmd.commandCleanup completion) of
            (Cmd.CommandExited 0, Cmd.CommandClean) -> checkRequired directory rest
            (Cmd.CommandExited 1, Cmd.CommandClean) ->
              pure (Left (RequiredSiblingMissing sibling (candidateCommit exact)))
            other -> pure (Left (SourcePreflightUnavailable (Text.pack (show other))))

    startCandidateChecks own exact source checks = do
      -- Refuse malformed plan source before any check command is submitted.
      if null checks || any ((/= renderGitOid (candidateCommit exact)) . focusedSource
          . (\item -> planSpec item (candidateCommit exact))) checks
        then publish (ReviewStopped (CandidateChecksRefused "checks must name this exact candidate"))
        else do
          prepared <- raise (prepareSource exact source)
          case prepared of
            Left reason -> publish (ReviewStopped (CandidateChecksRefused reason))
            Right () -> do
              started <- startCheckPlanInto (checksCompleted own) (candidateCommit exact) checks
              case started of
                Left issue -> publish (ReviewStopped (CandidateChecksRefused (Text.pack (show issue))))
                Right plan -> do
                  R.modify' (\state -> state { flowCheckPlans = flowCheckPlans state ++ [(exact, plan)] })
                  case planWatcher plan of
                    Just (Right _) -> publish (CheckingCandidate exact plan)
                    _ -> publish (ReviewStopped (CandidateChecksRefused "no check observer was admitted; inspect flowCheckPlans"))

    finishCandidateChecks own candidate started state = do
      let report = PlanReport started (Just state)
          entries = checkEntries state
          allStarted = all (either (const False) (const True) . snd) (planStarts started)
          exactJobs = length entries == length (planStarts started) && and
            [ any (\entry -> checkName entry == name && runJob (checkRun entry) == runJob run
                  && runSpec (checkRun entry) == runSpec run) entries
            | (name, Right run) <- planStarts started ]
          provenEntry entry = maybe False (checkEvidenceComplete entry) (checkOutcome entry)
      R.modify' (\current -> current { flowCheckReports = flowCheckReports current ++ [(candidate, report)] })
      case planWatcher started of
        Just (Right watcher) -> do
          retired <- finishChecks watcher
          R.modify' (\current -> current { flowCheckCleanup = flowCheckCleanup current ++ [retired] })
        _ -> pure ()
      if not allStarted || not exactJobs || null entries
        then publish (ReviewStopped (CandidateChecksUnknown (planSummary report)))
        else if not (all provenEntry entries)
          then publish (ReviewStopped (CandidateChecksUnknown (planSummary report)))
        else if planPassed report
          then startReviewer own candidate
          else do
            count <- R.gets flowRepairCount
            let findings = [checkLine entry | entry <- entries,
                  Just outcome <- [checkOutcome entry], checkVerdict entry outcome /= CheckPassed]
                context = ReviewContext task candidate (Repair candidate findings) count
                  (flowRepairLimit policy) (flowEscalationCriteria policy)
            route <- choose context
            R.modify' (\current -> current
              { flowReviewRoutes = flowReviewRoutes current ++ [(context, route)] })
            case routeChoice route of
              EscalateReview reason -> publish (ReviewStopped (ReviewEscalated reason))
              HonorReview -> requestRepair own candidate findings

    startReviewer own exact = do
      reports <- R.gets flowCheckReports
      let evidence = Text.intercalate "\n" [planSummary report | (candidate, report) <- reports, candidate == exact]
          request = ReviewRequest (AssignedTask task) exact OwnerRepairs
      launched <- attemptUnfold (taskGroup task) $
        childWithProgress @WorkProgress @(Outcome ReviewDecision) (flowReviewer policy (flowSourcePlan policy) request evidence)
      case launched of
        Left refusal -> publish (ReviewStopped (ReviewerAdmissionRefused (renderUnfoldError refusal)))
        Right (reviewer, updates) -> retainReviewer own False exact reviewer updates

    acceptReview own selected reviewer result = case result of
      Left failure -> publish (ReviewStopped (ReviewerUnavailable failure))
      Right receipt -> case (case responseValue receipt of
          Produced (Accepted _) -> reviewCandidateAtSubmission
          _ -> candidateAtSubmission) selected (responseWorktree receipt) of
        Left reason -> publish (ReviewStopped (ReviewerSourceRefused reason))
        Right _ -> case responseValue receipt of
          Blocked reason evidence -> publish (ReviewStopped (ReviewerBlocked reason evidence))
          Produced decision -> case sourceCheck selected decision of
            Left reason -> publish (ReviewStopped reason)
            Right () -> do
              count <- R.gets flowRepairCount
              let context = ReviewContext task selected decision count
                    (flowRepairLimit policy) (flowEscalationCriteria policy)
              route <- choose context
              R.modify' (\state -> state
                { flowReviewRoutes = flowReviewRoutes state ++ [(context, route)] })
              case routeChoice route of
                EscalateReview reason -> publish (ReviewStopped (ReviewEscalated reason))
                HonorReview -> case decision of
                  Accepted reviewed -> do
                    let request = ReviewRequest (AssignedTask task) selected OwnerRepairs
                    proof <- admitReviewedCheckpoint request reviewer
                    case proof of
                      Left issue -> publish (ReviewStopped (ReviewerEvidenceRefused issue))
                      Right exact -> do
                        R.modify' (\state -> state { flowReviewedProof = Just exact })
                        finishAccepted reviewed
                  Repair candidate findings
                    | null findings -> correctReviewer own candidate
                    | otherwise -> requestRepair own candidate findings

    correctReviewer own candidate = do
      used <- R.gets flowCorrectionUsed
      if used
        then publish (ReviewStopped (EmptyRepairFindings candidate))
        else do
          requests <- R.gets flowReviewerRequests
          case reverse requests of
            [] -> publish (ReviewStopped ReviewerWithoutRequest)
            reviewer : _ -> do
              let request = ReviewRequest (AssignedTask task) candidate OwnerRepairs
              _ <- requestWithProgressInto @WorkProgress @(Outcome ReviewDecision)
                (responseActor reviewer)
                ((assignment [label|review-correction|] request)
                  { guidance = Just (flowCorrectionInstructions policy), report = Silent })
                (\(attempt, updates) -> retainReviewer own True candidate attempt updates)
              pure ()

    sourceCheck selected decision = case decision of
      Accepted reviewed
        | reviewedBasis reviewed /= AssignedTask task -> Left ReviewerScopeMismatch
        | candidateCommit (reviewedCandidate reviewed) /= candidateCommit selected ->
            Left (ReviewerSourceMismatch (candidateCommit selected)
              (candidateCommit (reviewedCandidate reviewed)))
        | reviewedCandidate reviewed /= selected ->
            Left (ReviewerCandidateMismatch selected (reviewedCandidate reviewed))
        | otherwise -> Right ()
      Repair candidate _
        | candidateCommit candidate /= candidateCommit selected ->
            Left (ReviewerSourceMismatch (candidateCommit selected) (candidateCommit candidate))
        | candidate /= selected -> Left (ReviewerCandidateMismatch selected candidate)
        | otherwise -> Right ()

    requestRepair own candidate findings = do
      count <- R.gets flowRepairCount
      if count >= flowRepairLimit policy
        then publish (ReviewStopped (RepairBudgetSpent count candidate findings))
        else do
          _ <- requestWithProgressInto @WorkProgress @(Outcome Candidate)
            (responseActor implementer)
            ((assignment [label|repair|] (RepairTask task candidate findings))
              { guidance = Just (flowRepairInstructions policy), report = Silent })
            (\(attempt, updates) -> retainRepair own candidate attempt updates)
          pure ()

-- Never reset or publish a branch. tryMerge owns checkout mutation; an
-- ancestry check ensures this is a fast-forward, and HEAD is checked again.
prepareCheckSource
  :: (Member BoundWorktree effects, Member WorktreeIntegration effects, Member Commands effects)
  => Candidate -> WorktreeEvidence -> Eff effects (Either Text ())
prepareCheckSource exact source = do
  bound <- boundWorktree
  case (source, bound) of
    (NoBoundWorktree, _) -> pure (Left "candidate has no bound-source evidence")
    (WorktreeObservationFailed failure, _) -> pure (Left (renderWorktreeError failure))
    (_, Left failure) -> pure (Left (Text.pack (show failure)))
    (WorktreeObserved sourceReceipt _ _, Right handle) -> do
      let directory = cwd (handleReceipt handle)
          git args = Cmd.run (Cmd.inDirectory directory (Cmd.argv ("git" : args)))
      before <- git ["rev-parse", "HEAD"]
      clean <- git ["status", "--porcelain"]
      case (Cmd.stdout before, Cmd.commandCleanup (Cmd.commandResult before),
            Cmd.stdout clean, Cmd.commandCleanup (Cmd.commandResult clean)) of
        (Right headText, Cmd.CommandClean, Right dirty, Cmd.CommandClean)
          | Text.null (Text.strip dirty) -> do
          ancestor <- git ["merge-base", "--is-ancestor", Text.strip headText, renderGitOid (candidateCommit exact)]
          case (Cmd.commandOutcome (Cmd.commandResult ancestor), Cmd.commandCleanup (Cmd.commandResult ancestor)) of
            (Cmd.CommandExited 0, Cmd.CommandClean) -> do
              merged <- tryMerge MergeRequest
                { mergeSourceHead = candidateCommit exact
                , mergeSourceWorktree = treeId sourceReceipt
                , mergeSourceBranch = Nothing
                , mergeTargetWorktree = worktreeId handle
                , mergeMessage = "prepare exact candidate checks", mergeAdvance = Nothing }
              case merged of
                Left issue -> pure (Left (Text.pack (show issue)))
                Right (FastForwarded _ _ _) -> verifyHead git
                Right (AlreadyContained _ _) -> verifyHead git
                Right other -> pure (Left ("check checkout did not fast-forward: " <> Text.pack (show other)))
            (Cmd.CommandExited 1, Cmd.CommandClean) ->
              pure (Left "check checkout cannot fast-forward to candidate")
            other -> pure (Left ("source preflight command failed: " <> Text.pack (show other)))
        _ -> pure (Left "check checkout is dirty or source evidence is unavailable")
  where
    verifyHead git = do
      after <- git ["rev-parse", "HEAD"]
      pure $ case (Cmd.stdout after, Cmd.commandCleanup (Cmd.commandResult after)) of
        (Right actual, Cmd.CommandClean)
          | Text.strip actual == renderGitOid (candidateCommit exact) -> Right ()
        _ -> Left "check checkout did not reach exact candidate; source retained"
