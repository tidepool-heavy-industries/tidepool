do
  _ <- Cmd.start [bash|printf started-once|]
  if error "continuation failed after admission" then pure () else pure ()
