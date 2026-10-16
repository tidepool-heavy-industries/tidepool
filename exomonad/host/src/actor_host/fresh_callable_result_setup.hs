{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Actors.Exomonad

Right callableAgent <- spawnSubagent (FreshCtx "Return the supplied integer and an increment closure.")
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
    { spawnLabel = Just "fresh-callable-result", spawnLifetime = ActorOwned })
Right callableResponse <- request @(Int, Int -> Int) callableAgent (41 :: Int)
  (defaultRequestOptions { requestLabel = Just "fresh-callable-result" })
callableWatch <- watch (Just "fresh-callable-result") (response callableResponse)
