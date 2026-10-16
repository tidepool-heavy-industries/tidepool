{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

do
  Right seed <- checkpoint "embedded parent checkpoint before later failure"
  let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]
  Right alpha <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    ((defaultSpawnOptions workerSpec) { spawnLabel = Just "checkpoint-alpha", spawnLifetime = ActorOwned })
  Right beta <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    ((defaultSpawnOptions workerSpec) { spawnLabel = Just "checkpoint-beta", spawnLifetime = ActorOwned })
  Right alphaRequest <- request @Text alpha ("read the captured Haskell context" :: Text)
    (defaultRequestOptions { requestLabel = Just "checkpoint-alpha" })
  Right betaRequest <- request @Text beta ("read the captured Haskell context" :: Text)
    (defaultRequestOptions { requestLabel = Just "checkpoint-beta" })
  R.send (storeAgents (R.client groupStore)) [alpha, beta]
  replies <- watch (Just "later-failure-captured-replies")
    ((,) <$> settlement alphaRequest <*> settlement betaRequest)
  await (observed replies)
  pure True
