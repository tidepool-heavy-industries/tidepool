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
import Tidepool.Effects.Core (ContextReadWrite)
import qualified Tidepool.Internal.ActorProfiles as Catalogue
import Tidepool.Effects.Row (Subset)

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

class KnownToolEffects (effects :: [Type -> Type]) where
  toolEffectNames :: Proxy effects -> [Text]

instance KnownToolEffects '[] where
  toolEffectNames _ = []

instance (Catalogue.KnownToolEffect effect, KnownToolEffects effects) => KnownToolEffects (effect ': effects) where
  toolEffectNames _ = Catalogue.knownToolEffectName (Proxy @effect) : toolEffectNames (Proxy @effects)

type family AsyncEffects (effects :: [Type -> Type]) :: Constraint where
  AsyncEffects '[] = ()
  AsyncEffects (ContextReadWrite ': effects) =
    TypeError ('Text "ContextReadWrite cannot be used by an asynchronous tool, model callback, or hook.")
  AsyncEffects (effect ': effects) = AsyncEffects effects
