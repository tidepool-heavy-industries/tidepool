{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right seed <- checkpoint "capture the effectful request for its observer"
Right observerAgent <- spawnSubagent (ForkCtx seed) (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "observer", spawnLifetime = ActorOwned })
Right observer <- request @Text observerAgent ("observe and invoke the inherited command closure" :: Text)
  (defaultRequestOptions { requestLabel = Just "observer" })
