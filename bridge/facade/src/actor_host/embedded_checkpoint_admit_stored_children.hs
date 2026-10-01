do
  Just seed <- R.call (readSeed (R.client seedStore)) ()
  (alpha, beta) <- unfold (batch ("embedded-checkpoint" :: CampaignLabel)
    ("checkpoint-readers" :: ForkGroupLabel))
    ( (,) <$> child (withLifetime ActorOwned (withContext (fromCheckpoint seed)
        (researching @Text projectHead
          (assignment [label|checkpoint-alpha|] ("read the captured Haskell context" :: Text)))))
        <*> child (withLifetime ActorOwned (withContext (fromCheckpoint seed)
        (researching @Text projectHead
          (assignment [label|checkpoint-beta|] ("read the captured Haskell context" :: Text)))))
    )
  firstRelease <- releaseCheckpoint seed
  secondRelease <- releaseCheckpoint seed
  refusal <- attemptUnfold (batch ("embedded-checkpoint" :: CampaignLabel)
    ("after-release" :: ForkGroupLabel))
    (child (withContext (fromCheckpoint seed)
      (researching @Text projectHead
        (assignment [label|after-release|] ("must not be admitted" :: Text)))))
  case (firstRelease, secondRelease, refusal) of
    (Right (), Right (), Left (UnfoldCheckpointRefused ReleasedCheckpoint)) -> pure True
    _ -> error "checkpoint release/refusal contract failed" >> pure True
