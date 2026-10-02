runtimePrefix <- pure (51 :: Int)
failedRun <- if error "native failure after completed prefix" then pure (0 :: Int) else pure (0 :: Int)
runtimeTail <- pure (52 :: Int)
