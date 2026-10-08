_ <- do
  value <- awaitGreenForm "single" (\x -> x + (11 :: Int))
  say (tshow (value 5 == 16))
