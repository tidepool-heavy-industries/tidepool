{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

Right workerAgent <- spawnSubagent (FreshCtx "Return an effectful command closure.")
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "producer", spawnLifetime = ActorOwned })
Right worker <- request @(() -> Eff '[Commands] Cmd.Job) workerAgent ()
  (defaultRequestOptions { requestLabel = Just "producer" })
