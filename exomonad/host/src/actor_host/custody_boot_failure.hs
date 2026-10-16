{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

let brokenSpec = (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree])
      { A.specTools = error "intentional Haskell installer failure after custody installation" }
Right workerAgent <- spawnSubagent (FreshCtx "Run the failing native installer.")
  (ForkWorktree projectHead)
  ((defaultSpawnOptions brokenSpec)
    { spawnLabel = Just "worker", spawnLifetime = ActorOwned })
