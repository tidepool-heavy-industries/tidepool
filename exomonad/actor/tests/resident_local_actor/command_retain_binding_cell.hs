do
  job <- Cmd.start
    (Cmd.withArguments [pack "1"] (Cmd.bashCommand (pack "sleep \"$1\"")))
  binding <- Cmd.retainJobBinding job
  _ <- Cmd.await job
  pure binding
