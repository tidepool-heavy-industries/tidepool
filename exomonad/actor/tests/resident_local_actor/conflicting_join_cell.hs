data OnlySIDE = OnlySIDE

instance JoinClass Bool where
  joinValue _ = RESULT_VALUE

onlySIDE <- pure (RESULT_VALUE :: Int)
joinedSIDE <- do
  job <- Cmd.background
    (Cmd.withArguments [pack "MARKER"] (Cmd.bashCommand (pack "true")))
  let label = Just (pack "command-finished")
  ready <- Watch.watch label (Cmd.awaitFinished job)
  result <- Watch.await (Watch.observed ready)
  case result of
    Left failure -> error (tshow failure)
    Right _ -> pure (RESULT_VALUE :: Int)
