_ <- do
  _ <- Async.concurrently
    (pure (Core.error "controlled green infrastructure failure" :: Int) >>= \value -> say (tshow value))
    (awaitGreenForm "two" (7 :: Int))
  say "unexpected successful green failure"
