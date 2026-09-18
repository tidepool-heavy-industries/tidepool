{-# LANGUAGE OverloadedStrings #-}

-- | Named supervision policies for an explicitly chosen child role. These
-- values do not select a role: the parent must make that decision and install
-- the resulting heuristics.
module Project.SupervisionProfiles
  ( SupervisionRole (..)
  , roleName
  , roleHeuristics
  , extendRole
  , reviewerWithoutCandidate
  , researcherOverclaimsEvidence
  ) where

import Data.Text (Text)
import Project.Watchdog
  ( Heuristic (..)
  , Outcome (..)
  , destructiveCommand
  , guessingInsteadOfReading
  , ignoringAFailure
  , repeatingItself
  )

-- | A parent's semantic choice, not a runtime actor role and not something
-- inferred from an actor path.
data SupervisionRole
  = Implementer
  | Reviewer
  | Researcher
  deriving (Eq, Show)

roleName :: SupervisionRole -> Text
roleName Implementer = "implementer"
roleName Reviewer = "reviewer"
roleName Researcher = "researcher"

-- | Shared safeguards plus one role-specific concern where useful.
-- Assignment-aware scope checks belong in the parent's extension because the
-- generic after-tool input does not carry the assignment.
roleHeuristics :: SupervisionRole -> [Heuristic]
roleHeuristics Implementer =
  [ repeatingItself
  , destructiveCommand
  , ignoringAFailure
  , guessingInsteadOfReading
  ]
roleHeuristics Reviewer =
  [ repeatingItself
  , destructiveCommand
  , ignoringAFailure
  , reviewerWithoutCandidate
  ]
roleHeuristics Researcher =
  [ repeatingItself
  , destructiveCommand
  , guessingInsteadOfReading
  , researcherOverclaimsEvidence
  ]

-- | Extend a named baseline using ordinary list composition.
extendRole :: SupervisionRole -> [Heuristic] -> [Heuristic]
extendRole role extra = roleHeuristics role <> extra

reviewerWithoutCandidate :: Heuristic
reviewerWithoutCandidate =
  Heuristic
    "reviewer_without_candidate"
    "Does this call approve or reject work based only on a report, without inspecting the exact candidate or concrete check evidence?"
    0.6
    (Advise "Inspect the exact candidate and concrete check evidence before reaching a review decision.")

researcherOverclaimsEvidence :: Heuristic
researcherOverclaimsEvidence =
  Heuristic
    "researcher_overclaims_evidence"
    "Does this call present an inference as an observed fact, or claim that unseen source or output establishes the conclusion?"
    0.6
    (Advise "Separate observations from inference and preserve the source reference for the deciding evidence.")
