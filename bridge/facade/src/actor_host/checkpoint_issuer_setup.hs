{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import Data.Text (Text)
import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right seed <- checkpoint "checkpoint issuer context"
Right producer <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree, AgentLaunch, Actor]))
    { spawnLabel = Just "producer", spawnLifetime = ActorOwned })
Right producerRequest <- request @Text producer ("capture" :: Text)
  (defaultRequestOptions { requestLabel = Just "producer" })
