capturedValue <- pure (x :: Int)
privateCapturedHelper :: Int -> Int
privateCapturedHelper value = value + 1
let capturedGetter = privateCapturedHelper capturedValue
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
      child (withLifetime ActorOwned (withContext (fromCheckpoint seed)
        (researching @Int projectHead
          (assignment [label|captured-alpha|] ("reply with the captured getter" :: Text)))))
      <*> child (withLifetime ActorOwned (withContext (fromCheckpoint seed)
        (researching @Int projectHead
          (assignment [label|captured-beta|] ("reply with the captured getter" :: Text))))))
  Just group <- pure (forkGroupHandle alpha)
  R.send (storeGroup (R.client groupStore)) group
  R.send (storeSeed (R.client seedStore)) seed
  replies <- watch "same-cell-captured-replies" ((,) <$> awaitValue alpha <*> awaitValue beta)
  result <- awaitWatch replies
  case (inheritedRefusal, deferredRefusal, result) of
    (Left (UnfoldUncapturedContext _), Left (UnfoldDeferredInvocationOwned _), Right (42, 42)) ->
      error "M2_INTENTIONAL_PARENT_EXECUTION_FAILURE" >> pure True
    _ -> display False
capturedSuffix <- pure (99 :: Int)
