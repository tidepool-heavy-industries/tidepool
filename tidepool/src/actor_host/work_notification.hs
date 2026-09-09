import qualified Tidepool.Actor as Actor
let Right requestName = requestLabel "router-notification"
worker <- startAgent (readonlyAgent "progress-source")
(response, progress) <- requestWithProgress @WorkProgress @Text worker (requestOptions requestName ("publish a decision" :: Text))
owner <- actorContext
let sources = [("source", response, progress)]
collector <- followWork sources (notifyWork owner (workMessage id))
