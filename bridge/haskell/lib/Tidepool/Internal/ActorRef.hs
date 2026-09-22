{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}

-- | Engine-private representation of exact actor references.
--
-- This leaf module deliberately knows nothing about effect rows. Generated
-- kernel declarations can mention an exit-typed reference without importing
-- the higher-level actor facade back into the effect module that facade uses.
module Tidepool.Internal.ActorRef
  ( ActorRef (..)
  , ExitRef (..)
  , actorAddress
  ) where

import Data.Kind (Type)
import Prelude

import Tidepool.Internal.ExitCell (ExitCell)

data ActorRef (protocol :: Type -> Type) exit where
  ActorRef :: Int -> Int -> ExitCell pending exit -> ActorRef protocol exit

-- | An exact actor reference with its mailbox protocol hidden.
--
-- Worker custody needs only the actor's successful exit type. Hiding the
-- protocol avoids coupling the private worker ledger to a particular mailbox
-- API while retaining the live Haskell exit cell verbatim.
data ExitRef exit where
  ExitRef :: ActorRef protocol exit -> ExitRef exit

actorAddress :: ActorRef protocol exit -> (Int, Int)
actorAddress (ActorRef actorId incarnation _) = (actorId, incarnation)
