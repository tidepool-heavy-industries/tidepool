{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right workerAgent <- spawnSubagent (FreshCtx "Complete the custody test assignment.")
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "worker", spawnLifetime = ActorOwned })
Right worker <- request @Text workerAgent ("custody" :: Text)
  (defaultRequestOptions { requestLabel = Just "worker" })
