{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

capturedValue <- pure (x :: Int)
let capturedGetter = capturedValue + 1
Right seed <- checkpoint "same-cell captured context"
let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]
Right alpha <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions workerSpec) { spawnLabel = Just "captured-alpha", spawnLifetime = ActorOwned })
Right beta <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions workerSpec) { spawnLabel = Just "captured-beta", spawnLifetime = ActorOwned })
Right alphaRequest <- request @Int alpha ("read the captured getter" :: Text)
  (defaultRequestOptions { requestLabel = Just "captured-alpha" })
Right betaRequest <- request @Int beta ("read the captured getter" :: Text)
  (defaultRequestOptions { requestLabel = Just "captured-beta" })
R.send (storeAgents (R.client groupStore)) [alpha, beta]
replies <- watch (Just "same-cell-captured-replies")
  ((,) <$> settlement alphaRequest <*> settlement betaRequest)
result <- await (observed replies)
firstRelease <- releaseCheckpoint seed
secondRelease <- releaseCheckpoint seed
case (firstRelease, secondRelease, result) of
  (Right (), Right (), Right (Right 42, Right 42)) -> display True
  _ -> display False
