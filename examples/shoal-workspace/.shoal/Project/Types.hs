module Project.Types where

import Data.Text (Text)
import Tidepool.Actors.Shoal (AgentRef, BranchLabel, ForkGroupPath, WorktreeSeed, ResponseFailure)

-- Project language is ordinary source, independent of runtime authority.
data Task = Task
  { planPath :: Text
  , obligation :: Text
  , acceptance :: Text
  } deriving (Show, Eq)

data Candidate = Candidate
  { candidateCommit :: Text
  , checkedCommands :: [Text]
  , remainingGates :: [Text]
  } deriving (Show, Eq)

-- Review owns the next repair request; an implementer reference does not grant
-- access to the original requester's response or worktree authority.
data ReviewTask = ReviewTask
  { reviewAssignment :: Task
  , reviewInput :: Candidate
  , reviewImplementer :: AgentRef
  }

data ReviewedCandidate = ReviewedCandidate
  { reviewedCandidate :: Candidate
  , reviewHead :: Text
  , reviewChecks :: [Text]
  , reviewRationale :: Text
  } deriving (Show, Eq)

data RepairTask = RepairTask
  { repairAssignment :: Task
  , repairInput :: Candidate
  , repairFindings :: [Text]
  } deriving (Show, Eq)

data DesignQuestion = DesignQuestion
  { questionPlan :: Text
  , questionSource :: Text
  , questionFinding :: Text
  , questionEvidence :: [Text]
  , questionAlternatives :: [Text]
  , questionUnblocks :: [Text]
  } deriving (Show, Eq)

data DesignAnswer
  = Decision Text [Text]
  | AmendPlan Text [Text]
  | NeedEvidence [Text]
  deriving (Show, Eq)

data ReviewDecision = Accepted ReviewedCandidate | Repair Candidate Text | NeedsDesign DesignQuestion
  deriving (Show, Eq)

data Delivery
  = Integrated Text [Text]
  | Preparation Candidate
  | Blocked Text
  | ReviewBlocked Candidate Text
  | DesignBlocked DesignQuestion
  | ExecutionUnavailable ResponseFailure
  deriving (Show, Eq)

-- The planner supplies each branch's decomposition and source choices once.
data DeliveryLane = DeliveryLane
  { laneTask :: Task
  , implementationGroup :: ForkGroupPath
  , implementationLabel :: BranchLabel
  , implementationSeed :: WorktreeSeed
  , reviewGroup :: ForkGroupPath
  , reviewLabel :: BranchLabel
  , integrationGroup :: ForkGroupPath
  , integrationLabel :: BranchLabel
  , integrationSeed :: WorktreeSeed
  }
