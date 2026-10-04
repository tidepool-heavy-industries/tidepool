data M2JoinA = M2JoinA Int deriving Show
m2JoinA <- do
  Right seed <- checkpoint "original nominal join inputs"
  (alpha, beta) <- unfold (batch "embedded-nominal-join" "original-workers")
    ((,) <$>
      child (withLifetime ActorOwned (withContext (fromCheckpoint seed)
        (researching @M2Reply projectHead
          (m2OriginalAssignment [label|original-alpha|]))))
      <*> child (withLifetime ActorOwned (withContext (fromCheckpoint seed)
        (researching @M2Reply projectHead
          (m2OriginalAssignment [label|original-beta|])))))
  replies <- watch "original-nominal-replies" ((,) <$> awaitValue alpha <*> awaitValue beta)
  result <- awaitWatch replies
  Right () <- releaseCheckpoint seed
  case result of
    Right (M2Reply 43, M2Reply 43) -> pure (M2JoinA 43)
    _ -> error "original nominal replies changed during same-root publication"
pure True
