_ <- do
  _ <- Async.concurrently
    (awaitGreenForm "one" (11 :: Int))
    (awaitGreenForm "two" (7 :: Int))
  say "unexpected continuation after parent cancellation"
