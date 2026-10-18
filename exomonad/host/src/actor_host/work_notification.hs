import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
Right workerCapture <- checkpoint currentModel "typed worker fixture"
Right worker <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "progress-source" })
Right (response, progress) <- requestWithProgress @WorkProgress @Text worker ("publish a decision" :: Text) defaultRequestOptions
let owner = me
let sources = [("source", R.settlement response, R.progress progress)]
Right collector <- traverse R.start (workDefinition sources (notifyWork owner (workMessage id)))
