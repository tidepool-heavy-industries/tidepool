import Tidepool.Actors.Exomonad

offlineWorkerAnswers <- await ((,) <$> settlement offlineEditRequest <*> settlement offlineTestRequest)
case offlineWorkerAnswers of
  Right (Right edited, Right tested) -> display (edited == "edited" && tested == "tests-written")
  _ -> display False
