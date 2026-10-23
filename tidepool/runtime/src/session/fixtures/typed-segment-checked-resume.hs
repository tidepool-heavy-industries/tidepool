parkedValue <- do
  segmentRecord 1
  segmentRecord (-2147483647)
  segmentRecord 2
  pure (23 :: Int)
