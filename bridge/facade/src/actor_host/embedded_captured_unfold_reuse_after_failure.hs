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
  case (result, firstRelease, secondRelease) of
    (Right 42, Right (), Right ()) -> display True
    _ -> error "retained failed-cell capture contract failed" >> display True
