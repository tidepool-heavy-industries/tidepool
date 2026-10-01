import qualified Data.Text as Text
interviewStates <- if null interviewDetachFailures
  then traverse pollResponse interviewRequests else pure []
let answered state = case state of
      ResponseReady receipt | not (Text.null (responseValue receipt)) -> Just receipt
      _ -> Nothing
let retainedInterviews = if null interviewDetachFailures
      then traverse answered interviewStates else Nothing
-- Save these exact answer receipts in the project interview record before
-- running review-flow-workflow-close. A delivered request is not an answer.
inspectFull (show (fmap (map responseValue) retainedInterviews))
