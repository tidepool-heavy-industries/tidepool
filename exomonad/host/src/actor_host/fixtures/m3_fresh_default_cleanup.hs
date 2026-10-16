stopped <- stopAgent freshDefaultChild
case stopped of
  StoppedNow -> display True
  AlreadyStopped -> display True
  _ -> display False
