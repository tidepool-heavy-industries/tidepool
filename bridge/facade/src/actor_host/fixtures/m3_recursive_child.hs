{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Lookup)
import Tidepool.Actors.Exomonad

inheritedAnswer <- recursiveHelper
Right childSeed <- checkpoint "recursive child native helper"
let leafSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]
let leafOptions = (defaultSpawnOptions leafSpec)
      { spawnModel = Just (Alias "luna"), spawnLifetime = ActorOwned, spawnLabel = Just "recursive-grandchild" }
Right recursiveGrandchild <- spawnSubagent (ForkCtx childSeed) (ForkWorktree currentCheckout) leafOptions
Right grandchildRequest <- request @Int recursiveGrandchild ("return the inherited native helper" :: Text) defaultRequestOptions
grandchildWatch <- watch (Just "recursive-grandchild-reply") (settlement grandchildRequest)
grandchildAnswer <- await (observed grandchildWatch)
case grandchildAnswer of
  Right (Right 42) -> display (inheritedAnswer == 42)
  _ -> display False
