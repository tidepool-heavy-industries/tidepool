{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import Data.Text (Text)
import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Just seed <- R.call (readSeed (R.client seedStore)) ()
Right observer <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
    { spawnLabel = Just "observer", spawnLifetime = ActorOwned })
Right observerRequest <- request @Text observer ("inspect" :: Text)
  (defaultRequestOptions { requestLabel = Just "observer" })
