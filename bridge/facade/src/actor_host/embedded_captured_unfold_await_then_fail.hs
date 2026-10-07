{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

capturedValue <- pure (x :: Int)
privateCapturedHelper :: Int -> Int
privateCapturedHelper value = value + 1
let capturedGetter = privateCapturedHelper capturedValue
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
R.send (storeSeed (R.client seedStore)) seed
error "M2_INTENTIONAL_PARENT_EXECUTION_FAILURE"
