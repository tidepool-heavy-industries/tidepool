{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import Data.Text (Text)
import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands)
import Tidepool.Actors.Exomonad

Right seed <- checkpoint currentModel "checkpoint issuer context"
Right producer <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, BoundWorktree, AgentLaunch, ForkAgent OpenAI, Actor]))
    { spawnPermissions = modelPermissions [allowForkAgent currentModel], spawnLabel = Just "producer", spawnLifetime = ActorOwned })
Right producerRequest <- request @Text producer ("capture" :: Text)
  (defaultRequestOptions { requestLabel = Just "producer" })
