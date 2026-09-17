{-# LANGUAGE DataKinds #-}

-- | Static, extensible capability rows for interactive actors.
--
-- Constructors for witnesses are intentionally private. A row can only be
-- reflected when every effect has a registered 'KnownEffect' instance.
module Tidepool.Actors.Role
  ( ActorContext
  , AgentLaunch
  , AgentInspection
  , AgentControl
  , Notifications
  , Commands
  , Actor
  , BoundWorktree
  , WorktreeRegistry
  , WorktreeAllocation
  , WorktreeIntegration
  , Forks
  , Jev
  , EffectWitness
  , Effects
  , KnownEffect (effectWitness)
  , KnownEffects (knownEffects)
  , effectKeys
  , Subset
  , CoreEffects
  , ResearchEffects
  , ResearchLeafEffects
  , CodingEffects
  , IntegrationEffects
  , ActorEffects
  ) where

import Tidepool.Agent.Reply (Replies)
import Tidepool.Agent.Watch (Watches)
import Tidepool.Effects.Core
  ( ActorContext
  , AgentControl
  , Notifications
  , Commands
  , Console
  , Sleep
  , Actor
  , AgentInspection
  , AgentLaunch
  , BoundWorktree
  , Forks
  , Jev
  , WorktreeAllocation
  , WorktreeIntegration
  , WorktreeRegistry
  )

import Tidepool.Effects.Row

-- These nominal capabilities are the public residual row. Their operations
-- are supplied by their owner modules. Actor supplies typed Haskell actor
-- execution; workspace operations retain their separate capabilities.

type CoreEffects = '[Replies, Watches, ActorContext, Notifications, Jev, Commands, Actor]
type ResearchLeafEffects = '[Replies, Watches, ActorContext, BoundWorktree, Notifications, Jev, Commands, Actor]
-- The research and coding rows mirror `EffectiveRole::research()` and
-- `EffectiveRole::coding()` in `tidepool-actor/src/role.rs`; a parity test
-- there reads this file and fails when the two drift.
type ResearchEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree
   , Sleep, Notifications, Jev, Commands, Console, Actor
   ]
type CodingEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree
   , WorktreeAllocation, WorktreeIntegration
   , Sleep, Notifications, Jev, Commands, Console, Actor
   ]
type IntegrationEffects =
  '[ Replies, Watches, ActorContext
   , AgentInspection, BoundWorktree, WorktreeIntegration, Notifications, Jev, Commands, Actor
   ]

-- | Capabilities installed for the interactive root incarnation.
type ActorEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation
   , WorktreeIntegration, Notifications, Jev, Commands, Actor
   ]
