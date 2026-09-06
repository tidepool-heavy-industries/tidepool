:{
guideWatchTypes :: Watch result -> EventWatch -> (Watch result, EventWatch)
guideWatchTypes readiness event = (readiness, event)
:}
let Right failureRequestLabel = requestLabel "guide-unavailable"
failureResponse <- request @Text (forkedActor worker) failureRequestLabel ("This request will be interrupted." :: Text)
let Right failureWatchLabel = watchLabel "guide-unavailable-ready"
failureReady <- watch failureWatchLabel (awaitSettled failureResponse)
let retainedFailureReady = fst (guideWatchTypes failureReady (WatchDeadline 1))
stopAgent (forkedActor worker)
