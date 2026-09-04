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
  , BoundWorktree
  , WorktreeRegistry
  , WorktreeAllocation
  , WorktreeIntegration
  , Forks
  , EffectWitness
  , Effects
  , KnownEffect (effectWitness)
  , KnownEffects (knownEffects)
  , Subset
  , CoreEffects
  , ResearchEffects
  , ResearchCoordinatorEffects
  , CodingEffects
  , ScaffoldEffects
  , IntegrationEffects
  , RootEffects
  ) where

import Data.Kind (Type)

import Tidepool.Agent.Reply (Replies)
import Tidepool.Agent.Watch (Watches)

-- These nominal capabilities are the public residual row. Their operations
-- are supplied by their owner modules; they are not aliases for the broad
-- legacy Actor or Worktree effects.
data ActorContext (a :: Type)
data AgentLaunch (a :: Type)
data AgentInspection (a :: Type)
data AgentControl (a :: Type)
data BoundWorktree (a :: Type)
data WorktreeRegistry (a :: Type)
data WorktreeAllocation (a :: Type)
data WorktreeIntegration (a :: Type)
data Forks (a :: Type)

data EffectWitness (effect :: Type -> Type) = EffectWitness

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

class Contains (effect :: Type -> Type) (effects :: [Type -> Type])
instance {-# OVERLAPPING #-} Contains effect (effect ': effects)
instance {-# OVERLAPPABLE #-} Contains effect effects => Contains effect (other ': effects)

class Subset (child :: [Type -> Type]) (parent :: [Type -> Type])
instance Subset '[] parent
instance (Contains effect parent, Subset effects parent) => Subset (effect ': effects) parent

instance KnownEffect Replies where effectWitness = EffectWitness
instance KnownEffect Watches where effectWitness = EffectWitness
instance KnownEffect ActorContext where effectWitness = EffectWitness
instance KnownEffect AgentLaunch where effectWitness = EffectWitness
instance KnownEffect AgentInspection where effectWitness = EffectWitness
instance KnownEffect AgentControl where effectWitness = EffectWitness
instance KnownEffect BoundWorktree where effectWitness = EffectWitness
instance KnownEffect WorktreeRegistry where effectWitness = EffectWitness
instance KnownEffect WorktreeAllocation where effectWitness = EffectWitness
instance KnownEffect WorktreeIntegration where effectWitness = EffectWitness
instance KnownEffect Forks where effectWitness = EffectWitness

type CoreEffects = '[Replies, Watches, ActorContext]
type ResearchEffects = '[Replies, Watches, ActorContext, BoundWorktree]
type ResearchCoordinatorEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree
   ]
type CodingEffects = '[Replies, Watches, ActorContext, BoundWorktree]
type ScaffoldEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree, WorktreeIntegration
   ]
type IntegrationEffects =
  '[ Replies, Watches, ActorContext
   , AgentInspection, BoundWorktree, WorktreeIntegration
   ]
type RootEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation
   , WorktreeIntegration
   ]
