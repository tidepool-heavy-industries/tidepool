do
  outcome <- stopAgent (responseActor producerRequest)
  display (case outcome of StoppedRetaining _ -> True; _ -> False)
