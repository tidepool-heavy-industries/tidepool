import qualified Tidepool.Agent.Contract as A
do
  let trimResult body = if ContextText.isInfixOf "context-setup-retained-result" body
        then C.trimText "must not publish failed trim" "must-not-publish"
        else ContextText.replace "parent-original" "must-not-publish" body
  staged <- C.modifyContext (over C.editableTexts trimResult)
  if any (ContextText.isInfixOf "must not publish failed trim") (toListOf C.editableTexts staged)
    then pure ()
    else error "native result trim was not staged" >> pure ()
  C.setNextModel "must-not-publish-model"
  C.setNextEffort C.High
  Right captured <- checkpoint "failure snapshot"
  Right worker <- spawnSubagent (ForkCtx captured) SameDir
    ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))
      { spawnLabel = Just "survives-parent-failure" })
  Right reply <- request @Text worker ("retained request" :: Text) defaultRequestOptions
  error "intentional context transaction failure" >> pure True
