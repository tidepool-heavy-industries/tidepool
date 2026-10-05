do
  settled <- pollResponse selectedWorker
  display (case settled of
    ResponseReady result -> case responseValue result of
      Types.Produced candidate ->
        Types.candidateCommit candidate == Types.taskSource selectedTask
          && Types.reportedChecks candidate == [Types.obligation selectedTask]
          && null (Types.remainingGates candidate)
      Types.Blocked _ _ -> False
    _ -> False)
