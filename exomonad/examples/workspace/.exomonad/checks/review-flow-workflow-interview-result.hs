import qualified Data.Text as Text
interviewStates <- traverse pollResponse interviewRequests
let answered state = case state of
      ResponseReady receipt | not (Text.null (responseValue receipt)) -> Just receipt
      _ -> Nothing
let retainedInterviews = traverse answered interviewStates
-- Save these exact answer receipts in the project interview record before
-- running review-flow-workflow-close. A delivered request is not an answer.
inspectFull (show (fmap (map responseValue) retainedInterviews))
