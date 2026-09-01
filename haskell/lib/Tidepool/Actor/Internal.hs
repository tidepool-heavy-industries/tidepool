{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
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
  , ActorDefinition (..)
  ) where

import Control.Monad.Freer (Eff)
import Data.Kind (Type)
import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core (Actor, ActorLocal, ActorMcp, Deliberate, FsRead, FsWrite)
import Tidepool.Internal.ExitCell (ExitCell)

data ActorRef (protocol :: Type -> Type) exit where
  ActorRef :: Int -> Int -> ExitCell pending exit -> ActorRef protocol exit

-- | Experimental named profiles for resident Haskell effect rows. The witness
-- fixes the child row; Rust independently validates spawn attenuation and
-- installed interpreters authorize nominal requests. These profiles do not
-- sandbox native tools belonging to an attached coding-agent process.
data EffectProfile (protocol :: Type -> Type) effs where
  ReadOnly :: EffectProfile protocol (ReadOnlyEffects protocol)
  ReadWrite :: EffectProfile protocol (ReadWriteEffects protocol)

type ReadOnlyEffects protocol =
  '[ ActorLocal protocol
   , ActorMcp
   , Actor
   , Deliberate
   , FsRead
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
  ActorDefinition
    :: { label :: Text
       , effectProfile :: EffectProfile protocol actorEffs
       , initialization :: startup -> Eff actorEffs initial
       , behavior :: startup -> initial -> Eff actorEffs exit
       , visibleToChild :: [Text]
       , onShutdown :: ShutdownReason -> Eff actorEffs ()
       }
    -> ActorDefinition startup protocol exit
