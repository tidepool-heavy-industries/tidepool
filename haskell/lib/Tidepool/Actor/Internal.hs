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
  , visibleToChild
  , onShutdown
  , actorLaunchWorktrees
  , withLaunchWorktree
  ) where

import Control.Monad.Freer (Eff)
import Data.Kind (Type)
import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core (Actor, ActorLocal, ActorMcp, AgentSession, Deliberate, FsRead, FsWrite, WorkerKernel, Worktree)
import Tidepool.Internal.ExitCell (ExitCell)

data ActorRef (protocol :: Type -> Type) exit where
  ActorRef :: Int -> Int -> ExitCell pending exit -> ActorRef protocol exit

-- | Experimental named profiles for resident Haskell effect rows. The witness
-- fixes the child row, while Rust independently validates spawn attenuation
-- and principal- or grant-sensitive operations. These profiles do not
-- sandbox native tools belonging to an attached coding-agent process.
data EffectProfile (protocol :: Type -> Type) effs where
  ReadOnly :: EffectProfile protocol (ReadOnlyEffects protocol)
  ReadWrite :: EffectProfile protocol (ReadWriteEffects protocol)

type ReadOnlyEffects protocol =
  '[ ActorLocal protocol
   , ActorMcp
   , AgentSession
   , Actor
   , Deliberate
   , FsRead
   , Worktree
   , WorkerKernel
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
    :: { internalLabel :: Text
       , internalEffectProfile :: EffectProfile protocol actorEffs
       , internalInitialization :: startup -> Eff actorEffs initial
       , internalBehavior :: startup -> initial -> Eff actorEffs exit
       , internalVisibleToChild :: [Text]
       , internalOnShutdown :: ShutdownReason -> Eff actorEffs ()
       , internalLaunchWorktrees :: [Text]
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
  -> [Text]
  -> (ShutdownReason -> Eff actorEffs ())
  -> ActorDefinition startup protocol exit
pattern ActorDefinition
  { label
  , effectProfile
  , initialization
  , behavior
  , visibleToChild
  , onShutdown
  } <- ActorDefinitionInternal
    label effectProfile initialization behavior visibleToChild onShutdown _
  where
    ActorDefinition label effectProfile initialization behavior visibleToChild onShutdown =
      ActorDefinitionInternal
        label effectProfile initialization behavior visibleToChild onShutdown []

{-# COMPLETE ActorDefinition #-}

actorLaunchWorktrees :: ActorDefinition startup protocol exit -> [Text]
actorLaunchWorktrees
  (ActorDefinitionInternal _ _ _ _ _ _ worktrees) = worktrees

withLaunchWorktree
  :: Text
  -> ActorDefinition startup protocol exit
  -> ActorDefinition startup protocol exit
withLaunchWorktree treeId
  (ActorDefinitionInternal l p initialize install exports shutdown worktrees) =
    ActorDefinitionInternal
      l p initialize install exports shutdown (worktrees <> [treeId])
