import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
let requestName = [label|router-notification|]
worker <- startAgent (readonlyAgent "progress-source")
(response, progress) <- requestWithProgress @WorkProgress @Text worker (assignment requestName ("publish a decision" :: Text))
let owner = me
let sources = [("source", response, progress)]
collector <- followWork sources (notifyWork owner (workMessage id))
