{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

data M2JoinA = M2JoinA Int deriving (Show, Eq)
m2JoinA <- do
  Right seed <- checkpoint "original nominal join inputs"
  let workerSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]
  Right alpha <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    ((defaultSpawnOptions workerSpec) { spawnLabel = Just "original-alpha", spawnLifetime = ActorOwned })
  Right beta <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
    ((defaultSpawnOptions workerSpec) { spawnLabel = Just "original-beta", spawnLifetime = ActorOwned })
  Right alphaRequest <- request @M2Reply alpha m2OriginalInput
    (defaultRequestOptions { requestLabel = Just "original-alpha" })
  Right betaRequest <- request @M2Reply beta m2OriginalInput
    (defaultRequestOptions { requestLabel = Just "original-beta" })
  R.send (storeAgents (R.client groupStore)) [alpha, beta]
  replies <- watch (Just "original-nominal-replies")
    ((,) <$> settlement alphaRequest <*> settlement betaRequest)
  result <- await (observed replies)
  Right () <- releaseCheckpoint seed
  case result of
    Right (Right (M2Reply 43), Right (M2Reply 43)) -> pure (M2JoinA 43)
    Left failure -> error ("original nominal reply watch failed: " <> T.pack (show failure))
    Right values -> error ("original nominal replies changed during same-root publication: " <> T.pack (show values))
display True
