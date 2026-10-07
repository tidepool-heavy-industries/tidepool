_ <- do
  _ <- Async.concurrently
    (awaitGreenForm "one" () >> say (tshow (Core.error "controlled green infrastructure failure" :: Int)))
    (awaitGreenForm "two" (7 :: Int))
  say "unexpected successful green failure"
