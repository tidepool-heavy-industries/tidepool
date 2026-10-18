recordSecond <- R.call (Probe.runModel (R.client recordProbe)) "second"
recordCalls <- R.call (Probe.readCalls (R.client recordProbe)) ()
display (recordCalls == 2 && case (Model.modelOutcome recordSecond, Model.modelReceipt recordFirst, Model.modelReceipt recordSecond) of { (Right (Probe.Answer 42), Just first, Just second) -> Model.invocationIdentity first /= Model.invocationIdentity second && Model.parentCellIdentity first == Model.parentCellIdentity second && Model.providerRequests (Model.invocationUsage second) == 1; _ -> False })
