{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right seed <- checkpoint currentModel "capture the producer request for its observer"
Right observerAgent <- spawnSubagent (ForkCtx seed) (ForkWorktree currentCheckout)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Watches, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "observer", spawnLifetime = ActorOwned })
Right observer <- request @(Request ((Text, [Int]), Int -> Int)) observerAgent ("observe the inherited request" :: Text)
  (defaultRequestOptions { requestLabel = Just "observer" })
