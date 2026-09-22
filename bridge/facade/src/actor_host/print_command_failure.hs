do
  print ("printed-before" :: Text)
  _ <- Cmd.await job
  case (Left ("printed-after" :: Text) :: Either Text ()) of
    Left issue -> print issue
    Right () -> pure ()
  if error "failure-after-print" then pure () else pure ()
