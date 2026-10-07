_ <- do
  first <- Async.async (awaitGreenForm "one" (\x -> x + (11 :: Int)))
  second <- Async.async (awaitGreenForm "two" (\x -> x * (7 :: Int)))
  chosen <- Async.wait second
  Async.cancel first
  abandoned <- Async.waitCatch first
  say (tshow (chosen 5 == 35 && case abandoned of
    Left Async.AsyncCancelled -> True
    Right _ -> False))
