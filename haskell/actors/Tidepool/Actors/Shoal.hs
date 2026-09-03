{-# LANGUAGE DataKinds #-}

-- | The generic Haskell vocabulary available to an interactive Shoal actor.
--
-- This is a facade, not a prewritten actor program. The model authors its
-- declarations and actions incrementally. Project-specific policies such as
-- DevSwarm belong in separately loaded application modules.
module Tidepool.Actors.Shoal
  ( ActorEffects
  , module Tidepool.Actor
  , AgentAction
  , ActionFailure (..)
  , liftAction
  , waitOn
  , nextTurn
  , module Tidepool.Worktree
  ) where

import Tidepool.Actor
import Tidepool.Agent.Action
  ( ActionFailure (..)
  , AgentAction
  , liftAction
  , nextTurn
  , waitOn
  )
import Tidepool.Effects.Core (Actor, AgentSession, Worktree)
import Tidepool.Worktree

-- | Capabilities installed for the interactive root incarnation.
--
-- Naming the row makes the workbench's inferred types readable; it does not
-- prescribe any actor protocol, declarations, state machine, or program.
type ActorEffects = '[AgentSession, Actor, Worktree]
