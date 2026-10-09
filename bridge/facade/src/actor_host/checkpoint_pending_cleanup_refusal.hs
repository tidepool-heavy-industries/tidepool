do
  before <- pollResponse producerRequest
  outcome <- forgetAgent (responseActor producerRequest)
  after <- pollResponse producerRequest
  case (before, outcome, after) of
    (ResponsePending _, AgentForgetRunning, ResponsePending _) -> display True
    _ -> display False
