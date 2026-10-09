Right () <- releaseCheckpoint childSeed
stopped <- stopAgent recursiveGrandchild
case stopped of
  StoppedNow -> display True
  AlreadyStopped -> display True
  _ -> display False
Right (Right answer) <- pure grandchildAnswer
respond answer
