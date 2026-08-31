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
  ( AgentRef (..)
  , ActorSpec (..)
  , newAgentRef
  , completeAgentRef
  ) where

import Control.Monad.Freer (Eff, Members)
import Data.Kind (Type)
import Data.Text (Text)
import Prelude

import Tidepool.Effects.Core (ActorLocal, Deliberate)
import Tidepool.Internal.ExitCell (ExitCell, fillExitCell, newExitCell)

data AgentRef (api :: Type -> Type) exit where
  AgentRef :: Int -> Int -> ExitCell pending exit -> AgentRef api exit

-- | A deployable Haskell actor. The startup result and concrete child row are
-- existential implementation details; callers see only the startup,
-- protocol, and successful-exit contract.
data ActorSpec startup (api :: Type -> Type) exit where
  ActorSpec
    :: Members '[Deliberate, ActorLocal api exit] actorEffs
    => Text
    -> (startup -> Eff actorEffs initial)
    -> (startup -> initial -> Eff actorEffs exit)
    -> ActorSpec startup api exit

-- | Temporary constructor for the typed-wait integration proof. Real startup
-- must allocate the cell before it suspends, capture that same cell in the
-- child's entry closure, and attach the returned routing identity afterward.
-- Remove this helper when that operation lands.
{-# NOINLINE newAgentRef #-}
newAgentRef :: Int -> Int -> pending -> AgentRef api exit
newAgentRef actorId incarnation pending =
  AgentRef actorId incarnation (newExitCell pending)

-- | Publish the successful exit before Rust makes completion observable.
completeAgentRef :: AgentRef api exit -> exit -> ()
completeAgentRef (AgentRef _ _ cell) = fillExitCell cell
