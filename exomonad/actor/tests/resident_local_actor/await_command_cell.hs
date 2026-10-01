do
  job <- Cmd.start
    (Cmd.withArguments [pack "5"] (Cmd.bashCommand (pack "sleep \"$1\"")))
  Cmd.detach job
  _ <- OBSERVATION
  pure True
