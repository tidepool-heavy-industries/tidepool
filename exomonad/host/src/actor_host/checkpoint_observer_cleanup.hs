do
  outcome <- stopAgent (responseActor observerRequest)
  case outcome of
    StoppedNow -> display True
    AlreadyStopped -> display True
    unexpected -> display unexpected
