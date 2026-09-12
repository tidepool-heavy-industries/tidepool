:{
guideWatchTypes :: Watch result -> EventWatch -> (Watch result, EventWatch)
guideWatchTypes readiness event = (readiness, event)
guideIsUnavailable :: WatchState result -> Bool
guideIsUnavailable WatchPending = False
guideIsUnavailable (WatchReady _) = False
guideIsUnavailable (WatchUnavailable _) = True
:}
let failureLabel = "guide-unavailable" :: Label
failureResponse <- request @Text (responseActor worker) (assignment failureLabel ("This request will be interrupted." :: Text))
let failureWatchLabel = "guide-unavailable-ready" :: WatchLabel
failureReady <- watch failureWatchLabel (awaitSettled failureResponse)
let outerFailureWatchLabel = "guide-outer-unavailable" :: WatchLabel
outerFailureReady <- watch outerFailureWatchLabel (awaitResponse failureResponse)
let retainedFailureReady = fst (guideWatchTypes failureReady (WatchDeadline 1))
stopAgent (responseActor worker)
