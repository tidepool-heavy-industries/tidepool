{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

do
  Right seed <- checkpoint "eight measured captured actors"
  let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]
      labels = ["measure-a", "measure-b", "measure-c", "measure-d", "measure-e", "measure-f", "measure-g", "measure-h"]
      options label = (defaultSpawnOptions workerSpec)
        { spawnLabel = Just label, spawnLifetime = ActorOwned }
      input = "Run the bounded offline workload then respond 42." :: Text
  admissions <- mapM (\label -> spawnSubagent (ForkCtx seed) (ForkWorktree projectHead) (options label)) labels
  Right agents <- pure (sequence admissions)
  requests <- mapM (\(label, agent) -> request @Int agent input
    (defaultRequestOptions { requestLabel = Just label })) (zip labels agents)
  Right requests <- pure (sequence requests)
  replies <- watch (Just "eight measured replies") (sequenceA (map settlement requests))
  Right results <- await (observed replies)
  Right [42, 42, 42, 42, 42, 42, 42, 42] <- pure (sequence results)
  Right () <- releaseCheckpoint seed
  display True
