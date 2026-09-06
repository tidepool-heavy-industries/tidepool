:{
guideWatchIdentity :: Watch result -> Watch result
guideWatchIdentity value = value
:}
let Right failureRequestLabel = requestLabel "guide-unavailable"
failureResponse <- request @Text (forkedActor worker) failureRequestLabel ("This request will be interrupted." :: Text)
let Right failureWatchLabel = watchLabel "guide-unavailable-ready"
failureReady <- watch failureWatchLabel (awaitSettled failureResponse)
let retainedFailureReady = guideWatchIdentity failureReady
stopAgent (forkedActor worker)
