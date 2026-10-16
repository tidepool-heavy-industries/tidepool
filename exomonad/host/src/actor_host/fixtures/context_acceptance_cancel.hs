import qualified Tidepool.Agent.Contract as A
do
  let trimResult body = if ContextText.isInfixOf "context-setup-retained-result" body
        then C.trimText "must not publish cancelled trim" "must-not-publish"
        else ContextText.replace "parent-original" "must-not-publish" body
  current <- C.getContext
  let patch = C.contextPatch
        [C.replaceText target updated | target <- C.textTargets current,
          let updated = trimResult (C.targetText target), updated /= C.targetText target]
  Right _ <- pure (C.previewContext current patch)
  C.ContextStaged _ <- C.stageContext patch
  C.setNextModel "must-not-publish-model"
  C.setNextEffort C.High
  Right captured <- checkpoint "failure snapshot"
  Right worker <- spawnSubagent (ForkCtx captured) SameDir
    ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
      { spawnLabel = Just "survives-parent-failure" })
  Right reply <- request @Text worker ("retained request" :: Text) defaultRequestOptions
  sleep (seconds 30)
  pure True
