import qualified Tidepool.Agent.Contract as A
Right idleChild <- spawnSubagent (FreshCtx "fresh-idle-context-seed") SameDir
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
    { spawnLabel = Just "installed-idle-child", spawnModel = Just (Literal "test-model") })
display ("installed-idle" :: Text)
pure idleChild
