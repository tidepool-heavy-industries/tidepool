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
  R.send (storeSeed (R.client seedStore)) seed
  replies <- watch "same-cell-captured-replies" ((,) <$> awaitValue alpha <*> awaitValue beta)
  result <- awaitWatch replies
  case (inheritedRefusal, result) of
    (Left (UnfoldUncapturedContext _), Right (42, 42)) ->
      error "intentional captured parent Haskell execution failure" >> pure True
    _ -> error "captured same-cell reply contract failed" >> pure True
