{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import Data.Text (Text)
import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

let x = 41 :: Int
let getX = x + 1
Right seed <- checkpoint "checkpoint issuer context"
Right producer <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "producer", spawnLifetime = ActorOwned })
Right producerRequest <- request @Text producer ("capture" :: Text)
  (defaultRequestOptions { requestLabel = Just "producer" })
