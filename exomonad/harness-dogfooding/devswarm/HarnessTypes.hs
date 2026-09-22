{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | The small durable compatibility shell around DevSwarm's live owner tree.
-- Product truth remains in git and retained worktrees; this state only seeds
-- the next root owner session and remembers its last rendered outcome.
module HarnessTypes
  ( State (..)
  , initialState
  , SeedObjective (..)
  , RootOutcome
  , module DevSwarm.Delegation
  , module DevSwarm.Types
  , render
  ) where

import DevSwarm.Delegation
import DevSwarm.Types
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

data State = State
  { objective :: Text
  , turnsCompleted :: Int
  , lastOutcome :: Maybe Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

initialState :: State
initialState = State
  { objective = ""
  , turnsCompleted = 0
  , lastOutcome = Nothing
  }

data SeedObjective = SeedObjective
  { seedObjective :: Text
  }
  deriving (Eq, Show, Generic, ToJSON, FromJSON, JsonSchema)

-- | The root answer type is named here so the driver's answer contract imports
-- this module, which deliberately re-exports the DevSwarm owner vocabulary.
type RootOutcome = OwnerOutcome

render :: State -> Text
render st =
  [fmt|You own the root DevSwarm node for this Tidepool development run.

Objective: {objectiveLine}
Turns completed: {show st.turnsCompleted}
{previous}

The organization is expressed directly in Haskell:

- `fork @OwnerOutcome (renderNodeBrief brief)` creates one recursively capable
  child owner session.
- Several owners run concurrently with ordinary `async` and `wait`.
- `delegateTask (Investigate ...)`, `(Implement ...)`, `(Review ...)`, or
  `(Revise ...)` creates short-lived repository delegates.
- Implementation delegates return live `CandidateChange` capabilities.
  Review and revision receive those values; the delegated model never receives
  an owner capability.
- Worktrees are proposals, not orchestration nodes. Fork an owner only when
  work deserves durable decomposition.

Use project-local Text heuristics freely for routing. Local tests, hooks, CI,
and adversarial review are signals for your judgment, not proof objects.
Finalize exactly one `OwnerOutcome`.|]
  where
    objectiveLine = if objective st == "" then "(being seeded this turn)" else objective st
    previous = case lastOutcome st of
      Nothing -> "No prior root outcome."
      Just outcome -> "Previous root outcome:\n" <> outcome
