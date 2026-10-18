import qualified Tidepool.Agent.Contract as A
Right idleChild <- spawnSubagent (FreshCtx "fresh-idle-context-seed") SameDir
  ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies]))
    { spawnLabel = Just "installed-idle-child" })
display ("installed-idle" :: Text)
pure idleChild
