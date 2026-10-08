_ <- stopAgent worker
Right retainedFailure <- await (settlement interrupted)
outerFailure <- await (result interrupted)
Right retainedSuccess <- await (result pending)
let failurePreserved = case retainedFailure of
      Left (ResponseTargetCancelled _) -> True
      _ -> False
    observationFailed = case outerFailure of
      Left (AwaitDependencyUnavailable _ (ResponseTargetCancelled _)) -> True
      _ -> False
display (failurePreserved, observationFailed, retainedSuccess == input)
