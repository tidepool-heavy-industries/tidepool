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
-- Keep the original responses and the caller-owned requests inspectable.
interviewRequests <- if terminal then traverse (\actor -> request @Text actor "Review the handoff"
  (defaultRequestOptions
    { requestLabel = Just "review-interview"
    , requestGuidance = Just "Before retirement, report what helped, what caused waits or confusion, and one concrete change to try next. Include exact source or calls where useful. Distinguish observation from inference." })) reviewerActors else pure []
inspectFull (show (flowStage terminalSnapshot, length interviewRequests,
  length (flowReviewRoutes terminalSnapshot)))
