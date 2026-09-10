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
  , Actor
  , BoundWorktree
  , WorktreeRegistry
  , WorktreeAllocation
  , WorktreeIntegration
  , Forks
  , EffectWitness
  , Effects
  , KnownEffect (effectWitness)
  , KnownEffects (knownEffects)
  , effectKeys
  , Subset
  , CoreEffects
  , ResearchEffects
  , ResearchLeafEffects
  , ResearchCoordinatorEffects
  , CodingEffects
  , ScaffoldEffects
  , IntegrationEffects
  , RootEffects
  ) where

import Tidepool.Agent.Reply (Replies)
import Tidepool.Agent.Watch (Watches)
import Tidepool.Effects.Core
  ( ActorContext
  , AgentControl
  , Notifications
  , Actor
  , AgentInspection
  , AgentLaunch
  , BoundWorktree
  , Forks
  , WorktreeAllocation
  , WorktreeIntegration
  , WorktreeRegistry
  )

import Tidepool.Effects.Row

-- These nominal capabilities are the public residual row. Their operations
-- are supplied by their owner modules. Actor supplies typed Haskell actor
-- execution; workspace operations retain their separate capabilities.

type CoreEffects = '[Replies, Watches, ActorContext, Notifications, Actor]
type ResearchLeafEffects = '[Replies, Watches, ActorContext, BoundWorktree, Notifications, Actor]
type ResearchEffects = ResearchCoordinatorEffects
type ResearchCoordinatorEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree, Notifications, Actor
   ]
type CodingEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree
   , WorktreeIntegration, Notifications, Actor
   ]
type ScaffoldEffects = CodingEffects
type IntegrationEffects =
  '[ Replies, Watches, ActorContext
   , AgentInspection, BoundWorktree, WorktreeIntegration, Notifications, Actor
   ]
type RootEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation
   , WorktreeIntegration, Notifications, Actor
   ]
