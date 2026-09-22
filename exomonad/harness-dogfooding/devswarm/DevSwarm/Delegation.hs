{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE StandaloneDeriving #-}
{-# LANGUAGE TypeApplications #-}

-- | DevSwarm's project-local delegation language.
--
-- An owner chooses a semantic request.  'delegateTask' interprets that request
-- through Tidepool's narrow 'Agent.Delegate' capability: private model schemas,
-- worktree allocation, and spawn receipts stay below this module.  Candidate
-- handles are assembled by the interpreter after the delegated model returns;
-- they are never fields the model is asked to manufacture.
module DevSwarm.Delegation
  ( DelegateTask (..)
  , DelegateFailure (..)
  , delegateTask
  , ResearchBrief (..)
  , ResearchFindings (..)
  , ChangeBrief (..)
  , ChangeReport (..)
  , CandidateRef (..)
  , CandidateChange (..)
  , ReviewBrief (..)
  , ReviewFindings (..)
  , ReviewFinding (..)
  , FindingSeverity (..)
  , CandidateDecision (..)
  , RevisionBrief (..)
  , RejectionReason (..)
  ) where

import Control.Monad.Freer (Eff, Member)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import qualified Tidepool.Agent.Delegate as Agent
import qualified Tidepool.Data.Text as T
import Tidepool.Effects.Core
  ( GitOid (..)
  , WorktreeHandle
  , WorktreeId (..)
  , worktreeId
  )
import Tidepool.Prelude

-- | One request an owner may make of a short-lived delegate.  The result
-- index makes the branch at the request site determine the value returned.
data DelegateTask result where
  Investigate :: ResearchBrief -> DelegateTask ResearchFindings
  Implement :: ChangeBrief -> DelegateTask CandidateChange
  Review :: ReviewBrief -> DelegateTask ReviewFindings
  Revise :: CandidateChange -> RevisionBrief -> DelegateTask CandidateChange

deriving instance Show (DelegateTask result)

-- | Delegation failed before producing the requested domain value.  Detailed
-- spawn/workspace diagnostics remain in the rendered reason without making
-- DevSwarm branch on Tidepool's transport stages.
newtype DelegateFailure = DelegateFailure
  { delegateFailureReason :: Text
  }
  deriving (Eq, Show)

data ResearchBrief = ResearchBrief
  { researchQuestion :: Text
  , researchContext :: Text
  }
  deriving (Eq, Show)

-- | A model-produced research report.  These are observations and unknowns,
-- not a mechanical proof object.
data ResearchFindings = ResearchFindings
  { findingsSummary :: Text
  , findingsObservations :: [Text]
  , findingsUnknowns :: [Text]
  }
  deriving (Eq, Show, Generic, FromJSON, JsonSchema)

data ChangeBrief = ChangeBrief
  { changeObjective :: Text
  , changeContext :: Text
  , changeAcceptance :: [Text]
  }
  deriving (Eq, Show)

-- | The implementation delegate's own account of its change.  Git identity
-- comes from the interpreter, separately, in 'CandidateRef'.
data ChangeReport = ChangeReport
  { changeSummary :: Text
  , changeNotes :: [Text]
  }
  deriving (Eq, Show, Generic, FromJSON, JsonSchema)

-- | The durable identity of a candidate.  This is the portion suitable for a
-- node store; it does not grant access to the retained workspace.
data CandidateRef = CandidateRef
  { candidateWorktreeId :: WorktreeId
  , candidateBase :: GitOid
  , candidateHead :: GitOid
  }
  deriving (Eq, Show)

-- | A live implementation proposal.  The opaque handle is intentionally
-- owner-side and in-heap.  An owner may pass it back to 'Review' or 'Revise';
-- the delegate model receives only a rendered brief and its own isolated
-- workspace.
data CandidateChange = CandidateChange
  { candidateRef :: CandidateRef
  , candidateWorktree :: WorktreeHandle
  , candidateReport :: ChangeReport
  }
  deriving (Show)

data ReviewBrief = ReviewBrief
  { reviewCandidate :: CandidateChange
  , reviewConcerns :: [Text]
  }
  deriving (Show)

-- | Advisory findings, deliberately not shaped like 'CandidateDecision'.
data ReviewFindings = ReviewFindings
  { reviewSummary :: Text
  , reviewObservations :: [ReviewFinding]
  }
  deriving (Eq, Show, Generic, FromJSON, JsonSchema)

data ReviewFinding = ReviewFinding
  { findingSeverity :: FindingSeverity
  , findingText :: Text
  }
  deriving (Eq, Show, Generic, FromJSON, JsonSchema)

data FindingSeverity
  = Informational
  | Concerning
  | Blocking
  deriving (Eq, Show, Generic, FromJSON, JsonSchema)

-- | The owning agent's decision.  Reviewer findings are only one input to it.
data CandidateDecision
  = IntegrateCandidate
  | ReviseCandidate RevisionBrief
  | RejectCandidate RejectionReason
  deriving (Eq, Show)

newtype RevisionBrief = RevisionBrief
  { revisionRequest :: Text
  }
  deriving (Eq, Show)

newtype RejectionReason = RejectionReason
  { rejectionReason :: Text
  }
  deriving (Eq, Show)

-- | Interpret one semantic request.  Owners obtain concurrency by composing
-- this ordinary effectful function with 'Tidepool.Async'; no batch protocol is
-- built into the request language.
delegateTask
  :: Member Agent.Delegate effs
  => DelegateTask result
  -> Eff effs (Either DelegateFailure result)
delegateTask (Investigate brief) =
  fmap (mapRun Agent.delegateValue) $
    Agent.delegateTyped @ResearchFindings
      Agent.DelegateBrief
        { Agent.delegateLabel = "investigate"
        , Agent.delegateInstruction = researchPrompt brief
        , Agent.delegateExpected = "Return the requested ResearchFindings record."
        }
delegateTask (Implement brief) =
  fmap (mapRun candidateFromRun) $
    Agent.delegateTyped @ChangeReport
      Agent.DelegateBrief
        { Agent.delegateLabel = "implement"
        , Agent.delegateInstruction = changePrompt brief
        , Agent.delegateExpected = "Implement and commit the change, then return a ChangeReport."
        }
delegateTask (Review brief) =
  fmap (mapRun Agent.delegateValue) $
    Agent.delegateTypedFrom @ReviewFindings
      (candidateWorktree (reviewCandidate brief))
      Agent.DelegateBrief
        { Agent.delegateLabel = "review"
        , Agent.delegateInstruction = reviewPrompt brief
        , Agent.delegateExpected = "Return adversarial ReviewFindings; do not modify the candidate."
        }
delegateTask (Revise candidate revision) =
  fmap (mapRun candidateFromRun) $
    Agent.delegateTypedIn @ChangeReport
      (candidateWorktree candidate)
      Agent.DelegateBrief
        { Agent.delegateLabel = "revise"
        , Agent.delegateInstruction = revisionPrompt candidate revision
        , Agent.delegateExpected = "Revise and commit in the existing worktree, then return a ChangeReport."
        }

mapRun
  :: (Agent.DelegateRun source -> result)
  -> Either Agent.DelegateError (Agent.DelegateRun source)
  -> Either DelegateFailure result
mapRun project = either (Left . DelegateFailure . Agent.renderDelegateError) (Right . project)

candidateFromRun :: Agent.DelegateRun ChangeReport -> CandidateChange
candidateFromRun run =
  CandidateChange
    { candidateRef = CandidateRef
        { candidateWorktreeId = worktreeId (Agent.delegateWorktree run)
        , candidateBase = Agent.delegateBase run
        , candidateHead = Agent.delegateHead run
        }
    , candidateWorktree = Agent.delegateWorktree run
    , candidateReport = Agent.delegateValue run
    }

researchPrompt :: ResearchBrief -> Text
researchPrompt brief =
  T.unlines
    [ "Investigate this question for the owning DevSwarm agent:"
    , researchQuestion brief
    , ""
    , "Relevant context:"
    , researchContext brief
    ]

changePrompt :: ChangeBrief -> Text
changePrompt brief =
  T.unlines
    [ "Implement this change for the owning DevSwarm agent:"
    , changeObjective brief
    , ""
    , "Context:"
    , changeContext brief
    , ""
    , "Acceptance notes:"
    , bullets (changeAcceptance brief)
    ]

reviewPrompt :: ReviewBrief -> Text
reviewPrompt brief =
  T.unlines
    [ "Review the candidate in this isolated checkout adversarially."
    , candidateDescription (reviewCandidate brief)
    , ""
    , "Pay particular attention to:"
    , bullets (reviewConcerns brief)
    ]

revisionPrompt :: CandidateChange -> RevisionBrief -> Text
revisionPrompt candidate revision =
  T.unlines
    [ "Revise the existing candidate in place."
    , candidateDescription candidate
    , ""
    , revisionRequest revision
    ]

candidateDescription :: CandidateChange -> Text
candidateDescription candidate =
  let ref = candidateRef candidate
  in T.unlines
      [ "Candidate worktree: " <> renderWorktreeId (candidateWorktreeId ref)
      , "Base: " <> renderGitOid (candidateBase ref)
      , "Head: " <> renderGitOid (candidateHead ref)
      , "Delegate summary: " <> changeSummary (candidateReport candidate)
      ]

renderGitOid :: GitOid -> Text
renderGitOid (GitOid oid) = oid

renderWorktreeId :: WorktreeId -> Text
renderWorktreeId (WorktreeId wid) = wid

bullets :: [Text] -> Text
bullets [] = "(none supplied)"
bullets xs = T.unlines (map ("- " <>) xs)
