do
  job <- Cmd.start
    (Cmd.withArguments [pack "1"] (Cmd.bashCommand (pack "sleep \"$1\"")))
  _ <- Cmd.retainJobBinding job
  _ <- Cmd.await job
  pure ()
