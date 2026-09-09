module Project.Types where

import Data.Text (Text)
import Tidepool.Actors.Shoal (AgentRef, BranchLabel, ForkGroupPath, ForkEffort, WatchLabel)

-- A task is the understanding handed to a fresh context, not a workflow stage.
data Task = Task
  { taskGroup :: ForkGroupPath
  , planPath :: Text
  , taskSource :: Text
  , obligation :: Text
  , rationale :: Text
  , ownedPaths :: [Text]
  , acceptance :: Text
  , acceptedDecisions :: [AcceptedDecision]
  } deriving (Show, Eq)

-- The owner records its supported choice at the incorporated source revision.
-- This is evidence-bearing task data; the record grants no runtime authority.
data AcceptedDecision = AcceptedDecision
  { decisionQuestion :: Question
  , decisionSource :: Text
  , decisionSummary :: Text
  , decisionEvidence :: [Text]
  } deriving (Show, Eq)

data Candidate = Candidate
  { candidateCommit :: Text
  , checkedCommands :: [Text]
  , remainingGates :: [Text]
  } deriving (Show, Eq)

-- Queuing a repair to the owner of a pending delivery would deadlock it.
-- A separate implementer is available for repair after returning its candidate.
data RepairOwner = OwnerRepairs | RetainedImplementer AgentRef

data ReviewTask = ReviewTask
  { reviewAssignment :: Task
  , reviewInput :: Candidate
  , repairOwner :: RepairOwner
  }

data ReviewedCandidate = ReviewedCandidate
  { acceptedAssignment :: Task
  , reviewedCandidate :: Candidate
  , reviewChecks :: [Text]
  , reviewRationale :: Text
  } deriving (Show, Eq)

data ReviewDecision
  = Accepted ReviewedCandidate
  | Repair Candidate [Text]
  deriving (Show, Eq)

data RepairTask = RepairTask
  { repairAssignment :: Task
  , repairInput :: Candidate
  , repairFindings :: [Text]
  } deriving (Show, Eq)

-- Reviewed source and the resulting integration head are different facts.
-- The remaining product gates stay attached to the exact reviewed candidate.
data Outcome value = Produced value | Blocked Text [Text]
  deriving (Show, Eq)

data CheckedDelivery = Delivered ReviewedCandidate Text [Text]
  deriving (Show, Eq)

type Delivery = Outcome CheckedDelivery

data DesignQuestion = DesignQuestion
  { questionPlan :: Text
  , questionSource :: Text
  , questionFinding :: Text
  , questionEvidence :: [Text]
  , questionAlternatives :: [Text]
  , questionUnblocks :: [Text]
  } deriving (Show, Eq, Ord)

data DesignAnswer
  = Decision Text [Text]
  | AmendPlan PlanAmendment
  | NeedEvidence [Text]
  deriving (Show, Eq)

data PlanAmendment = PlanAmendment
  { amendmentBase :: Text
  , amendmentCommit :: Text
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
  = Incorporated PlanAmendment Text [Text]
  | IncorporationBlocked PlanAmendment Text [Text]
  deriving (Show, Eq)

data DesignSlot = DesignSlot
  { specialistPlan :: Text
  , specialistGroup :: ForkGroupPath
  , specialistLabel :: BranchLabel
  , specialistWatch :: WatchLabel
  , specialistModel :: Text
  , specialistEffort :: ForkEffort
  }

-- Evidence and questions are independently useful progress payloads. They are
-- authored data, never authority to retry, stop or release a resource.
data WorkProgress = WorkProgress
  { workEvidence :: [Candidate]
  , workQuestions :: Attention
  } deriving (Show, Eq)

-- Only unresolved decisions/blockers needing the recipient's action. Successful
-- incorporation and unchanged standing gates belong to evidence, not questions.
-- Retain questions until a supported resolution, including across source closure.
data Question = Question
  { questionKey :: Text
  , questionDetails :: DesignQuestion
  } deriving (Show, Eq, Ord)

type Attention = [Question]
