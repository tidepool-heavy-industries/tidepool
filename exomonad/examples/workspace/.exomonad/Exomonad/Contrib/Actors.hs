{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE TypeOperators #-}

-- Project-sized defaults for authored actor records. Scheduling and resource
-- authority remain with Exomonad's existing owners.
module Exomonad.Contrib.Actors (CoordinationEffects, coordinationActor) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Row (knownEffects)

type CoordinationEffects api = LocalEffects api '[Replies, Actor, Notifications]

coordinationActor
  :: Text
  -> api (Definition (Handler (ActorState api) (CoordinationEffects api)))
  -> ActorSpec api (CoordinationEffects api)
coordinationActor name = R.definition name (Actor.Selected knownEffects)
