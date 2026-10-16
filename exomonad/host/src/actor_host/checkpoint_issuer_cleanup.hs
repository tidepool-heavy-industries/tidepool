do
  outcome <- stopAgent (responseActor producerRequest)
  display (case outcome of StoppedNow -> True; AlreadyStopped -> True; _ -> False)
