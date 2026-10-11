{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TypeOperators #-}

-- | Engine-private representation of typed actor references.
--
-- Rust owns the exact routing identity. Haskell owns the successful exit
-- value through the shared managed cell, so copying a reference never creates
-- another runtime root and a closure-valued exit survives actor teardown.
module Tidepool.Actor.Internal
  ( ActorRef (..)
  , EffectProfile (..)
  , profileCode
  , ShutdownReason (..)
  , ActorDefinition (..)
  , pattern ActorDefinition
  , label
  , effectProfile
  , initialization
  , behavior
  , onShutdown
  , actorWorkspace
  , actorSources
  , withSources
  , withLaunchWorkspace
  , tryCallUnit
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Kind (Type)
import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core
  ( Actor (..)
  , ActorEffectProfile (..)
  , ActorCallStatus (..)
  , ActorLocal
  , WorkspaceHandle
  )
import Tidepool.Internal.ActorRef (ActorRef (..))
import Tidepool.Actor.Source (Source)
import Tidepool.Effects.Row (Effects, effectKeys)

-- | The witness fixes the child's exact available effects. Rust independently
-- checks attenuation and concrete resource grants.
data EffectProfile (protocol :: Type -> Type) effs where
  Selected :: Effects effects -> EffectProfile protocol (ActorLocal protocol ': effects)

profileCode :: EffectProfile protocol effects -> ActorEffectProfile
profileCode (Selected effects) = ActorSelectedProfile (effectKeys effects)

-- | Typed reason supplied to an actor's cooperative shutdown hook. Detailed
-- diagnostics remain runtime metadata; cleanup policy branches on lifecycle
-- class rather than parsing prose.
data ShutdownReason
  = ShutdownCompleted
  | ShutdownFailed
  | ShutdownCancelled
  deriving (Show, Eq)

data ActorDefinition startup (protocol :: Type -> Type) exit where
  ActorDefinitionInternal
    :: { -- Human-readable observability label; actor identity comes from
         -- 'ActorRef', so sibling definitions may deliberately reuse it.
         internalLabel :: Text
       , internalEffectProfile :: EffectProfile protocol actorEffs
       , internalInitialization :: startup -> Eff actorEffs initial
       , internalBehavior :: startup -> initial -> Eff actorEffs exit
       , internalOnShutdown :: ShutdownReason -> Eff actorEffs ()
       , internalWorkspace :: Maybe WorkspaceHandle
       , internalSources :: [Source protocol]
       , internalReplacement :: Maybe (exit -> Eff actorEffs exit)
       }
    -> ActorDefinition startup protocol exit

-- | Public full-record construction. Runtime launch recipes are deliberately
-- absent from this pattern: capability modules attach their own opaque recipe
-- without turning 'ActorDefinition' into a generic grant bag.
pattern ActorDefinition
  :: Text
  -> EffectProfile protocol actorEffs
  -> (startup -> Eff actorEffs initial)
  -> (startup -> initial -> Eff actorEffs exit)
  -> (ShutdownReason -> Eff actorEffs ())
  -> ActorDefinition startup protocol exit
pattern ActorDefinition
  { label
  , effectProfile
  , initialization
  , behavior
  , onShutdown
  } <- ActorDefinitionInternal
    { internalLabel = label
    , internalEffectProfile = effectProfile
    , internalInitialization = initialization
    , internalBehavior = behavior
    , internalOnShutdown = onShutdown
    }
  where
    ActorDefinition label effectProfile initialization behavior onShutdown =
      ActorDefinitionInternal
        { internalLabel = label
        , internalEffectProfile = effectProfile
        , internalInitialization = initialization
        , internalBehavior = behavior
        , internalOnShutdown = onShutdown
        , internalWorkspace = Nothing
        , internalSources = []
        , internalReplacement = Nothing
        }

{-# COMPLETE ActorDefinition #-}

-- | An explicitly issued resource grant selected only for startup.
actorWorkspace :: ActorDefinition startup protocol exit -> Maybe WorkspaceHandle
actorWorkspace ActorDefinitionInternal { internalWorkspace = workspace } = workspace

withLaunchWorkspace
  :: WorkspaceHandle
  -> ActorDefinition startup protocol exit
  -> ActorDefinition startup protocol exit
withLaunchWorkspace workspace definition = definition { internalWorkspace = Just workspace }

actorSources :: ActorDefinition startup protocol exit -> [Source protocol]
actorSources ActorDefinitionInternal { internalSources = sources } = sources

withSources
  :: [Source protocol]
  -> ActorDefinition startup protocol exit
  -> ActorDefinition startup protocol exit
withSources sources definition = definition { internalSources = sources }

-- | Internal unit-call boundary for protocols that can turn target lifecycle
-- failure into their own typed control flow.
tryCallUnit
  :: Member Actor effs
  => ActorRef protocol exit
  -> protocol ()
  -> Eff effs (Either Text ())
tryCallUnit (ActorRef actorId incarnation) request = do
  status <- send (ActorTryCallWith (actorId, incarnation) request)
  pure $ case status of
    ActorCallSucceeded -> Right ()
    ActorCallFailed summary -> Left summary
