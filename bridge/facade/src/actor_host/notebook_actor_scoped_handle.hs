import qualified Data.Text as Text
import qualified Tidepool.Agent.Reply.Internal as Reply
import qualified Tidepool.Agent.Watch.Internal as Watch
import qualified Tidepool.Inspection as Inspection

let response = fst (Inspection.workbenchDisplay (Reply.ResponseUnavailable (Reply.ResponseRejected Reply.ReplyUnauthorized) :: Reply.ResponseState Text))
let watch = fst (Inspection.workbenchDisplay (Watch.WatchUnavailable (Watch.WatchRejected Reply.ReplyWrongIncarnation) :: Watch.WatchState Text))
let direct = fst (Inspection.displayWith 512 (Left Reply.ReplyUnauthorized :: Either Reply.ReplyError ()))
if all (Text.isInfixOf "actor-scoped") [response, direct]
    && all (Text.isInfixOf "your assignment can continue") [response, direct]
    && Text.isInfixOf "different actor incarnation" watch
  then ("actor-scoped handle guidance rendered" :: Text)
  else error "actor-scoped handle guidance missing"
