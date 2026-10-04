do
  Just seed <- R.call (readSeed (R.client seedStore)) ()
  again <- unfold (batch ("embedded-captured" :: CampaignLabel)
    ("after-cell-failure" :: ForkGroupLabel))
    (child (withContext (fromCheckpoint seed)
      (researching @Int projectHead
        (assignment [label|capture-still-usable|] ("reply with the retained getter" :: Text)))))
  replies <- watch "reused-captured-reply" (awaitValue again)
  result <- awaitWatch replies
  firstRelease <- releaseCheckpoint seed
  secondRelease <- releaseCheckpoint seed
  refused <- attemptUnfold (batch "embedded-captured" "reuse-after-release")
    (child (withContext (fromCheckpoint seed)
      (researching @Int projectHead
        (assignment [label|reuse-released-must-not-launch|] ("released capture" :: Text)))))
  cleaned <- planCleanupFor again >>= executeCleanup
  case (result, firstRelease, secondRelease, refused, cleanupReceiptComplete cleaned) of
    (Right 42, Right (), Right (), Left (UnfoldCheckpointRefused ReleasedCheckpoint), True) -> display True
    _ -> error "retained failed-cell capture contract failed" >> display True
