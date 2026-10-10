Right callableReceipt <- await (response callableResponse)
let callableValue = responseValue callableReceipt
again <- pollResponse callableResponse
let repeatedReceipt = case again of { ResponseReady receipt -> receipt; _ -> error "repeat response was not ready" }
display (case (callableValue, responseValue repeatedReceipt) of
  ((n, f), (m, g)) -> (n, f 1, f 10, m, g (-2), responseExecution callableReceipt == responseExecution repeatedReceipt, responseWorktree callableReceipt == responseWorktree repeatedReceipt))
