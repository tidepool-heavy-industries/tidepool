{-# LANGUAGE OverloadedStrings #-}
module Project.Types where

import Data.Text (Text)
import Tidepool.Actors.Exomonad
  ( AgentRef, Label, ForkGroupPath, ForkEffort, GitOid, Model, WatchLabel
  , CampaignLabel, campaignLabel, batch
  )
import Tidepool.Agent.Assignment (labelText)
import Tidepool.Inspection (Display (..), application, displayRecord)
import Tidepool.Worktree (renderGitOid)

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

-- Turn an already-validated fork Label into a group path's campaign segment.
-- Label and CampaignLabel share the same kebab-case, <=48-char validator
-- (Tidepool.Agent.Assignment.Internal / Tidepool.Actors.Unfold), so a Label's
-- own text always satisfies campaignLabel; the Left branch is unreachable in
-- practice, not a real runtime possibility this constructor has to reject.
labelCampaign :: Label -> CampaignLabel
labelCampaign label = either (error . show) id (campaignLabel (labelText label))

-- A defaults constructor for the harness's number-one missing primitive (the
-- wave-3 root interview): a fork that only needs "objective, owned paths,
-- acceptance". Derives a fork group from the label (batch <label> "work",
-- the least surprising reading of ForkGroupPath's batch/subgroup shapes: a
-- fresh two-segment path named after who is doing the work), points at the
-- workspace's shared vocabulary plan, and leaves no rationale or accepted
-- decisions yet -- ordinary record fields any caller can still override with
-- a record update. The source revision has no sensible default: a
-- WorktreeSeed (Project.Work's lunaTaskFrom/solTaskFrom) does not carry a
-- resolvable GitOid purely, so it is unavoidable to require one here,
-- explicitly, last.
task :: Label -> Text -> [Text] -> Text -> GitOid -> Task
task label objective owned accept source = Task
  { taskGroup = batch (labelCampaign label) "work"
  , planPath = ".exomonad/plans/language.md"
  , taskSource = source
  , obligation = objective
  , rationale = ""
  , ownedPaths = owned
  , acceptance = accept
  , acceptedDecisions = []
  }

-- The owner records its supported choice at the incorporated source revision.
-- This is evidence-bearing task data; the record grants no runtime authority.
data AcceptedDecision = AcceptedDecision
  { decisionQuestion :: Question
  , decisionSource :: GitOid
  , decisionSummary :: Text
  , decisionEvidence :: [Text]
  } deriving (Show, Eq)

data Candidate = Candidate
  { candidateCommit :: GitOid
  , checkedCommands :: [Text]
  , remainingGates :: [Text]
  } deriving (Show, Eq)

instance Display Candidate where
  displayTree = displayTreePrec 0
  displayTreePrec p c = displayRecord p "Candidate"
    [ ("candidateCommit", displayTree (candidateCommit c))
    , ("checkedCommands", displayTree (checkedCommands c))
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

data ReviewedCandidate = ReviewedCandidate
  { reviewedBasis :: ReviewBasis
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

instance Display value => Display (Outcome value) where
  displayTree = displayTreePrec 0
  displayTreePrec p (Produced value) = application p "Produced" [displayTreePrec 11 value]
  displayTreePrec p (Blocked reason evidence) =
    application p "Blocked" [displayTreePrec 11 reason, displayTreePrec 11 evidence]

data CheckedDelivery = Delivered ReviewedCandidate GitOid [Text]
  deriving (Show, Eq)

type Delivery = Outcome CheckedDelivery

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
data WorkProgress = WorkProgress
  { workEvidence :: [Candidate]
  , workQuestions :: Attention
  } deriving (Show, Eq)

instance Display WorkProgress where
  displayTree = displayTreePrec 0
  displayTreePrec p w = displayRecord p "WorkProgress"
    [ ("workEvidence", displayTree (workEvidence w))
    , ("workQuestions", displayTree (workQuestions w))
    ]

-- Only unresolved decisions/blockers needing the recipient's action. Successful
-- incorporation and unchanged standing gates belong to evidence, not questions.
-- Retain questions until a supported resolution, including across source closure.
data Question = Question
  { questionKey :: Text
  , questionDetails :: DesignQuestion
  } deriving (Show, Eq, Ord)

type Attention = [Question]
