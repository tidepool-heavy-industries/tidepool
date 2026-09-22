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
  , Lookup
  , Reflect
  , Source
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
  , Actor
  , AgentInspection
  , AgentLaunch
  , BoundWorktree
  , Forks
  , Jev
  , Lookup
  , Reflect
  , Source
  , WorktreeAllocation
  , WorktreeIntegration
  , WorktreeRegistry
  )

import Tidepool.Effects.Row

-- These nominal capabilities are the public residual row. Their operations
-- are supplied by their owner modules. Actor supplies typed Haskell actor
-- execution; workspace operations retain their separate capabilities.

type CoreEffects = '[Replies, Watches, ActorContext, Notifications, Jev, Commands, Actor, Reflect, Lookup]
type ResearchLeafEffects = '[Replies, Watches, ActorContext, BoundWorktree, Notifications, Jev, Commands, Actor, Reflect, Lookup]
-- What a child DECLARES, which is a subset of the role ceiling in
-- `tidepool-actor/src/role.rs`. The ceiling may be wider: it is a maximum, not
-- a request, and every effect named here must have a handler installed in each
-- environment that launches such a child (the recipe-check environment among
-- them). A test in `role.rs` checks the subset direction, not equality.
type ResearchEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree, Notifications, Jev, Commands, Actor, Reflect, Lookup
   ]
-- A coding child reloads the source layer of the checkout it holds; the layer
-- its calls reach is fixed when the actor is constructed, so this never lets
-- it republish the run's own source.
type CodingEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree
   , WorktreeAllocation, WorktreeIntegration, Notifications, Jev, Commands, Actor, Reflect, Lookup
   , Source
   ]
type IntegrationEffects =
  '[ Replies, Watches, ActorContext
   , AgentInspection, BoundWorktree, WorktreeIntegration, Notifications, Jev, Commands, Actor, Reflect, Lookup
   ]

-- | Capabilities installed for the interactive root incarnation.
--
-- @Source@ is the run's own layer, which this incarnation owns. It is also
-- what a coding child holds for its own checkout, and a parent may only grant
-- what it holds — so without it here a root could not start a coding child at
-- all, even though the runtime has always granted it the run's layer.
type ActorEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation
   , WorktreeIntegration, Notifications, Jev, Commands, Actor, Reflect, Lookup
   , Source
   ]
