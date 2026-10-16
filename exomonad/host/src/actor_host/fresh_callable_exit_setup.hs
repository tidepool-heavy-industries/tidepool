{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import Tidepool.Actor
import Tidepool.Effects.Row (KnownEffects (knownEffects))

let callableExitDefinition = stateful "fresh-callable-exit" (Selected (knownEffects @'[]))
      (\state (_, reply) -> pure (reply, state))
      :: ActorDefinition (Int, Int -> Int) ((,) ()) (Int, Int -> Int)
callableExitActor <- startActor callableExitDefinition (41, (\n -> n + 41))
