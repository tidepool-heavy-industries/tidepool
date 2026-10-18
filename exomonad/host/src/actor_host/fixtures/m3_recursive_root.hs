{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

import qualified Tidepool.Agent.Contract as A
import qualified AgentSpec as Installed
import qualified Project.Tools as Tools
import Project.Work (WorkspaceEffects)
import Tidepool.Actors.Exomonad
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options

Right rootSeed <- checkpoint currentModel "recursive root native helper"
let recursiveSpec = Installed.agentSpec :: A.AgentSpec (Tools.WorkspaceTools (ForkAgent OpenAI ': WorkspaceEffects)) (ForkAgent OpenAI ': WorkspaceEffects)
let recursiveOptions = (defaultSpawnOptions currentModel recursiveSpec)
      { spawnPermissions = modelPermissions [allowForkAgent (fixtureOpenAI "luna" (Selection.OpenAIOptions Selection.OpenAILow))], spawnModel = fixtureOpenAI "luna" (Selection.OpenAIOptions Selection.OpenAILow), spawnLifetime = ActorOwned, spawnLabel = Just "recursive-child" }
Right recursiveChild <- spawnSubagent (ForkCtx rootSeed) (ForkWorktree projectHead) recursiveOptions
Right childRequest <- request @Int recursiveChild (RecursiveRootInput 20) defaultRequestOptions
childWatch <- watch (Just "recursive-child-reply") (settlement childRequest)
childAnswer <- await (observed childWatch)
case childAnswer of
  Right (Right 42) -> display True
  _ -> display False
