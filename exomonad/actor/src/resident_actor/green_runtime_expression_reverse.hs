_ <- do
  let awaitForm name original = do
        opened <- send (Tidepool.Effects.Core.FormOpenWith (toJSON name))
        lease <- case opened of
          Right value -> pure value
          Left _ -> Tidepool.Effects.Core.error "controlled form open failed"
        answered <- send (Tidepool.Effects.Core.FormAwaitWith lease)
        attempt <- case answered of
          Right (Tidepool.Effects.Core.FormSubmitted value _) -> pure value
          _ -> Tidepool.Effects.Core.error "controlled form answer failed"
        committed <- send (Tidepool.Effects.Core.FormCommitWith lease attempt (toJSON name))
        case committed of
          Right Tidepool.Effects.Core.FormApplied -> pure original
          _ -> Tidepool.Effects.Core.error "controlled form commit failed"
  (first, second) <- concurrently
    (awaitForm "one" (\value -> value + (11 :: Int)))
    (awaitForm "two" (\value -> value * (7 :: Int)))
  say (tshow (first 4 == 15 && second 4 == 28))
