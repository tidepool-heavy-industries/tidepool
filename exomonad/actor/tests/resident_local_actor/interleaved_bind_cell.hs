a <- do
  job <- Cmd.background
    (Cmd.withArguments [pack "1"] (Cmd.bashCommand (pack "sleep \"$1\"")))
  let label = either (error . tshow) id (Watch.watchLabel (pack "command-finished"))
  ready <- Watch.watch label (Cmd.awaitFinished job)
  result <- Watch.awaitWatch ready
  case result of
    Left failure -> error (tshow failure)
    Right _ -> pure (2 :: Int)
