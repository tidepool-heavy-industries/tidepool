{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE TypeOperators #-}

-- Project-sized defaults for authored actor records. Runtime roles, scheduling
-- and resource authority remain with Shoal's existing owners.
module Project.Actors (CoordinationEffects, coordinationActor, releaseGroup) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Shoal
import Tidepool.Effects.Row (knownEffects)

type CoordinationEffects api = LocalEffects api '[Replies, Actor, Notifications]

coordinationActor
  :: Text
  -> api (Definition (Handler (ActorState api) (CoordinationEffects api)))
  -> ActorSpec api (CoordinationEffects api)
coordinationActor name = R.definition name (Actor.Selected knownEffects)

-- The cleanup owner checks exact revisions and admission while releasing.
-- The receipt retains active/uncertain members; this does not stop them first.
releaseGroup
  :: (Member AgentInspection effects, Member AgentControl effects)
  => ForkGroupHandle -> Eff effects CleanupReceipt
releaseGroup group = planCleanup group >>= executeCleanup
