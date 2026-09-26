let cases = case terminal of
      WorkFinished name (Right receipt) ->
        ( reviewReadiness (WorkFinished name (Right (receipt
            { responseValue = Produced (Candidate sourceHead [] []) })))
        , reviewReadiness (WorkFinished name (Right (receipt
            { responseWorktree = NoBoundWorktree })))
        , reviewReadiness (WorkFinished name (Right (receipt
            { responseValue = Blocked "stopped" [] })))
        )
      _ -> (Nothing, Nothing, Nothing)
inspectFull (show cases)
