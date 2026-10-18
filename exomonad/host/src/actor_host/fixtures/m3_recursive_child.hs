{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified Tidepool.Agent.Contract as A
import Tidepool.Effects.Core (Commands, Console, Lookup)
import Tidepool.Actors.Exomonad
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options

data RecursiveChildInput = RecursiveChildInput Int
inheritedAnswer <- recursiveHelper
Right childSeed <- checkpoint currentModel "recursive child native helper"
let leafSpec = A.defaultWorkbenchSpec @'[Replies, Commands, Console, Lookup, BoundWorktree]
let leafOptions = (defaultSpawnOptions currentModel leafSpec)
      { spawnModel = fixtureOpenAI "luna" (Selection.OpenAIOptions Selection.OpenAILow), spawnLifetime = ActorOwned, spawnLabel = Just "recursive-grandchild" }
Right recursiveGrandchild <- spawnSubagent (ForkCtx childSeed) (ForkWorktree currentCheckout) leafOptions
Right grandchildRequest <- request @Int recursiveGrandchild ((sessionInput :: RecursiveRootInput), RecursiveChildInput 22) defaultRequestOptions
grandchildWatch <- watch (Just "recursive-grandchild-reply") (settlement grandchildRequest)
grandchildAnswer <- await (observed grandchildWatch)
case grandchildAnswer of
  Right (Right 42) -> display (inheritedAnswer == 42)
  _ -> display False
