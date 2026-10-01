{-# LANGUAGE QuasiQuotes #-}
import Tidepool.Agent.Ref (agentIdentity)
terminalSnapshot <- R.call (reviewSnapshot (R.client flow)) ()
let terminal = case flowStage terminalSnapshot of
      ReviewAccepted _ -> True
      ReviewStopped _ -> True
      _ -> False
let reviewerActors = nubBy (\left right -> agentIdentity left == agentIdentity right)
      (map responseActor (flowReviewerRequests terminalSnapshot))
-- Ask each retained reviewer once, including reviewers from earlier repair rounds.
-- Keep the original responses and their ordered retention receipts inspectable.
interviewRequests <- if terminal then traverse (\actor -> request @Text @Text actor
  ((assignment [label|review-interview|] ("Review the handoff" :: Text))
    { guidance = Just "Before retirement, report what helped, what caused waits or confusion, and one concrete change to try next. Include exact source or calls where useful. Distinguish observation from inference."
    , report = Silent })) reviewerActors else pure []
interviewRetention <- traverse detachRequest interviewRequests
let interviewDetachFailures = [issue | Left issue <- interviewRetention]
if null interviewDetachFailures
  then inspectFull (show (flowStage terminalSnapshot, length interviewRequests,
    length (flowReviewRoutes terminalSnapshot)))
  else inspectFull (show (Left interviewDetachFailures :: Either [ReplyError] ()))
