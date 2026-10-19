{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands)
import Tidepool.Actors.Exomonad

Right grandchildAgent <- spawnSubagent (FreshCtx "Complete the grandchild assignment.")
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, BoundWorktree]))
    { spawnLabel = Just "descendants-grandchild", spawnLifetime = ActorOwned })
Right grandchildResponse <- request @Int grandchildAgent (2 :: Int)
  (defaultRequestOptions { requestLabel = Just "descendants-grandchild" })
