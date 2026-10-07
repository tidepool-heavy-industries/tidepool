expired <- pollResponse worker
_ <- case expired of
  ResponseUnavailable (ResponseRejected ReplyStale) ->
    let settledValue = responseValue retainedSettledReceipt
    in if fst (fst retained) == "custody" && snd (fst retained) == [2 .. 401]
          && snd retained 41 == 42
          && fst retainedValue == fst retained && snd retainedValue 42 == 43
          && fst retainedSettledValue == fst retained && snd retainedSettledValue 43 == 44
          && fst settledValue == fst retained && snd settledValue 44 == 45
          && responseExecution retainedReceipt == responseExecution retainedSettledReceipt
          && responseWorktree retainedReceipt == responseWorktree retainedSettledReceipt
       then pure ()
       else Effects.error "released lazy result or receipt changed"
  _ -> Effects.error "released response did not refuse observation"
