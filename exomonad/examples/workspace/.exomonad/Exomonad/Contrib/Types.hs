{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}
module Exomonad.Contrib.Types
  ( Task (..), AcceptedDecision (..)
  , Candidate (..), RepairOwner (..), ReviewBasis (..), reviewBase, reviewOwnedPaths, reviewAcceptance
  , ReviewRequest (..), ReviewedCandidate (..), ReviewDecision (..), RepairTask (..)
  , Outcome (..), ReportedDelivery (..), Delivery, DesignQuestion (..), DesignAnswer (..)
  , PlanAmendment (..), IncorporationTask (..), Incorporation (..), DesignSlot (..)
  , ReviewedCheckpoint, checkpointBasis, checkpointCandidate, checkpointReceipt
  , ReviewEvidenceIssue (..), admitReviewedCheckpoint, candidateAtSubmission
  , reviewCandidateAtSubmission, cleanReviewCheckout
  , WorkProgress, pattern WorkProgress, workEvidence, workQuestions, workReviewed, withReviewedCheckpoint, mergeWorkProgress
  , Question (..), Attention, sameQuestion
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.List (nub, sort)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Agent.Reply
  ( Replies, RequestId, Response, ResponseResult (..), ResponseState (..)
  , ResponseFailure, WorktreeEvidence (..), ExecutionReceipt (..), pollResponse, requestId )
import Tidepool.Actors.Exomonad
  ( AgentRef, Label, ForkGroupPath, ForkEffort, GitOid, Model, WatchLabel
  )
import Tidepool.Inspection (Display (..), application, displayRecord)
import Tidepool.Worktree
  ( DirtySummary (..), HeadState (..), SubmissionObservation (..)
  , WorkingState (..), renderGitOid, renderWorktreeError )

-- A task is the understanding handed to a fresh context, not a workflow stage.
data Task = Task
  { taskGroup :: ForkGroupPath
  , planPath :: Text
  , taskSource :: GitOid
  , obligation :: Text
  , rationale :: Text
  , ownedPaths :: [Text]
  , acceptance :: Text
  , acceptedDecisions :: [AcceptedDecision]
  } deriving (Show, Eq)

-- Display is what the workbench and settlement notices show; Show stays the
-- constructor dump for debugging.
instance Display Task where
  displayTree = displayTreePrec 0
  displayTreePrec p t = displayRecord p "Task"
    [ ("taskGroup", displayTree (taskGroup t))
    , ("planPath", displayTree (planPath t))
    , ("taskSource", displayTree (taskSource t))
    , ("obligation", displayTree (obligation t))
    , ("rationale", displayTree (rationale t))
    , ("ownedPaths", displayTree (ownedPaths t))
    , ("acceptance", displayTree (acceptance t))
    , ("acceptedDecisions", displayTree (acceptedDecisions t))
    ]

-- The owner records its supported choice at the incorporated source revision.
-- This is evidence-bearing task data; the record grants no runtime authority.
data AcceptedDecision = AcceptedDecision
  { decisionQuestion :: Question
  , decisionSource :: GitOid
  , decisionSummary :: Text
  , decisionEvidence :: [Text]
  } deriving (Show, Eq)

-- Authored check summaries are claims. Executed checks retain their original
-- command handles, counted evidence and terminal receipts separately.
data Candidate = Candidate
  { candidateCommit :: GitOid
  , reportedChecks :: [Text]
  , remainingGates :: [Text]
  } deriving (Show, Eq)

instance Display Candidate where
  displayTree = displayTreePrec 0
  displayTreePrec p c = displayRecord p "Candidate"
    [ ("candidateCommit", displayTree (candidateCommit c))
    , ("reportedChecks", displayTree (reportedChecks c))
    , ("remainingGates", displayTree (remainingGates c))
    ]

-- Queuing a repair to the owner of a pending delivery would deadlock it.
-- A separate implementer is available for repair after returning its candidate.
data RepairOwner = OwnerRepairs | RetainedImplementer AgentRef
  deriving (Show)

-- Preserve the contract that was actually submitted for review. An exact
-- commit review has no owning Task or plan to carry forward.
data ReviewBasis
  = AssignedTask Task
  | ExactScope GitOid [Text] Text -- cumulative base, owned paths, acceptance
  deriving (Show, Eq)

reviewBase :: ReviewBasis -> GitOid
reviewBase (AssignedTask task) = taskSource task
reviewBase (ExactScope base _ _) = base

reviewOwnedPaths :: ReviewBasis -> [Text]
reviewOwnedPaths (AssignedTask task) = ownedPaths task
reviewOwnedPaths (ExactScope _ paths _) = paths

reviewAcceptance :: ReviewBasis -> Text
reviewAcceptance (AssignedTask task) = acceptance task
reviewAcceptance (ExactScope _ _ accept) = accept

data ReviewRequest = ReviewRequest
  { reviewBasis :: ReviewBasis
  , reviewInput :: Candidate
  , repairOwner :: RepairOwner
  } deriving (Show)

-- A reviewer reports inspection notes; only ReviewedCheckpoint proves its
-- original typed response and exact source, independently of executed checks.
data ReviewedCandidate = ReviewedCandidate
  { reviewedBasis :: ReviewBasis
  , reviewedCandidate :: Candidate
  , reviewNotes :: [Text]
  , reviewRationale :: Text
  } deriving (Show, Eq)

data ReviewDecision
  = Accepted ReviewedCandidate
  | Repair Candidate [Text]
  deriving (Show, Eq)

-- The retained response is the original review evidence. Construction is
-- restricted to 'admitReviewedCheckpoint', which observes its Response handle.
data ReviewedCheckpoint = ReviewedCheckpoint
  { checkpointBasis :: ReviewBasis
  , checkpointCandidate :: Candidate
  , checkpointReceipt :: ResponseResult (Outcome ReviewDecision)
  }

instance Eq ReviewedCheckpoint where
  left == right = (checkpointBasis left, checkpointCandidate left)
    == (checkpointBasis right, checkpointCandidate right)

instance Show ReviewedCheckpoint where
  show checkpoint = "ReviewedCheckpoint " ++ show (checkpointBasis checkpoint)
    ++ " " ++ show (candidateCommit (checkpointCandidate checkpoint))

instance Display ReviewedCheckpoint where
  displayTree = displayTreePrec 0
  displayTreePrec p checkpoint = displayRecord p "ReviewedCheckpoint"
    [ ("basis", displayTree (Text.pack (show (checkpointBasis checkpoint))))
    , ("candidate", displayTree (checkpointCandidate checkpoint))
    , ("reviewRequest", displayTree (Text.pack (show
        (executionRequest (responseExecution (checkpointReceipt checkpoint))))))
    ]

data ReviewEvidenceIssue
  = CheckpointNotReady
  | CheckpointUnavailable ResponseFailure
  | CheckpointRequestMismatch RequestId RequestId
  | CheckpointSourceRejected Text
  | CheckpointBlocked Text [Text]
  | CheckpointNeedsRepair Candidate [Text]
  | CheckpointBasisMismatch ReviewBasis ReviewBasis
  | CheckpointCandidateMismatch Candidate Candidate
  deriving (Show, Eq)

-- Read the original typed response; supplied prose or a copied verdict cannot
-- create a checkpoint. Review launch owns actor independence; this validates
-- the observed source and verdict, not reviewer independence or integration.
admitReviewedCheckpoint
  :: Member Replies effects
  => ReviewRequest -> Response (Outcome ReviewDecision)
  -> Eff effects (Either ReviewEvidenceIssue ReviewedCheckpoint)
admitReviewedCheckpoint request response = do
  observed <- pollResponse response
  pure $ case observed of
    ResponseReady receipt -> validate receipt
    ResponseUnavailable failure -> Left (CheckpointUnavailable failure)
    _ -> Left CheckpointNotReady
  where
    requested = reviewInput request
    validate receipt
      | actualRequest /= expectedRequest = Left (CheckpointRequestMismatch expectedRequest actualRequest)
      | Left reason <- reviewCandidateAtSubmission requested (responseWorktree receipt) =
          Left (CheckpointSourceRejected reason)
      | otherwise = case responseValue receipt of
          Blocked reason evidence -> Left (CheckpointBlocked reason evidence)
          Produced (Repair candidate findings) -> Left (CheckpointNeedsRepair candidate findings)
          Produced (Accepted reviewed)
            | reviewedBasis reviewed /= reviewBasis request ->
                Left (CheckpointBasisMismatch (reviewBasis request) (reviewedBasis reviewed))
            | reviewedCandidate reviewed /= requested ->
                Left (CheckpointCandidateMismatch requested (reviewedCandidate reviewed))
            | otherwise -> Right (ReviewedCheckpoint (reviewBasis request) requested receipt)
      where
        expectedRequest = requestId response
        actualRequest = executionRequest (responseExecution receipt)

candidateAtSubmission :: Candidate -> WorktreeEvidence -> Either Text Candidate
candidateAtSubmission candidate evidence = case evidence of
  WorktreeObserved _ _ observation
    | actual == candidateCommit candidate -> Right candidate
    | otherwise -> Left ("candidate " <> renderGitOid (candidateCommit candidate) <> "; submitted " <> renderGitOid actual)
    where actual = headOid (submittedHead observation)
  NoBoundWorktree -> Left "candidate has no bound-source evidence"
  WorktreeObservationFailed failure -> Left (renderWorktreeError failure)

-- An accepted review refers to the committed checkout that was observed at
-- reply time. Uncommitted edits or an in-progress operation cannot be included
-- in that candidate. Ordinary implementation progress uses the HEAD-only gate.
reviewCandidateAtSubmission :: Candidate -> WorktreeEvidence -> Either Text Candidate
reviewCandidateAtSubmission candidate evidence = do
  matched <- candidateAtSubmission candidate evidence
  case evidence of
    WorktreeObserved _ _ observation
      | cleanReviewCheckout (workingState observation) -> Right matched
      | otherwise -> Left "review checkout was dirty or in progress at submission"
    _ -> Left "review checkout observation unavailable"

cleanReviewCheckout :: WorkingState -> Bool
cleanReviewCheckout state = case changes state of
  DirtySummary [] [] [] _ -> case operation state of
    Nothing -> True
    Just _ -> False
  _ -> False

data RepairTask = RepairTask
  { repairAssignment :: Task
  , repairInput :: Candidate
  , repairFindings :: [Text]
  } deriving (Show, Eq)

-- Reviewed source and the resulting integration head are different facts.
-- The remaining product gates stay attached to the exact reviewed candidate.
data Outcome value = Produced value | Blocked Text [Text]
  deriving (Show, Eq)

instance Display value => Display (Outcome value) where
  displayTree = displayTreePrec 0
  displayTreePrec p (Produced value) = application p "Produced" [displayTreePrec 11 value]
  displayTreePrec p (Blocked reason evidence) =
    application p "Blocked" [displayTreePrec 11 reason, displayTreePrec 11 evidence]

-- An authored integration report, not observed incorporation or a check proof.
-- The observed integration head and command receipt live in MergeResult.
data ReportedDelivery = Delivered ReviewedCandidate GitOid [Text]
  deriving (Show, Eq)

type Delivery = Outcome ReportedDelivery

data DesignQuestion = DesignQuestion
  { questionPlan :: Text
  , questionSource :: GitOid
  , questionFinding :: Text
  , questionEvidence :: [Text]
  , questionAlternatives :: [Text]
  , questionUnblocks :: [Text]
  } deriving (Show, Eq)

instance Ord DesignQuestion where
  compare left right = compare
    (questionPlan left, renderGitOid (questionSource left), questionFinding left,
      questionEvidence left, questionAlternatives left, questionUnblocks left)
    (questionPlan right, renderGitOid (questionSource right), questionFinding right,
      questionEvidence right, questionAlternatives right, questionUnblocks right)

data DesignAnswer
  = Decision Text [Text]
  | AmendPlan PlanAmendment
  | NeedEvidence [Text]
  deriving (Show, Eq)

data PlanAmendment = PlanAmendment
  { amendmentBase :: GitOid
  , amendmentCommit :: GitOid
  , amendmentPaths :: [Text]
  , amendmentReason :: Text
  , amendmentObligations :: [Text]
  , amendmentEvidence :: [Text]
  } deriving (Show, Eq)

data IncorporationTask = IncorporationTask
  { incorporationAssignment :: Task
  , incorporationAmendment :: PlanAmendment
  } deriving (Show, Eq)

data Incorporation
  = Incorporated PlanAmendment GitOid [Text]
  | IncorporationBlocked PlanAmendment Text [Text]
  deriving (Show, Eq)

data DesignSlot = DesignSlot
  { specialistPlan :: Text
  , specialistGroup :: ForkGroupPath
  , specialistLabel :: Label
  , specialistWatch :: WatchLabel
  , specialistModel :: Model
  , specialistEffort :: ForkEffort
  }

-- Evidence and questions are independently useful progress payloads. They are
-- authored data, never authority to retry, stop or release a resource.
data WorkProgress = WorkProgressData
  { workEvidence :: [Candidate]
  , workQuestions :: Attention
  , workReviewed :: [ReviewedCheckpoint]
  }
  deriving (Show, Eq)

-- Two-argument construction stays concise; real record fields preserve the
-- reviewed value when callers update evidence or questions.
pattern WorkProgress :: [Candidate] -> Attention -> WorkProgress
pattern WorkProgress evidence questions <- WorkProgressData evidence questions _
  where WorkProgress evidence questions = WorkProgressData evidence questions []
{-# COMPLETE WorkProgress #-}

withReviewedCheckpoint :: ReviewedCheckpoint -> WorkProgress -> WorkProgress
withReviewedCheckpoint checkpoint progress = WorkProgressData
  (workEvidence progress) (workQuestions progress) appended
  where
    before = workReviewed progress
    appended = if checkpoint `elem` before then before else before ++ [checkpoint]

-- Each publication replaces attention; candidate and review evidence accumulate.
mergeWorkProgress :: WorkProgress -> WorkProgress -> WorkProgress
mergeWorkProgress previous current = WorkProgressData
  (nub (workEvidence previous ++ workEvidence current))
  (nub (sort (workQuestions current)))
  (nub (workReviewed previous ++ workReviewed current))

instance Display WorkProgress where
  displayTree = displayTreePrec 0
  displayTreePrec p w = displayRecord p "WorkProgress"
    [ ("workEvidence", displayTree (workEvidence w))
    , ("workQuestions", displayTree (workQuestions w))
    , ("workReviewed", displayTree (workReviewed w))
    ]

-- Only unresolved decisions/blockers needing the recipient's action. Successful
-- incorporation and unchanged standing gates belong to evidence, not questions.
-- Retain questions until a supported resolution, including across source closure.
data Question = Question
  { questionKey :: Text
  , questionDetails :: DesignQuestion
  } deriving (Show, Eq, Ord)

sameQuestion :: Question -> Question -> Bool
sameQuestion left right = questionKey left == questionKey right
  && questionPlan (questionDetails left) == questionPlan (questionDetails right)

type Attention = [Question]
