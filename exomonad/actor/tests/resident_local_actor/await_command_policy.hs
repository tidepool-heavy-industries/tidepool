serveToolsWith 0 $ \_ ->
  ResidentTools
    { waitForCommand =
        tool "Wait for a command and retain its output presentation." $ \request -> do
          job <- Cmd.start
            (Cmd.withArguments
              [tshow request.delay]
              (Cmd.bashCommand (pack "sleep \"$1\"")))
          _ <- Cmd.await job
          pure (WaitOutput True)
    }
