data LookupWorkerResult
  = LookupFinished Text
  | LookupFailed Text

summarizeLookup :: LookupWorkerResult -> Text
summarizeLookup result =
  case result of
    LookupFinished value -> value
    LookupFailed reason -> "failed: " <> reason

worker <- unfold (batch "lookup-acceptance" "workers") $
  child @LookupWorkerResult $
    coding projectHead $
      assignment "lookup-worker" ("respond with LookupFinished \"ready\"" :: Text)
worker
