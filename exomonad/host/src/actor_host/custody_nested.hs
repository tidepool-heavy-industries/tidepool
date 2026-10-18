{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Right seed <- checkpoint currentModel "nested custody context"
Right leafAgent <- spawnSubagent (ForkCtx seed)
  (ForkWorktree currentCheckout)
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "leaf", spawnLifetime = ActorOwned })
Right nested <- request @Text leafAgent ("custody-leaf-reply" :: Text)
  (defaultRequestOptions { requestLabel = Just "leaf" })
