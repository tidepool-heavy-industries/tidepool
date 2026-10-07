guideWatchTypes :: Watch result -> EventWatch -> (Watch result, EventWatch)
guideWatchTypes readiness event = (readiness, event)
guideIsUnavailable :: WatchState result -> Bool
guideIsUnavailable (WatchPending _) = False
guideIsUnavailable (WatchReady _) = False
guideIsUnavailable (WatchUnavailable _) = True
failureResponse <- request @Text (responseActor worker) ("This request will be interrupted." :: Text)
  (defaultRequestOptions { requestLabel = Just "guide-unavailable" })
failureReady <- watch (Just "guide-unavailable-ready") (settlement failureResponse)
outerFailureReady <- watch (Just "guide-outer-unavailable") (result failureResponse)
let retainedFailureReady = fst (guideWatchTypes failureReady (WatchDeadline 1))
stopAgent (responseActor worker)
