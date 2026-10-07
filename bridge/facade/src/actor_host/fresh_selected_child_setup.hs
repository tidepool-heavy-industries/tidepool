{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

Right freshAgent <- spawnSubagent (FreshCtx "Render the supplied parent input as a fresh task.")
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Console]))
    { spawnLabel = Just "fresh-selected-child", spawnLifetime = ActorOwned })
Right freshSelectedChild <- request @FreshParentReply freshAgent (FreshParentInput 41)
  (defaultRequestOptions { requestLabel = Just "fresh-selected-child" })
