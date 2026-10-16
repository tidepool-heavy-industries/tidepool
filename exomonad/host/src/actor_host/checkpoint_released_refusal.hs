{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

Just seed <- R.call (readSeed (R.client seedStore)) ()
refusal <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)
  (defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
case refusal of
  Left (SpawnRefused _) -> display True
  _ -> display False
