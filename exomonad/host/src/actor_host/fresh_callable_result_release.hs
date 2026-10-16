import qualified Tidepool.Effects as Effects
forgotten <- forgetResponse callableResponse
_ <- case forgotten of { ResponseForgotten -> pure (); _ -> Effects.error "completed response was not forgotten" }
stale <- pollResponse callableResponse
_ <- case stale of { ResponseUnavailable (ResponseRejected ReplyStale) -> pure (); _ -> Effects.error "forgotten response was still observable" }
watchSnapshot <- pollWatch callableWatch
let watchReceipt = case watchSnapshot of { WatchReady receipt -> receipt; _ -> error "settled watch lost its original result" }
_ <- forgetWatch callableWatch
display (case (callableValue, responseValue watchReceipt) of
  ((n, f), (m, g)) -> n == 42 && f 100 == 141
    && m == 42 && g (-10) == 31
    && responseExecution callableReceipt == responseExecution watchReceipt
    && responseWorktree callableReceipt == responseWorktree watchReceipt)
