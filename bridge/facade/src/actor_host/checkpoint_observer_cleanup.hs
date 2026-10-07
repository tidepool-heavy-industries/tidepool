do
  outcome <- stopAgent (responseActor observerRequest)
  display (case outcome of StoppedNow -> True; AlreadyStopped -> True; _ -> False)
