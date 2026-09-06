let Right failureRequestLabel = requestLabel "guide-unavailable"
failureResponse <- request @Text (forkedActor worker) failureRequestLabel ("This request will be interrupted." :: Text)
let Right failureWatchLabel = watchLabel "guide-unavailable-ready"
failureReady <- watch failureWatchLabel (awaitSettled failureResponse)
stopAgent (forkedActor worker)
