_ <- do
  first <- Async.async (awaitGreenForm "one" (\x -> x + (11 :: Int)))
  second <- Async.async (awaitGreenForm "two" (\x -> x * (7 :: Int)))
  pending <- Async.poll first
  chosen <- Async.wait second
  Async.cancel first
  cancelled <- Async.poll first
  completed <- Async.poll second
  abandoned <- Async.waitCatch first
  completedAgain <- Async.poll second
  let isCompleted state = case state of
        Async.ThreadCompleted value -> value 5 == 35
        _ -> False
  say (tshow ((case pending of
    Async.ThreadRunning -> True
    _ -> False) && (case cancelled of
    Async.ThreadCancelled -> True
    _ -> False) && isCompleted completed && isCompleted completedAgain
    && chosen 5 == 35 && case abandoned of
    Left Async.AsyncCancelled -> True
    Right _ -> False))
