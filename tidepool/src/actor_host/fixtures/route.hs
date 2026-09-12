let campaign = "route-check" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let producerLabel = "producer" :: BranchLabel
let consumerLabel = "consumer" :: BranchLabel
(producer, consumer) <- unfold (batch campaign wave) ((,) <$> child (coding @Text producerLabel projectHead ("candidate" :: Text)) <*> child (coding @Text consumerLabel projectHead ("reviewer ready" :: Text)))
let forwardedLabel = "review-candidate" :: RequestLabel
forwarding <- route (awaitSettledFork producer) (\settlement -> case settlement of { ReplyAvailable answer -> do { _ <- requestWith @Text (forkedActor consumer) (requestOptions forwardedLabel (responseValue answer)); pure () }; ReplyUnavailable failure -> error (T.pack (show failure)) })
broken <- route (awaitSettledFork producer) (\_ -> error "deliberate route failure")
let reviewWave = "independent-review" :: ForkGroupLabel
let reviewLabel = "review" :: BranchLabel
reviewLaunch <- route (awaitSettledFork producer) (\settlement -> case settlement of { ReplyAvailable answer -> do { _ <- unfold (batch campaign reviewWave) (child (withContext (selected id) (withModel "gpt-5.6-sol" (coding @Text reviewLabel projectHead (responseValue answer))))); pure () }; ReplyUnavailable _ -> pure () })
