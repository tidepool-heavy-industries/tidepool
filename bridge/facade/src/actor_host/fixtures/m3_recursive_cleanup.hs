Right () <- releaseCheckpoint rootSeed
stopped <- stopAgent recursiveChild
case stopped of
  StoppedNow -> display True
  AlreadyStopped -> display True
  _ -> display False
