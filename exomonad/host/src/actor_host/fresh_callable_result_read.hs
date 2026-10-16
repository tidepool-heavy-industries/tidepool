Right callableReceipt <- await (response callableResponse)
let callableValue = responseValue callableReceipt
again <- pollResponse callableResponse
let repeatedReceipt = case again of { ResponseReady receipt -> receipt; _ -> error "repeat response was not ready" }
display (case (callableValue, responseValue repeatedReceipt) of
  ((n, f), (m, g)) -> n == 42 && f 1 == 42 && f 10 == 51
    && m == 42 && g (-2) == 39
    && responseExecution callableReceipt == responseExecution repeatedReceipt
    && responseWorktree callableReceipt == responseWorktree repeatedReceipt)
