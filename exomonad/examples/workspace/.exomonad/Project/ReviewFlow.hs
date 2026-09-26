{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- One Task's committed candidate, fresh review, and bounded same-child repair.
-- The owner still integrates accepted source and retires the worker group.
module Project.ReviewFlow
  ( ReviewFlow (reviewSnapshot, firstCandidate)
  , ReviewFlowState (..)
  , ReviewStage (..)
  , ReviewStop (..)
  , ReviewChoice (..)
  , ReviewSourcePlan (..)
  , ReviewFlowPolicy (..)
  , defaultReviewFlowPolicy
  , reviewFlow
  ) where

import GHC.Generics (Generic)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (GitRef (..), WorktreeHandle (..))
import Tidepool.Actors.Worktree (boundWorktree)
import Tidepool.Worktree (WorktreeReceipt (..), renderGitOid)
import Project.Types
import Project.Work (candidateAtSubmission, projectPrompt, reviewContext)

data ReviewChoice = HonorReview | EscalateReview Text
  deriving (Show, Eq)

-- ComponentReview deliberately reviews only the submitted component. A
-- product review declares the sibling commits that must already be in it.
data ReviewSourcePlan = ComponentReview | RequiresSiblingCommits [GitOid]
  deriving (Show, Eq)

-- Disposition may stop on a project-specific finding, but cannot upgrade a
-- repair request into acceptance. The exact candidate check runs first.
data ReviewFlowPolicy = ReviewFlowPolicy
  { flowRepairLimit :: Int
  , flowSourcePlan :: ReviewSourcePlan
  , flowReviewChoice :: Candidate -> ReviewDecision -> ReviewChoice
  , flowNotice :: ReviewStage -> Maybe Text
  }

defaultReviewFlowPolicy :: ReviewFlowPolicy
defaultReviewFlowPolicy = ReviewFlowPolicy
  { flowRepairLimit = 2
  , flowSourcePlan = ComponentReview
  , flowReviewChoice = \_ _ -> HonorReview
  , flowNotice = \stage -> case stage of
      ReviewAccepted reviewed -> Just
        ("review accepted " <> renderGitOid (candidateCommit (reviewedCandidate reviewed))
          <> "; owner integration remains")
      ReviewStopped reason -> Just ("review stopped: " <> Text.pack (show reason))
      _ -> Nothing
  }

data ReviewStop
  = CandidateUnavailable ResponseFailure
  | CandidateBlocked Text [Text]
  | CandidateSourceRefused Text
  | CandidateReceiptMismatch RequestId RequestId
  | RequiredSiblingMissing GitOid GitOid
  | SourcePreflightUnavailable Text
  | InvalidRepairLimit Int
  | RepairWithoutRequest
  | ReviewerUnavailable ResponseFailure
  | ReviewerAdmissionRefused Text
  | ReviewerBlocked Text [Text]
  | ReviewerSourceRefused Text
  | ReviewerReceiptMismatch RequestId RequestId
  | ReviewerWithoutRequest
  | ReviewerOutOfOrder
  | ReviewerSourceMismatch GitOid GitOid
  | ReviewerCandidateMismatch Candidate Candidate
  | ReviewerScopeMismatch
  | EmptyRepairFindings Candidate
  | ReviewEscalated Text
  | RepairBudgetSpent Int Candidate [Text]
  deriving (Show)

data ReviewStage
  = AwaitingCandidate
  | ReviewingCandidate Candidate
  | AwaitingReviewCorrection Candidate
  | AwaitingRepair Candidate
  | ReviewAccepted ReviewedCandidate
  | ReviewStopped ReviewStop
  deriving (Show)

data ReviewFlowState = ReviewFlowState
  { flowStage :: ReviewStage
  , flowRepairCount :: Int
  , flowCorrectionUsed :: Bool
  , flowCandidateReceipts :: [Either ResponseFailure (ResponseResult (Outcome Candidate))]
  , flowReviewerRequests :: [Response (Outcome ReviewDecision)]
  , flowReviewerUpdates :: [Progress WorkProgress]
  , flowReviewerRoutes :: [Forwarding (Outcome ReviewDecision)]
  , flowRepairRequests :: [Response (Outcome Candidate)]
  , flowRepairUpdates :: [Progress WorkProgress]
  , flowRepairRoutes :: [Forwarding (Outcome Candidate)]
  , flowNotices :: [Either NotificationError NotificationReceipt]
  }

instance Show ReviewFlowState where
  show state = "ReviewFlowState " ++ show (flowStage state)
    ++ " repairs=" ++ show (flowRepairCount state)
    ++ " candidates=" ++ show (length (flowCandidateReceipts state))
    ++ " reviewers=" ++ show (length (flowReviewerRequests state))
    ++ " repairRequests=" ++ show (length (flowRepairRequests state))
    ++ " notices=" ++ show (length (flowNotices state))

data ReviewFlow mode = ReviewFlow
  { flowStateField :: mode :- State ReviewFlowState
  , reviewSnapshot :: mode :- Call () (R.Reply ReviewFlowState)
  , firstCandidate :: mode :- Call (Either ResponseFailure (ResponseResult (Outcome Candidate))) NoReply
  , reviewerStarted :: mode :- Call (Candidate, Response (Outcome ReviewDecision), Progress WorkProgress) NoReply
  , reviewerCorrectionStarted :: mode :- Call (Candidate, Response (Outcome ReviewDecision), Progress WorkProgress) NoReply
  , routeReviewer :: mode :- Call (Response (Outcome ReviewDecision)) NoReply
  , repairStarted :: mode :- Call (Candidate, Response (Outcome Candidate), Progress WorkProgress) NoReply
  , routeRepair :: mode :- Call (Response (Outcome Candidate)) NoReply
  , repairedCandidate :: mode :- Call (Either ResponseFailure (ResponseResult (Outcome Candidate))) NoReply
  , reviewerResult :: mode :- Call (Either ResponseFailure (ResponseResult (Outcome ReviewDecision))) NoReply
  } deriving Generic

type ReviewFlowEffects = R.LocalEffects ReviewFlow ResearchEffects

-- The caller supplies an unbound managed worktree through R.withWorktree and
-- retains its handle. Exact-ref reviewer forks need that bound source authority.
-- After R.start, retain R.forwardResult implementer (firstCandidate (R.client flow)).
reviewFlow
  :: AgentRef -> Task -> ReviewFlowPolicy -> Response (Outcome Candidate)
  -> ActorSpec ReviewFlow ReviewFlowEffects
reviewFlow owner task policy implementer =
  R.definition "review-flow" (Actor.Selected knownEffects) ReviewFlow
    { flowStateField = ReviewFlowState AwaitingCandidate 0 False [] [] [] [] [] [] [] []
    , reviewSnapshot = \() -> R.get
    , firstCandidate = \result -> do
        own <- R.self @ReviewFlow
        acceptCandidate own implementer result
    , reviewerStarted = \(candidate, reviewer, updates) -> do
        own <- R.self @ReviewFlow
        R.modify' (\state -> state
          { flowStage = ReviewingCandidate candidate
          , flowCorrectionUsed = False
          , flowReviewerRequests = flowReviewerRequests state ++ [reviewer]
          , flowReviewerUpdates = flowReviewerUpdates state ++ [updates] })
        R.send (routeReviewer own) reviewer
    , reviewerCorrectionStarted = \(candidate, reviewer, updates) -> do
        own <- R.self @ReviewFlow
        R.modify' (\state -> state
          { flowStage = AwaitingReviewCorrection candidate
          , flowCorrectionUsed = True
          , flowReviewerRequests = flowReviewerRequests state ++ [reviewer]
          , flowReviewerUpdates = flowReviewerUpdates state ++ [updates] })
        R.send (routeReviewer own) reviewer
    , routeReviewer = \reviewer -> do
        own <- R.self @ReviewFlow
        route <- R.forwardResult reviewer (reviewerResult own)
        R.modify' (\state -> state { flowReviewerRoutes = flowReviewerRoutes state ++ [route] })
    , repairStarted = \(candidate, attempt, updates) -> do
        own <- R.self @ReviewFlow
        R.modify' (\state -> state
          { flowStage = AwaitingRepair candidate
          , flowRepairCount = flowRepairCount state + 1
          , flowRepairRequests = flowRepairRequests state ++ [attempt]
          , flowRepairUpdates = flowRepairUpdates state ++ [updates] })
        R.send (routeRepair own) attempt
    , routeRepair = \attempt -> do
        own <- R.self @ReviewFlow
        route <- R.forwardResult attempt (repairedCandidate own)
        R.modify' (\state -> state { flowRepairRoutes = flowRepairRoutes state ++ [route] })
    , repairedCandidate = \result -> do
        own <- R.self @ReviewFlow
        attempts <- R.gets flowRepairRequests
        case reverse attempts of
          attempt : _ -> acceptCandidate own attempt result
          [] -> publish (ReviewStopped RepairWithoutRequest)
    , reviewerResult = \result -> do
        own <- R.self @ReviewFlow
        state <- R.get
        case (flowStage state, reverse (flowReviewerRequests state)) of
          (ReviewingCandidate selected, reviewer : _) -> checkReviewer own selected reviewer result
          (AwaitingReviewCorrection selected, reviewer : _) -> checkReviewer own selected reviewer result
          (_, []) -> publish (ReviewStopped ReviewerWithoutRequest)
          _ -> publish (ReviewStopped ReviewerOutOfOrder)
    }
  where
    checkReviewer own selected reviewer result = case result of
      Right receipt
        | executionRequest (responseExecution receipt) /= requestId reviewer ->
            publish (ReviewStopped (ReviewerReceiptMismatch
              (requestId reviewer) (executionRequest (responseExecution receipt))))
      _ -> acceptReview own selected result

    publish stage = do
      R.modify' (\state -> state { flowStage = stage })
      case flowNotice policy stage of
        Nothing -> pure ()
        Just message -> do
          sent <- sendMessage owner message
          R.modify' (\state -> state { flowNotices = flowNotices state ++ [sent] })

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
                    Right () -> startReviewer own exact

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

    startReviewer own exact = do
      let request = ReviewRequest (AssignedTask task) exact
            OwnerRepairs
      launched <- attemptUnfold (taskGroup task) $
        childWithProgress @WorkProgress @(Outcome ReviewDecision) $
        withInstructions (projectPrompt "review") $
        withContext (selected (\input -> reviewContext input <> sourceScope)) $
        withModel "luna" $ withEffort Medium $
        narrowed @ResearchLeafEffects knownEffects
          (inspectionPolicy (atRef (GitRef (renderGitOid (candidateCommit exact)))))
          ((assignment [label|review|] request) { report = Silent })
      case launched of
        Left refusal -> publish (ReviewStopped (ReviewerAdmissionRefused (renderUnfoldError refusal)))
        Right (reviewer, updates) -> R.send (reviewerStarted own) (exact, reviewer, updates)

    sourceScope = case flowSourcePlan policy of
      ComponentReview ->
        "\nReview scope: this is a component review. Sibling integration and product acceptance are outside this review."
      RequiresSiblingCommits required ->
        "\nReview scope: required sibling commits were verified in this exact candidate: "
          <> Text.intercalate ", " (map renderGitOid required)

    acceptReview own selected result = case result of
      Left failure -> publish (ReviewStopped (ReviewerUnavailable failure))
      Right receipt -> case candidateAtSubmission selected (responseWorktree receipt) of
        Left reason -> publish (ReviewStopped (ReviewerSourceRefused reason))
        Right _ -> case responseValue receipt of
          Blocked reason evidence -> publish (ReviewStopped (ReviewerBlocked reason evidence))
          Produced decision -> case sourceCheck selected decision of
            Left reason -> publish (ReviewStopped reason)
            Right () -> case flowReviewChoice policy selected decision of
              EscalateReview reason -> publish (ReviewStopped (ReviewEscalated reason))
              HonorReview -> case decision of
                Accepted reviewed -> publish (ReviewAccepted reviewed)
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
                  { guidance = Just (projectPrompt "review" <>
                      "\nYour prior Repair response had no findings. Return Accepted only after verifying this exact source, or Repair with concrete findings. This correction is final."), report = Silent })
                (\(attempt, updates) -> R.send (reviewerCorrectionStarted own) (candidate, attempt, updates))
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
              { guidance = Just (projectPrompt "repair"), report = Silent })
            (\(attempt, updates) -> R.send (repairStarted own) (candidate, attempt, updates))
          pure ()
