{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- What a child implementer is asked to do, and what it answers with. The
-- parent writes a `Contract` and hands it to a review; the review checks
-- every claim in an `ImplReport` against its own evidence, and composes a
-- `ReviewBrief` for the read-only reviewer it admits. `ReviewPolicy` lives
-- here too, since a contract carries one per task: it is the first thing a
-- reader tunes when copying this harness into another project.
module Project.Contract
  ( -- The routing policy: this is the first thing to edit when copying the
    -- harness into another project.
    ReviewPolicy (..)
  , defaultReviewPolicy
    -- The contract the parent writes and hands in
  , Contract (..)
  , ImplReport (..)
  , ImplNote (..)
  , ReviewVerdict (..)
  , ReviewBrief (..)
  , RepairTask (..)
    -- What the reviewer is told, and how it is instructed
  , renderBrief
  , reviewerInstructions
  ) where

import Data.Text (Text)
import qualified Data.Text as Text

import qualified Jev.Operators as J
import Tidepool.Actors.Shoal

-- ---------------------------------------------------------------------------
-- The editable policy. Everything a reader tunes first lives here: which
-- measured Jev policy accepts which seam, the floor a Noul answer has to
-- clear, and how many repairs one task buys before the review stops and asks.
-- Named ReviewPolicy because `J.Policy` is Jev's own three thresholds.
-- ---------------------------------------------------------------------------

data ReviewPolicy = ReviewPolicy
  { repairLimit :: Int          -- ^ repairs per task before the review escalates
  , noulFloor :: Double         -- ^ a Noul's `yes` has to clear this (seams d, f, h)
  , itemSatisfied :: Double     -- ^ a checklist item's `yes` at or above this is supported satisfaction
  , itemViolated :: Double      -- ^ at or below this is supported violation; between is unresolved
  , hunksBudget :: Int          -- ^ characters of hunks one Jev packet may carry
  , outputBudget :: Int         -- ^ characters of check output one Jev packet may carry
  , policyAccept :: J.Policy    -- ^ (a) the acceptance gate: a receipt follows
  , policyReflex :: J.Policy    -- ^ (b) reflex residue: read-only classification
  , policyVerdict :: J.Policy   -- ^ (c) reviewer verdict routing
  , policyBrief :: J.Policy     -- ^ (e) reviewer brief: it starts a worker
  , policyStuck :: J.Policy     -- ^ (g) stuck detection: read-only
  }

instance Show ReviewPolicy where
  show policy = "ReviewPolicy repairLimit=" ++ show (repairLimit policy)
    ++ " noulFloor=" ++ show (noulFloor policy)

defaultReviewPolicy :: ReviewPolicy
defaultReviewPolicy = ReviewPolicy
  { repairLimit = 2
  , noulFloor = 0.5
  , itemSatisfied = 0.7
  , itemViolated = 0.3
  , hunksBudget = 24000
  , outputBudget = 4000
  , policyAccept = J.merging
  , policyReflex = J.routing
  , policyVerdict = J.routing
  , policyBrief = J.spawning
  , policyStuck = J.routing
  }

-- ---------------------------------------------------------------------------
-- What the parent writes
-- ---------------------------------------------------------------------------

-- The task contract. The parent fills `contractLikelyMiss` in its pre-wave pass
-- and that text is written verbatim into the review's `item_missing` option: a
-- review with nothing to check the likely miss against cannot catch it.
data Contract = Contract
  { contractTask :: Text
  , contractOwner :: AgentRef
  , contractOwnedPaths :: [Text]
  , contractRequiredTests :: [Text]
  , contractChecklist :: [Text]
  , contractLikelyMiss :: Text
  , contractBase :: GitOid
  , contractPolicy :: ReviewPolicy
  } deriving (Show)

-- The implementer's typed reply. Narration is not evidence: every field here
-- is a claim the review checks against its own `git diff` and the literal
-- output.
data ImplReport = ImplReport
  { reportBranch :: Text
  , reportCommit :: Text
  , reportPaths :: [Text]
  , reportCommand :: Text
  , reportOutput :: Text
  , reportUnresolved :: [Text]
    -- ^ empty for a leaf; a node lists the conditions its subtree could not
    -- resolve, so the parent's review notifies the parent instead of
    -- guessing.
  } deriving (Show, Eq)

-- The implementer's progress payload; `noteEvidence` is what seam (g) watches
-- for a delta that carries no new evidence.
data ImplNote = ImplNote
  { noteText :: Text
  , noteEvidence :: [Text]
  } deriving (Show, Eq)

data ReviewVerdict
  = Accepted Text
  | RepairRequested [Text]
  | PremiseProblem Text
  deriving (Show, Eq)

-- Composed by seam (e): which evidence fields the reviewer actually needs.
data ReviewBrief = ReviewBrief
  { briefTask :: Text
  , briefOwnedPaths :: [Text]
  , briefChecklist :: [Text]
  , briefLikelyMiss :: Text
  , briefBase :: Text
  , briefCandidate :: Text
  , briefHunks :: Text
  , briefTestOutput :: Maybe Text
  , briefCheckOutput :: Maybe Text
  } deriving (Show, Eq)

data RepairTask = RepairTask
  { repairTaskName :: Text
  , repairCandidate :: Text
  , repairFindings :: [Text]
  , repairChecklist :: [Text]
  } deriving (Show, Eq)

-- ---------------------------------------------------------------------------
-- What the reviewer is told
-- ---------------------------------------------------------------------------

reviewerInstructions :: Text
reviewerInstructions = Text.unlines
  [ "You are a fresh read-only reviewer. You read the supplied hunks and the"
  , "acceptance checklist; you do not run the repository and you do not edit."
  , "Return Accepted with the scope your reading establishes, RepairRequested"
  , "with findings that each name a file and a checklist item, or PremiseProblem"
  , "when the task's own contract is what is wrong."
  ]

renderBrief :: ReviewBrief -> Text
renderBrief brief = Text.unlines $
  [ "Task: " <> briefTask brief
  , "Owned paths: " <> Text.intercalate ", " (briefOwnedPaths brief)
  , "Checklist:"
  ] ++ map ("  - " <>) (briefChecklist brief) ++
  [ "Condition most likely to be missed: " <> briefLikelyMiss brief
  , "Base: " <> briefBase brief
  , "Candidate: " <> briefCandidate brief
  , "Hunks:"
  , briefHunks brief
  ] ++ maybe [] (\output -> ["Test output:", output]) (briefTestOutput brief)
    ++ maybe [] (\output -> ["Diff stat:", output]) (briefCheckOutput brief)
