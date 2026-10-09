{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Console, Lookup)
import Tidepool.Actors.Exomonad

Right rootSeed <- checkpoint "recursive root native helper"
let recursiveSpec = A.defaultWorkbenchSpec @'[Replies, Watches, AgentLaunch, AgentControl, Commands, Console, Lookup, BoundWorktree]
let recursiveOptions = (defaultSpawnOptions recursiveSpec)
      { spawnModel = Just (Alias "luna"), spawnLifetime = ActorOwned, spawnLabel = Just "recursive-child" }
Right recursiveChild <- spawnSubagent (ForkCtx rootSeed) (ForkWorktree projectHead) recursiveOptions
Right childRequest <- request @Int recursiveChild ("delegate to a grandchild and return its inherited helper" :: Text) defaultRequestOptions
childWatch <- watch (Just "recursive-child-reply") (settlement childRequest)
childAnswer <- await (observed childWatch)
case childAnswer of
  Right (Right 42) -> display True
  _ -> display False
