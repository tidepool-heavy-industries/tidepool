{-# LANGUAGE GADTs #-}

-- | Engine-private representation of typed actor references.
--
-- Rust owns the exact routing identity. Haskell owns the successful exit
-- value through the shared managed cell, so copying a reference never creates
-- another runtime root and a closure-valued exit survives actor teardown.
module Tidepool.Actor.Internal
  ( AgentRef (..)
  , newAgentRef
  , completeAgentRef
  ) where

import Prelude

import Tidepool.Internal.ExitCell (ExitCell, fillExitCell, newExitCell)

data AgentRef api exit where
  AgentRef :: Int -> Int -> ExitCell pending exit -> AgentRef api exit

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
