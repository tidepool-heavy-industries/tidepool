serveToolsWith 0 $ \_ ->
  ResidentTools
    { waitForCommand =
        tool "Wait for a background command through a typed watch." $ \request -> do
          job <- Cmd.background
            (Cmd.withArguments
              [tshow request.delay]
              (Cmd.bashCommand (pack "sleep \"$1\"")))
          let label = Just (pack "command-finished")
          ready <- Watch.watch label (Cmd.awaitFinished job)
          result <- Watch.await (Watch.observed ready)
          case result of
            Left failure -> error (tshow failure)
            Right _ -> pure (WaitOutput True)
    }
