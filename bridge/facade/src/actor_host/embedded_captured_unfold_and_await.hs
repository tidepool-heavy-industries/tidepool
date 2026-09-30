do
  inheritedRefusal <- attemptUnfoldCaptured (batch ("embedded-captured" :: CampaignLabel)
    ("inherited-refusal" :: ForkGroupLabel))
    (child (researching @Int projectHead
      (assignment [label|must-not-launch|] ("inherited context is not independent" :: Text))))
  Right seed <- checkpoint "same-cell captured context"
  (alpha, beta) <- unfoldCaptured (batch ("embedded-captured" :: CampaignLabel)
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
  releasedRefusal <- attemptUnfoldCaptured (batch ("embedded-captured" :: CampaignLabel)
    ("released-refusal" :: ForkGroupLabel))
    (child (withContext (fromCheckpoint seed)
      (researching @Int projectHead
        (assignment [label|released-must-not-launch|] ("released capture" :: Text)))))
  replies <- watch "same-cell-captured-replies" ((,) <$> awaitValue alpha <*> awaitValue beta)
  result <- awaitWatch replies
  case (inheritedRefusal, firstRelease, secondRelease, releasedRefusal, result) of
    (Left (UnfoldUncapturedContext _), Right (), Right (),
      Left (UnfoldCheckpointRefused ReleasedCheckpoint), Right (42, 42)) -> pure True
    _ -> error "captured same-cell reply contract failed" >> pure True
