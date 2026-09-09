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
  , ReadOnlyEffects
  , ReadWriteEffects
  , ShutdownReason (..)
  , ActorDefinition
  , pattern ActorDefinition
  , label
  , effectProfile
  , initialization
  , behavior
  , onShutdown
  , actorLaunchWorktrees
  , actorSources
  , withSources
  , withLaunchWorktree
  , tryCallUnit
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Kind (Type)
import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core
  ( Actor (..)
  , ActorCallStatus (..)
  , ActorLocal
  , AgentSession
  , AgentTools
  , FsRead
  , FsWrite
  , Notifications
  , Worktree
  )
import Tidepool.Internal.ActorRef (ActorRef (..))
import Tidepool.Actor.Source (Source)

-- | Experimental named profiles for resident Haskell effect rows. The witness
-- fixes the child row, while Rust independently validates spawn attenuation
-- and principal- or grant-sensitive operations. These profiles do not
-- sandbox native tools belonging to an attached coding-agent process.
data EffectProfile (protocol :: Type -> Type) effs where
  ReadOnly :: EffectProfile protocol (ReadOnlyEffects protocol)
  ReadWrite :: EffectProfile protocol (ReadWriteEffects protocol)

type ReadOnlyEffects protocol =
  '[ ActorLocal protocol
   , AgentTools
   , AgentSession
   , Actor
   , FsRead
   , Worktree
   , Notifications
   ]

type ReadWriteEffects protocol = FsWrite ': ReadOnlyEffects protocol

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
       , internalLaunchWorktrees :: [Text]
       , internalSources :: [Source protocol]
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
        , internalLaunchWorktrees = []
        , internalSources = []
        }

{-# COMPLETE ActorDefinition #-}

actorLaunchWorktrees :: ActorDefinition startup protocol exit -> [Text]
actorLaunchWorktrees
  ActorDefinitionInternal { internalLaunchWorktrees = worktrees } = worktrees

actorSources :: ActorDefinition startup protocol exit -> [Source protocol]
actorSources ActorDefinitionInternal { internalSources = sources } = sources

withSources
  :: [Source protocol]
  -> ActorDefinition startup protocol exit
  -> ActorDefinition startup protocol exit
withSources sources definition = definition { internalSources = sources }

withLaunchWorktree
  :: Text
  -> ActorDefinition startup protocol exit
  -> ActorDefinition startup protocol exit
withLaunchWorktree treeId
  definition@ActorDefinitionInternal { internalLaunchWorktrees = worktrees } =
    definition { internalLaunchWorktrees = worktrees <> [treeId] }

-- | Internal unit-call boundary for protocols that can turn target lifecycle
-- failure into their own typed control flow.
tryCallUnit
  :: Member Actor effs
  => ActorRef protocol exit
  -> protocol ()
  -> Eff effs (Either Text ())
tryCallUnit (ActorRef actorId incarnation _) request = do
  status <- send (ActorTryCallWith (actorId, incarnation) request)
  pure $ case status of
    ActorCallSucceeded -> Right ()
    ActorCallFailed summary -> Left summary
