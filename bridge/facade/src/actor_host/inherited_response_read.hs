observed <- pollWatch inheritedWatch
let retainedReceipt = case observed of { WatchReady answer -> answer; _ -> error "inherited response did not become ready" }
let retained = responseValue retainedReceipt
Right retainedValue <- await (result worker)
Right (Right retainedSettledValue) <- await (settlement worker)
Right (Right retainedSettledReceipt) <- await (settledResponse worker)
original <- pollResponse worker
let originalReceipt = case original of { ResponseReady answer -> answer; _ -> error "original response did not become ready" }
_ <- if responseExecution retainedReceipt == responseExecution originalReceipt && responseWorktree retainedReceipt == responseWorktree originalReceipt && responseExecution retainedSettledReceipt == responseExecution originalReceipt && responseWorktree retainedSettledReceipt == responseWorktree originalReceipt then pure () else Effects.error "receipt projections changed original evidence"
