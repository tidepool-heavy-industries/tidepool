import qualified Tidepool.Actor as Actor
let requestName = "router-notification" :: Label
worker <- startAgent (readonlyAgent "progress-source")
(response, progress) <- requestWithProgress @WorkProgress @Text worker (assignment requestName ("publish a decision" :: Text))
owner <- actorContext
let sources = [("source", response, progress)]
collector <- followWork sources (notifyWork owner (workMessage id))
