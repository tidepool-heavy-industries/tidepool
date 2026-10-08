{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

capturedValue <- retainedAction
let capturedGetter = capturedValue + 1
Right seed <- checkpoint "same-cell three-actor capture"
let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]
let workerOptions = (defaultSpawnOptions workerSpec)
      { spawnModel = Just (Alias "luna"), spawnLifetime = ActorOwned }
Right alpha <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  (workerOptions { spawnLabel = Just "three-actor-alpha" })
Right beta <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  (workerOptions { spawnLabel = Just "three-actor-beta" })
Right alphaRequest <- request @Int alpha ("read the captured getter" :: Text)
  (defaultRequestOptions { requestLabel = Just "three-actor-alpha" })
Right betaRequest <- request @Int beta ("read the captured getter" :: Text)
  (defaultRequestOptions { requestLabel = Just "three-actor-beta" })
replies <- watch (Just "three-actor-replies")
  ((,) <$> settlement alphaRequest <*> settlement betaRequest)
result <- await (observed replies)
case result of
  Right (Right 42, Right 42) -> display True
  _ -> display False
