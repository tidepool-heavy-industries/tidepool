{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right observerAgent <- spawnSubagent (FreshCtx "Observe the supplied pending request handle") (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Watches, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "observer", spawnLifetime = ActorOwned })
Right observer <- request @(Request ((Text, [Int]), Int -> Int)) observerAgent worker
  (defaultRequestOptions { requestLabel = Just "observer" })
