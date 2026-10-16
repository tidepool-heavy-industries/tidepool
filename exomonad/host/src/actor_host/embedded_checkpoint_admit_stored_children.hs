{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Console, Lookup)
import Tidepool.Actors.Exomonad

do
  Just seed <- R.call (readSeed (R.client seedStore)) ()
  let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Console, Lookup, BoundWorktree]
  Right alpha <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    ((defaultSpawnOptions workerSpec) { spawnLabel = Just "checkpoint-alpha", spawnLifetime = ActorOwned })
  Right beta <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    ((defaultSpawnOptions workerSpec) { spawnLabel = Just "checkpoint-beta", spawnLifetime = ActorOwned })
  Right alphaRequest <- request @Text alpha ("read the captured Haskell context" :: Text)
    (defaultRequestOptions { requestLabel = Just "checkpoint-alpha" })
  Right betaRequest <- request @Text beta ("read the captured Haskell context" :: Text)
    (defaultRequestOptions { requestLabel = Just "checkpoint-beta" })
  firstRelease <- releaseCheckpoint seed
  secondRelease <- releaseCheckpoint seed
  refusal <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    (defaultSpawnOptions workerSpec)
  case (firstRelease, secondRelease, refusal) of
    (Right (), Right (), Left (SpawnRefused _)) -> pure True
    _ -> error "checkpoint release/refusal contract failed" >> pure True
