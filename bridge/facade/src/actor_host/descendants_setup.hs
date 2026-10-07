{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right childAgent <- spawnSubagent (FreshCtx "Run the descendants child fixture.")
  (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[AgentLaunch, Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "descendants-child", spawnLifetime = ActorOwned })
Right childResponse <- request @Int childAgent (1 :: Int)
  (defaultRequestOptions { requestLabel = Just "descendants-child" })
