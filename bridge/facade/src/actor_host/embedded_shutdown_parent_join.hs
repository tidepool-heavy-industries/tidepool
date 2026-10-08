capturedValue <- pure (x :: Int)
let capturedGetter = capturedValue + 1
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
  Just group <- pure (forkGroupHandle alpha)
  R.send (storeGroup (R.client groupStore)) group
  firstRelease <- releaseCheckpoint seed
  secondRelease <- releaseCheckpoint seed
  releasedRefusal <- attemptUnfold (batch ("embedded-captured" :: CampaignLabel)
    ("released-refusal" :: ForkGroupLabel))
    (child (withContext (fromCheckpoint seed)
      (researching @Int projectHead
        (assignment [label|released-must-not-launch|] ("released capture" :: Text)))))
  case (inheritedRefusal, deferredRefusal, firstRelease, secondRelease, releasedRefusal) of
    (Left (UnfoldUncapturedContext _), Left (UnfoldDeferredInvocationOwned _), Right (), Right (),
      Left (UnfoldCheckpointRefused ReleasedCheckpoint)) -> pure ()
    _ -> error "HOSTED_SHUTDOWN_PARENT_PREFLIGHT_CONTRACT_FAILED"
  -- Failure is retained as one settlement; the applicative join still waits
  -- for the active sibling, leaving this original call owned until shutdown.
  replies <- watch "same-cell-captured-replies" ((,) <$> awaitSettled alpha <*> awaitSettled beta)
  _ <- awaitWatch replies
  error "HOSTED_SHUTDOWN_PARENT_JOIN_SETTLED_BEFORE_NATIVE_CANCELLATION" >> display True
