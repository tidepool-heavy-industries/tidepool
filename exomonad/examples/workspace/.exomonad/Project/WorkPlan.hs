{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}

-- | A typed description of one coordinator's work. The constructors preserve
-- dependencies as Haskell values: a review consumes a developed candidate,
-- integration consumes an exact review, and verification consumes a checked
-- publication. The interpreter owns admissions and resumes from source events.
module Project.WorkPlan
  ( WorkPlan (..), ComponentScope (..), Development (..), WorkerAssignment (..), ReviewSpec (..)
  , IntegrationSpec (..), Verification (..)
  , Developed (..), Reviewed (..), CheckedSource (..), AcceptedSource (..)
  , PlanFailure (..), NodeKind (..), PlanPosition (..), enterComponent
  , checkComponentTask, checkComponentAmendment, effectiveRepairLimit
  , PlanStep (..), stepPlan
  , develop, review, integrate, verify, parallel, lunaWorker
  ) where

import Data.Text (Text)
import qualified Tidepool.Actor.Record as R
import Tidepool.Inspection (Display (..), displayRecord)
import Tidepool.Actors.Exomonad
  ( AgentRef, Branch, CodingEffects, Label, GitOid, Progress, Response
  , ResponseResult, WorktreeId, ForkEffort, WorktreeSeed )
import Project.FocusedGateExample (PlanCheck, PlanReport)
import Project.Work (lunaTaskInputFrom, taskContext)
import Project.Merge (MergeTarget, MergeResult)
import Project.ReviewFlow (ReviewFlowPolicy, ReviewFlowState)
import Project.Types
  ( Task (..), Candidate, Outcome, WorkProgress, ReviewedCheckpoint
  , PlanAmendment (..), Incorporation )

-- | The lead may refine implementation within this contract. A changed
-- acceptance criterion or sibling obligation goes back to the owning plan.
data ComponentScope = ComponentScope
  { componentTask :: Task
  , componentLead :: AgentRef
  , componentRepairBudget :: Int
  }

-- | A retained worker can receive another request. A new worker is launched
-- with the supplied typed branch, including its context checkpoint and budget.
-- The coordinator, rather than the author of this value, performs admission.
data Development
  = RetainedWorker AgentRef Label Task
  | ForkWorker Task
      (WorkerAssignment -> Branch CodingEffects WorkerAssignment (Outcome Candidate))

-- | A new worker receives the exact reporting route in its typed input. The
-- route's sender identity is still checked by the coordinator at report time.
data WorkerAssignment = WorkerAssignment
  { workerTask :: Task
  , incorporationRoute :: R.Send Incorporation
  }

instance Display WorkerAssignment where
  displayTree assignment = displayRecord 0 "WorkerAssignment"
    [("workerTask", displayTree (workerTask assignment))]

lunaWorker
  :: Label -> ForkEffort -> WorktreeSeed -> WorkerAssignment
  -> Branch CodingEffects WorkerAssignment (Outcome Candidate)
lunaWorker label effort source =
  lunaTaskInputFrom label effort source (taskContext . workerTask)

data ReviewSpec = ReviewSpec
  { reviewTask :: Task
  , reviewCheckout :: WorktreeId
  , reviewPolicy :: ReviewFlowPolicy
  , reviewChecks :: [PlanCheck]
  }

data IntegrationSpec = IntegrationSpec
  { integrationTarget :: MergeTarget
  , integrationTaskName :: Text
  , integrationMessage :: Text
  }

-- | The check plan and product acceptance predicate belong to the caller.
-- A successful check report is required before the predicate can accept.
data Verification a = Verification
  { verificationChecks :: [PlanCheck]
  , verificationAccept :: CheckedSource -> PlanReport -> Either Text a
  }

-- | These values are issued only after the interpreter has checked each
-- source's exact request and checkout evidence. Constructors remain visible
-- for observation; they do not grant runtime authority.
data Developed = Developed
  { developedTask :: Task
  , developedCandidate :: Candidate
  , developedRequest :: Response (Outcome Candidate)
  , developedProgress :: Progress WorkProgress
  , developedReceipt :: ResponseResult (Outcome Candidate)
  }

data Reviewed = Reviewed
  { reviewedDevelopment :: Developed
  , reviewedProof :: ReviewedCheckpoint
  , reviewedFlow :: ReviewFlowState
  }

data CheckedSource = CheckedSource
  { checkedReview :: Reviewed
  , checkedHead :: GitOid
  , checkedPreviousHead :: GitOid
  , checkedMerge :: MergeResult
  }

data AcceptedSource a = AcceptedSource
  { acceptedValue :: a
  , acceptedCheckedSource :: CheckedSource
  , acceptedReport :: PlanReport
  }

-- | Failures stay attached to the stage that refused them. In particular,
-- an uncertain admission is terminal for this attempt; it is never retried
-- through a replacement request.
data NodeKind = DevelopmentNode | ReviewNode | IntegrationNode | VerificationNode
  deriving (Show, Eq)

data PlanFailure
  = AdmissionRefused NodeKind Text
  | SourceRefused NodeKind Text
  | WorkerBlocked Text [Text]
  | ReviewStopped Text
  | IntegrationStopped MergeResult
  | VerificationStopped Text
  | ComponentExceededScope Text
  | ParallelStopped [PlanFailure]
  deriving (Show)

-- | Pure location state used when composing nested components. It is a
-- projection of the authored plan, not a second request or history registry.
data PlanPosition = PlanPosition
  { positionComponents :: [ComponentScope]
  , positionNode :: Maybe NodeKind
  }

enterComponent :: ComponentScope -> PlanPosition -> PlanPosition
enterComponent scope position = position
  { positionComponents = positionComponents position ++ [scope]
  , positionNode = Nothing
  }

-- | A child lead may narrow paths while keeping the declared acceptance.
-- Exact source movement is checked by the stage that admits that revision.
checkComponentTask :: ComponentScope -> Task -> Either PlanFailure ()
checkComponentTask scope proposed
  | planPath proposed /= planPath parent = Left (ComponentExceededScope "plan path changed")
  | acceptance proposed /= acceptance parent = Left (ComponentExceededScope "acceptance changed")
  | any (`notElem` ownedPaths parent) (ownedPaths proposed) =
      Left (ComponentExceededScope "task claims paths outside the component")
  | otherwise = Right ()
  where parent = componentTask scope

checkComponentAmendment :: ComponentScope -> PlanAmendment -> Either PlanFailure ()
checkComponentAmendment scope amendment
  | any (`notElem` ownedPaths (componentTask scope)) (amendmentPaths amendment) =
      Left (ComponentExceededScope "amendment changes paths outside the component")
  | null (amendmentObligations amendment) =
      Left (ComponentExceededScope "amendment names no local obligation")
  | otherwise = Right ()

effectiveRepairLimit :: [ComponentScope] -> Either PlanFailure Int
effectiveRepairLimit scopes = case map componentRepairBudget scopes of
  budgets | any (< 0) budgets -> Left (ComponentExceededScope "negative repair budget")
  budgets -> Right (minimum (2 : budgets))

-- | Sequential composition carries actual typed values through ordinary do.
-- Parallel branches are independent until both have settled; the interpreter
-- joins their typed results without making one branch's result a sibling input.
data WorkPlan a where
  Pure :: a -> WorkPlan a
  Bind :: WorkPlan b -> (b -> WorkPlan a) -> WorkPlan a
  Parallel :: WorkPlan a -> WorkPlan b -> WorkPlan (a, b)
  Component :: ComponentScope -> WorkPlan a -> WorkPlan a
  Develop :: Development -> WorkPlan Developed
  Review :: ReviewSpec -> Developed -> WorkPlan Reviewed
  Integrate :: IntegrationSpec -> Reviewed -> WorkPlan CheckedSource
  Verify :: Verification a -> CheckedSource -> WorkPlan (AcceptedSource a)

-- | One pure reduction of an authored plan. A node interpreter supplies the
-- value accepted by its exact stage; the continuation preserves every later
-- typed dependency. No request is admitted while reducing this structure.
data PlanStep a where
  Finished :: a -> PlanStep a
  NeedDevelopment :: [ComponentScope] -> Development
    -> (Developed -> WorkPlan a) -> PlanStep a
  NeedReview :: [ComponentScope] -> ReviewSpec -> Developed
    -> (Reviewed -> WorkPlan a) -> PlanStep a
  NeedIntegration :: [ComponentScope] -> IntegrationSpec -> Reviewed
    -> (CheckedSource -> WorkPlan a) -> PlanStep a
  NeedVerification :: [ComponentScope] -> Verification b -> CheckedSource
    -> (AcceptedSource b -> WorkPlan a) -> PlanStep a
  NeedParallel :: [ComponentScope] -> WorkPlan b -> WorkPlan c
    -> ((b, c) -> WorkPlan a) -> PlanStep a

stepPlan :: WorkPlan a -> PlanStep a
stepPlan = step []
  where
    step :: [ComponentScope] -> WorkPlan x -> PlanStep x
    step scopes plan = case plan of
      Pure value -> Finished value
      Bind prior continue -> mapStep scopes continue (step scopes prior)
      Parallel left right -> NeedParallel scopes left right Pure
      Component scope nested -> scopeStep scope (step (scopes ++ [scope]) nested)
      Develop input -> NeedDevelopment scopes input Pure
      Review spec candidate -> NeedReview scopes spec candidate Pure
      Integrate spec reviewed -> NeedIntegration scopes spec reviewed Pure
      Verify spec checked -> NeedVerification scopes spec checked Pure

    mapStep :: [ComponentScope] -> (x -> WorkPlan y) -> PlanStep x -> PlanStep y
    mapStep scopes continue current = case current of
      Finished value -> step scopes (continue value)
      NeedDevelopment scopes input next ->
        NeedDevelopment scopes input (\value -> Bind (next value) continue)
      NeedReview scopes spec candidate next ->
        NeedReview scopes spec candidate (\value -> Bind (next value) continue)
      NeedIntegration scopes spec reviewed next ->
        NeedIntegration scopes spec reviewed (\value -> Bind (next value) continue)
      NeedVerification scopes spec checked next ->
        NeedVerification scopes spec checked (\value -> Bind (next value) continue)
      NeedParallel scopes left right next ->
        NeedParallel scopes left right (\value -> Bind (next value) continue)

    scopeStep :: ComponentScope -> PlanStep x -> PlanStep x
    scopeStep scope current = case current of
      Finished value -> Finished value
      NeedDevelopment scopes input next ->
        NeedDevelopment scopes input (Component scope . next)
      NeedReview scopes spec candidate next ->
        NeedReview scopes spec candidate (Component scope . next)
      NeedIntegration scopes spec reviewed next ->
        NeedIntegration scopes spec reviewed (Component scope . next)
      NeedVerification scopes spec checked next ->
        NeedVerification scopes spec checked (Component scope . next)
      NeedParallel scopes left right next ->
        NeedParallel scopes (Component scope left) (Component scope right)
          (Component scope . next)

instance Functor WorkPlan where
  fmap f plan = Bind plan (Pure . f)

instance Applicative WorkPlan where
  pure = Pure
  function <*> argument = Bind function (\f -> fmap f argument)

instance Monad WorkPlan where
  (>>=) = Bind

develop :: Development -> WorkPlan Developed
develop = Develop

review :: ReviewSpec -> Developed -> WorkPlan Reviewed
review = Review

integrate :: IntegrationSpec -> Reviewed -> WorkPlan CheckedSource
integrate = Integrate

verify :: Verification a -> CheckedSource -> WorkPlan (AcceptedSource a)
verify = Verify

parallel :: WorkPlan a -> WorkPlan b -> WorkPlan (a, b)
parallel = Parallel
