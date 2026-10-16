do
  result <- Cmd.await job
  print result
  if error "failure-after-command" then pure () else pure ()
