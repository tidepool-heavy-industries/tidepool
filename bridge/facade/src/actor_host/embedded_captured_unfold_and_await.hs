do
  inheritedRefusal <- attemptUnfold (batch ("embedded-captured" :: CampaignLabel)
    ("inherited-refusal" :: ForkGroupLabel))
    ((,) <$>
      child (withContext (selected id)
        (researching @Int projectHead
          (assignment [label|selected-must-not-launch|] ("selected context cannot bypass preflight" :: Text))))
      <*> child (researching @Int projectHead
        (assignment [label|must-not-launch|] ("inherited context is not independent" :: Text))))
  deferredRefusal <- attemptUnfoldDeferred (batch ("embedded-captured" :: CampaignLabel)
    ("deferred-refusal" :: ForkGroupLabel))
    ((,) <$>
      child (withLifetime ActorOwned (withContext (selected id)
        (researching @Int projectHead
          (assignment [label|deferred-actor-must-not-launch|] ("actor ownership cannot bypass preflight" :: Text)))))
      <*> child (researching @Int projectHead
        (assignment [label|deferred-invocation-must-not-launch|] ("invocation-owned deferred work cannot start" :: Text))))
  Right seed <- checkpoint "same-cell captured context"
  (alpha, beta) <- unfold (batch ("embedded-captured" :: CampaignLabel)
    ("reply-workers" :: ForkGroupLabel))
    ((,) <$>
      child (withContext (fromCheckpoint seed)
        (researching @Int projectHead
          (assignment [label|captured-alpha|] ("reply with the captured getter" :: Text))))
      <*> child (withContext (fromCheckpoint seed)
        (researching @Int projectHead
          (assignment [label|captured-beta|] ("reply with the captured getter" :: Text)))))
  firstRelease <- releaseCheckpoint seed
  secondRelease <- releaseCheckpoint seed
  releasedRefusal <- attemptUnfold (batch ("embedded-captured" :: CampaignLabel)
    ("released-refusal" :: ForkGroupLabel))
    (child (withContext (fromCheckpoint seed)
      (researching @Int projectHead
        (assignment [label|released-must-not-launch|] ("released capture" :: Text)))))
  replies <- watch "same-cell-captured-replies" ((,) <$> awaitValue alpha <*> awaitValue beta)
  result <- awaitWatch replies
  case (inheritedRefusal, deferredRefusal, firstRelease, secondRelease, releasedRefusal, result) of
    (Left (UnfoldUncapturedContext _), Left (UnfoldDeferredInvocationOwned _), Right (), Right (),
      Left (UnfoldCheckpointRefused ReleasedCheckpoint), Right (42, 42)) -> pure True
    _ -> error "captured same-cell reply contract failed" >> pure True
