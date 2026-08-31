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
  , ActorDefinition (..)
  ) where

import Control.Monad.Freer (Eff, Members)
import Data.Kind (Type)
import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core (ActorLocal, Deliberate)
import Tidepool.Internal.ExitCell (ExitCell)

data ActorRef (protocol :: Type -> Type) exit where
  ActorRef :: Int -> Int -> ExitCell pending exit -> ActorRef protocol exit

data ActorDefinition startup (protocol :: Type -> Type) exit where
  ActorDefinition
    :: Members '[Deliberate, ActorLocal api exit] actorEffs
    => Text
    -> (startup -> Eff actorEffs initial)
    -> (startup -> initial -> Eff actorEffs exit)
    -> ActorDefinition startup api exit
