{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Console, Lookup)
import Tidepool.Actors.Exomonad

capturedValue <- pure (x :: Int)
let capturedGetter = capturedValue + 1
Right seed <- checkpoint "same-cell captured context"
let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Console, Lookup, BoundWorktree, Actor]
Right alpha <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions workerSpec) { spawnLabel = Just "captured-alpha", spawnLifetime = InvocationOwned })
Right beta <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions workerSpec) { spawnLabel = Just "captured-beta", spawnLifetime = InvocationOwned })
Right alphaRequest <- request @Int alpha ("reply with the captured getter" :: Text)
  (defaultRequestOptions { requestLabel = Just "captured-alpha" })
Right betaRequest <- request @Int beta ("reply with the captured getter" :: Text)
  (defaultRequestOptions { requestLabel = Just "captured-beta" })
R.send (storeAgents (R.client groupStore)) [alpha, beta]
firstRelease <- releaseCheckpoint seed
secondRelease <- releaseCheckpoint seed
case (firstRelease, secondRelease) of
  (Right (), Right ()) -> pure ()
  _ -> error "HOSTED_SHUTDOWN_PARENT_CAPTURE_RELEASE_FAILED"
-- Retain both independent settlements. Failure of one child must not finish
-- the parent while its sibling still waits on the owned shutdown gate.
replies <- watch (Just "same-cell-captured-replies")
  ((,) <$> settlement alphaRequest <*> settlement betaRequest)
_ <- await (observed replies)
error "HOSTED_SHUTDOWN_PARENT_JOIN_SETTLED_BEFORE_NATIVE_CANCELLATION" >> display True
