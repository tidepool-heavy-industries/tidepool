{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified AgentSpec as Installed
import qualified Project.Tools as Tools
import Project.Work (WorkspaceEffects)
import Tidepool.Actors.Exomonad
import qualified Exomonad.Models as Models
import qualified Tidepool.Model.Selection as Selection

let fixtureOpenAI key options = Selection.openAI Models.catalogIdentity key options

let freshDefaultOptions :: SpawnOptions CurrentModelFamily (Tools.WorkspaceTools WorkspaceEffects) WorkspaceEffects
    freshDefaultOptions = (defaultSpawnOptions currentModel Installed.agentSpec)
      { spawnModel = fixtureOpenAI "luna" (Selection.OpenAIOptions Selection.OpenAILow), spawnLifetime = ActorOwned
      , spawnInstructions = Just "Execute the shell, then settle the Text request with respond."
      }
Right freshDefaultChild <- spawnSubagent (FreshCtx "Return a pure typed Text response after the real command.")
  (ForkWorktree projectHead) freshDefaultOptions
Right freshDefaultRequest <- request @Text freshDefaultChild ("return fresh-default-native-reply" :: Text) defaultRequestOptions
freshDefaultAnswer <- await (result freshDefaultRequest)
case freshDefaultAnswer of
  Right answer -> display (answer == "fresh-default-native-reply")
  Left _ -> display False
