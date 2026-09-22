do
  _ <- Cmd.await job
  if error "failure-after-command" then pure () else pure ()
