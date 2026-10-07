_ <- do
  let awaitForm name original = do
        opened <- send (FormOpenWith (toJSON name))
        lease <- case opened of
          Right value -> pure value
          Left _ -> Tidepool.Effects.Core.error "controlled form open failed"
        answered <- send (FormAwaitWith lease)
        attempt <- case answered of
          Right (FormSubmitted value _) -> pure value
          _ -> Tidepool.Effects.Core.error "controlled form answer failed"
        committed <- send (FormCommitWith lease attempt (toJSON name))
        case committed of
          Right FormApplied -> pure original
          _ -> Tidepool.Effects.Core.error "controlled form commit failed"
  selected <- awaitForm "single" (\value -> value + (19 :: Int))
  say (tshow (selected 4 == 23))
