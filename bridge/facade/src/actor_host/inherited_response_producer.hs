{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right workerAgent <- spawnSubagent (FreshCtx "Return the requested value and increment function.")
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "worker", spawnLifetime = ActorOwned })
Right worker <- request @((Text, [Int]), Int -> Int) workerAgent ("custody" :: Text)
  (defaultRequestOptions { requestLabel = Just "worker" })
