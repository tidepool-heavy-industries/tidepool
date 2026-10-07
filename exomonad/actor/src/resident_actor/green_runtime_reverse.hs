_ <- do
  (first, second) <- Async.concurrently
    (awaitGreenForm "one" (\x -> x + (11 :: Int)))
    (awaitGreenForm "two" (\x -> x * (7 :: Int)))
  say (tshow (first 5 == 16 && second 5 == 35))
