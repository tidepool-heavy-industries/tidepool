:{
guideWatchTypes :: Watch result -> EventWatch -> (Watch result, EventWatch)
guideWatchTypes readiness event = (readiness, event)
guideIsUnavailable :: WatchState result -> Bool
guideIsUnavailable WatchPending = False
guideIsUnavailable (WatchReady _) = False
guideIsUnavailable (WatchUnavailable _) = True
:}
let Right failureRequestLabel = requestLabel "guide-unavailable"
failureResponse <- request @Text (forkedActor worker) failureRequestLabel ("This request will be interrupted." :: Text)
let Right failureWatchLabel = watchLabel "guide-unavailable-ready"
failureReady <- watch failureWatchLabel (awaitSettled failureResponse)
let Right outerFailureWatchLabel = watchLabel "guide-outer-unavailable"
outerFailureReady <- watch outerFailureWatchLabel (awaitResponse failureResponse)
let retainedFailureReady = fst (guideWatchTypes failureReady (WatchDeadline 1))
stopAgent (forkedActor worker)
