{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

import qualified AgentSpec as Installed
import qualified Project.Tools as Tools
import Project.Work (WorkspaceEffects)
import Tidepool.Actors.Exomonad

let offlineWorkerOptions :: Text -> SpawnOptions (Tools.WorkspaceTools WorkspaceEffects) WorkspaceEffects
    offlineWorkerOptions marker = (defaultSpawnOptions Installed.agentSpec)
      { spawnLifetime = ActorOwned
      , spawnLabel = Just marker
      , spawnInstructions = Just "Use the installed tools for the assigned file, then respond to the typed request."
      }
Right offlineEditor <- spawnSubagent (FreshCtx "deepseek-offline-worker-editor") SameDir
  ((offlineWorkerOptions "offline-editor")
    { spawnModel = Just DeepSeekPro
    , spawnReasoning = Just (DeepSeekReasoning DeepSeekHigh)
    })
Right offlineTester <- spawnSubagent (FreshCtx "deepseek-offline-worker-tester") SameDir
  (offlineWorkerOptions "offline-tester")
Right offlineEditRequest <- request @Text offlineEditor
  ("deepseek-offline-worker-editor: repair score.rs; respond edited" :: Text) defaultRequestOptions
Right offlineTestRequest <- request @Text offlineTester
  ("deepseek-offline-worker-tester: author score_tests.rs; respond tests-written" :: Text) defaultRequestOptions
display True
