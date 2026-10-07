import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
Right workerCapture <- checkpoint "typed worker fixture"
Right worker <- spawnSubagent (ForkCtx workerCapture) SameDir ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "progress-source" })
Right (response, progress) <- requestWithProgress @WorkProgress @Text worker ("publish a decision" :: Text) defaultRequestOptions
let owner = me
let sources = [("source", response, progress)]
Right collector <- followWork sources (notifyWork owner (workMessage id))
