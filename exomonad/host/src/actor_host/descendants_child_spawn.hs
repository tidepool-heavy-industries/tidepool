{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right grandchildAgent <- spawnSubagent (FreshCtx "Complete the grandchild assignment.")
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "descendants-grandchild", spawnLifetime = ActorOwned })
Right grandchildResponse <- request @Int grandchildAgent (2 :: Int)
  (defaultRequestOptions { requestLabel = Just "descendants-grandchild" })
