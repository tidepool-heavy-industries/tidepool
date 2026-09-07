let Right campaign = campaignLabel "route-check"
let Right wave = forkGroupLabel "workers"
let Right producerLabel = branchLabel "producer"
let Right consumerLabel = branchLabel "consumer"
(producer, consumer) <- unfold (batch campaign wave) ((,) <$> child (coding @Text producerLabel projectHead ("candidate" :: Text)) <*> child (coding @Text consumerLabel projectHead ("reviewer ready" :: Text)))
let Right forwardedLabel = requestLabel "review-candidate"
forwarding <- route (awaitSettledFork producer) (\settlement -> case settlement of { ReplyAvailable answer -> do { _ <- requestWith @Text (forkedActor consumer) (requestOptions forwardedLabel (responseValue answer)); pure () }; ReplyUnavailable failure -> error (T.pack (show failure)) })
broken <- route (awaitSettledFork producer) (\_ -> error "deliberate route failure")
let Right reviewWave = forkGroupLabel "independent-review"
let Right reviewLabel = branchLabel "review"
reviewLaunch <- route (awaitSettledFork producer) (\settlement -> case settlement of { ReplyAvailable answer -> do { _ <- unfold (batch campaign reviewWave) (child (withContext (selected id) (withModel "gpt-5.6-sol" (coding @Text reviewLabel projectHead (responseValue answer))))); pure () }; ReplyUnavailable _ -> pure () })
