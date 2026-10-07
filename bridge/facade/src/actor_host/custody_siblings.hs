{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]
Right firstAgent <- spawnSubagent (FreshCtx "Complete the first custody assignment.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workerSpec) { spawnLabel = Just "first", spawnLifetime = ActorOwned })
Right secondAgent <- spawnSubagent (FreshCtx "Complete the second custody assignment.") (ForkWorktree projectHead)
  ((defaultSpawnOptions workerSpec) { spawnLabel = Just "second", spawnLifetime = ActorOwned })
Right firstRequest <- request @Text firstAgent ("first" :: Text)
  (defaultRequestOptions { requestLabel = Just "first" })
Right secondRequest <- request @Text secondAgent ("second" :: Text)
  (defaultRequestOptions { requestLabel = Just "second" })
let siblings = (firstRequest, secondRequest)
