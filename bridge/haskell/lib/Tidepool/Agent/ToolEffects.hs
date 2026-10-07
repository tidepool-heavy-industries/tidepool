{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | Notebook effect profiles. Invocation-only effects have tool witnesses,
-- while the actor row's witnesses continue to describe delegable effects.
module Tidepool.Agent.ToolEffects
  ( SyncEffects, KnownToolEffects, toolEffectNames, AsyncEffects, Subset
  , ToolSchedule (..), KnownToolSchedule (toolSchedule), SupportedEffects, ValidToolProfile
  ) where

import Data.Kind (Constraint, Type)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import GHC.TypeLits (ErrorMessage (..), TypeError)
import Tidepool.Effects.Core (ActorEffectKey (..), ContextReadWrite)
import Tidepool.Effects.Row (KnownEffect, KnownEffects (knownEffects), Subset, effectKeys)

type SyncEffects effects = ContextReadWrite ': effects

data ToolSchedule = Asynchronous | BeforeNextInference
  deriving (Eq, Show)

class KnownToolSchedule (schedule :: ToolSchedule) where
  toolSchedule :: Proxy schedule -> ToolSchedule

instance KnownToolSchedule 'Asynchronous where toolSchedule _ = Asynchronous
instance KnownToolSchedule 'BeforeNextInference where toolSchedule _ = BeforeNextInference

type family SupportedEffects (schedule :: ToolSchedule) (base :: [Type -> Type]) where
  SupportedEffects 'Asynchronous base = base
  SupportedEffects 'BeforeNextInference base = SyncEffects base

type family ValidToolProfile (schedule :: ToolSchedule) (effects :: [Type -> Type]) :: Constraint where
  ValidToolProfile 'Asynchronous effects = AsyncEffects effects
  ValidToolProfile 'BeforeNextInference effects = ()

class KnownToolEffect (effect :: Type -> Type) where
  toolEffectName :: Proxy effect -> Text

instance {-# OVERLAPPABLE #-} KnownEffect effect => KnownToolEffect effect where
  toolEffectName _ = case effectKeys (knownEffects @'[effect]) of
    [key] -> actorEffectName key
    _ -> error "a singleton effect witness must name one effect"

instance {-# OVERLAPPING #-} KnownToolEffect ContextReadWrite where
  toolEffectName _ = "ContextReadWrite"

class KnownToolEffects (effects :: [Type -> Type]) where
  toolEffectNames :: Proxy effects -> [Text]

instance KnownToolEffects '[] where
  toolEffectNames _ = []

instance (KnownToolEffect effect, KnownToolEffects effects) => KnownToolEffects (effect ': effects) where
  toolEffectNames _ = toolEffectName (Proxy @effect) : toolEffectNames (Proxy @effects)

type family AsyncEffects (effects :: [Type -> Type]) :: Constraint where
  AsyncEffects '[] = ()
  AsyncEffects (ContextReadWrite ': effects) =
    TypeError ('Text "ContextReadWrite cannot be used by an asynchronous tool, model callback, or hook.")
  AsyncEffects (effect ': effects) = AsyncEffects effects

actorEffectName :: ActorEffectKey -> Text
actorEffectName key = case key of
  EffectResourceScopes -> "ResourceScopes"
  EffectReplies -> "Replies"
  EffectWatches -> "Watches"
  EffectForks -> "Forks"
  EffectActorContext -> "ActorContext"
  EffectAgentLaunch -> "AgentLaunch"
  EffectAgentInspection -> "AgentInspection"
  EffectAgentControl -> "AgentControl"
  EffectBoundWorktree -> "BoundWorktree"
  EffectWorktreeRegistry -> "WorktreeRegistry"
  EffectWorktreeAllocation -> "WorktreeAllocation"
  EffectWorktreeIntegration -> "WorktreeIntegration"
  EffectSleep -> "Sleep"
  EffectCommands -> "Commands"
  EffectConsole -> "Console"
  EffectNotifications -> "Notifications"
  EffectJev -> "Jev"
  EffectModelCall -> "ModelCall"
  EffectActor -> "Actor"
  EffectReflect -> "Reflect"
  EffectLookup -> "Lookup"
  EffectSource -> "Source"
  EffectRepoEvent -> "RepoEvent"
  EffectJournal -> "Journal"
