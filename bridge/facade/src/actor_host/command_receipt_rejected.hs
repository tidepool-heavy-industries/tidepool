do
  print ("before invalid launch" :: Text)
  _ <- Cmd.run (Cmd.argv [])
  print ("unreachable" :: Text)
