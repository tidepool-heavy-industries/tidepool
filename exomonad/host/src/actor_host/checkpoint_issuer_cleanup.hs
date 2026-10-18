do
  outcome <- stopAgent (responseActor producerRequest)
  case outcome of
    StoppedNow -> display True
    AlreadyStopped -> display True
    unexpected -> display unexpected
