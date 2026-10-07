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
  , Console
  , Actor
  , BoundWorktree
  , WorktreeRegistry
  , WorktreeAllocation
  , WorktreeIntegration
  , Forks
  , Jev
  , ModelCall
  , Lookup
  , Reflect
  , Source
  , Journal
  , RepoEvent
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

import Tidepool.Effects.Core
  ( ActorContext
  , AgentControl
  , Notifications
  , Commands
  , Console
  , Actor
  , AgentInspection
  , AgentLaunch
  , BoundWorktree
  , Forks
  , Jev
  , Journal
  , ModelCall
  , Lookup
  , Reflect
  , RepoEvent
  , Source
  , WorktreeAllocation
  , WorktreeIntegration
  , WorktreeRegistry
  )

import Tidepool.Effects.Row
-- Public requests share their generated rows with native preparation. Rust
-- separately owns the wider authority ceilings and the root's actual grants.
import Tidepool.Internal.ActorProfiles
  ( CoreEffects
  , ResearchEffects
  , ResearchLeafEffects
  , CodingEffects
  , IntegrationEffects
  , ActorEffects
  )
