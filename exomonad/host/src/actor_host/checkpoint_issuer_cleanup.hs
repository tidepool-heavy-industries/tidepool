do
  outcome <- stopAgent (responseActor producerRequest)
  case outcome of
    StoppedNow -> display True >> pure ()
    AlreadyStopped -> display True >> pure ()
    unexpected -> display (show unexpected) >> pure ()
