{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE RoleAnnotations #-}

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


type role ActorRef nominal nominal
data ActorRef (protocol :: Type -> Type) (exit :: Type) = ActorRef Int Int

-- | An exact actor reference with its mailbox protocol hidden.
--
-- A worker's owned handle needs only the actor's successful exit type. Hiding the
-- protocol avoids coupling the private worker ledger to a particular mailbox
-- API while preserving its nominal exit type.
data ExitRef exit where
  ExitRef :: ActorRef protocol exit -> ExitRef exit

actorAddress :: ActorRef protocol exit -> (Int, Int)
actorAddress (ActorRef actorId incarnation) = (actorId, incarnation)
