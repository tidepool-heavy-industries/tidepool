{-# LANGUAGE DataKinds #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

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

import Data.Kind (Type)

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
  , ActorEffectKey (..)
  )

-- These nominal capabilities are the public residual row. Their operations
-- are supplied by their owner modules. Actor supplies typed Haskell actor
-- execution; workspace operations retain their separate capabilities.

data EffectWitness (effect :: Type -> Type) = EffectWitness ActorEffectKey

data Effects (effects :: [Type -> Type]) where
  ENil :: Effects '[]
  ECons :: EffectWitness effect -> Effects effects -> Effects (effect ': effects)

class KnownEffect (effect :: Type -> Type) where
  effectWitness :: EffectWitness effect

class KnownEffects (effects :: [Type -> Type]) where
  knownEffects :: Effects effects

instance KnownEffects '[] where
  knownEffects = ENil

instance (KnownEffect effect, KnownEffects effects) => KnownEffects (effect ': effects) where
  knownEffects = ECons effectWitness knownEffects

effectKeys :: Effects effects -> [ActorEffectKey]
effectKeys ENil = []
effectKeys (ECons (EffectWitness key) rest) = key : effectKeys rest

class Contains (effect :: Type -> Type) (effects :: [Type -> Type])
instance {-# OVERLAPPING #-} Contains effect (effect ': effects)
instance {-# OVERLAPPABLE #-} Contains effect effects => Contains effect (other ': effects)

class Subset (child :: [Type -> Type]) (parent :: [Type -> Type])
instance Subset '[] parent
instance (Contains effect parent, Subset effects parent) => Subset (effect ': effects) parent

instance KnownEffect Replies where effectWitness = EffectWitness EffectReplies
instance KnownEffect Watches where effectWitness = EffectWitness EffectWatches
instance KnownEffect ActorContext where effectWitness = EffectWitness EffectActorContext
instance KnownEffect AgentLaunch where effectWitness = EffectWitness EffectAgentLaunch
instance KnownEffect AgentInspection where effectWitness = EffectWitness EffectAgentInspection
instance KnownEffect AgentControl where effectWitness = EffectWitness EffectAgentControl
instance KnownEffect BoundWorktree where effectWitness = EffectWitness EffectBoundWorktree
instance KnownEffect WorktreeRegistry where effectWitness = EffectWitness EffectWorktreeRegistry
instance KnownEffect WorktreeAllocation where effectWitness = EffectWitness EffectWorktreeAllocation
instance KnownEffect WorktreeIntegration where effectWitness = EffectWitness EffectWorktreeIntegration
instance KnownEffect Forks where effectWitness = EffectWitness EffectForks
instance KnownEffect Notifications where effectWitness = EffectWitness EffectNotifications
instance KnownEffect Actor where effectWitness = EffectWitness EffectActor

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
