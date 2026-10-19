{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands)
import Tidepool.Actors.Exomonad

Right childAgent <- spawnSubagent (FreshCtx "Run the descendants child fixture.")
  (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[AgentLaunch, SpawnAgent OpenAI, Replies, Commands, BoundWorktree]))
    { spawnPermissions = modelPermissions [allowSpawnAgent currentModel], spawnLabel = Just "descendants-child", spawnLifetime = ActorOwned })
Right childResponse <- request @Int childAgent (1 :: Int)
  (defaultRequestOptions { requestLabel = Just "descendants-child" })
