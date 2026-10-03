serveToolsWith 0 $ \_ ->
  ResidentTools
    { waitForCommand =
        presentWith (\_ -> pack (replicate 200000 'x')) $
          tool "Wait for a command, then exceed the presenter observation budget." $ \request -> do
            job <- Cmd.start
              (Cmd.withArguments
                [tshow request.delay]
                (Cmd.bashCommand (pack "sleep \"$1\"")))
            _ <- Cmd.retainJobBinding job
            _ <- Cmd.await job
            pure (WaitOutput True)
    }
