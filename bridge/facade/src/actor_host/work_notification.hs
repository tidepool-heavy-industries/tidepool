import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
let requestName = [label|router-notification|]
worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "progress-source"))
(response, progress) <- do { issued <- requestWithProgress @WorkProgress @Text worker (assignment requestName ("publish a decision" :: Text)); Right () <- detachRequest (fst issued); pure issued }
let owner = me
let sources = [("source", response, progress)]
collector <- followWork sources (notifyWork owner (workMessage id))
