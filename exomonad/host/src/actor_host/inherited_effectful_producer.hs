{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands)
import Tidepool.Actors.Exomonad

Right workerAgent <- spawnSubagent (FreshCtx "Return an effectful command closure.")
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, BoundWorktree]))
    { spawnLabel = Just "producer", spawnLifetime = ActorOwned })
Right worker <- request @(() -> Eff '[Replies, Commands, BoundWorktree] Cmd.Job) workerAgent ()
  (defaultRequestOptions { requestLabel = Just "producer" })
