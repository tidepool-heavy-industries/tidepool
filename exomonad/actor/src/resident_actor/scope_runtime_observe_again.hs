_ <- do
  let observe = do
        state <- Reply.pollResponse scopeNext
        case state of
          Reply.ResponseReady result -> pure (Reply.responseValue result == 91)
          Reply.ResponseUnavailable failure -> Effects.error ("second request failed: " <> tshow failure)
          _ -> sleep (milliseconds 1) >> observe
  observe >>= say . tshow
