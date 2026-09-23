let campaign = "route-check" :: CampaignLabel
let wave = "workers" :: ForkGroupLabel
let producerLabel = [label|producer|]
let consumerLabel = [label|consumer|]
(producer, consumer) <- unfold (batch campaign wave) ((,) <$> child (coding @Text projectHead (assignment producerLabel ("candidate" :: Text))) <*> child (coding @Text projectHead (assignment consumerLabel ("reviewer ready" :: Text))))
let forwardedLabel = [label|review-candidate|]
forwarding <- route (awaitSettled producer) (\settlement -> case settlement of { ReplyAvailable answer -> do { _ <- requestWith @Text (responseActor consumer) (assignment forwardedLabel (responseValue answer)); pure () }; ReplyUnavailable failure -> error (T.pack (show failure)) })
broken <- route (awaitSettled producer) (\_ -> error "deliberate route failure")
let reviewWave = "independent-review" :: ForkGroupLabel
let reviewLabel = [label|review|]
reviewLaunch <- route (awaitSettled producer) (\settlement -> case settlement of { ReplyAvailable answer -> do { _ <- unfold (batch campaign reviewWave) (child (withContext (selected id) (withModel (Literal "gpt-6-sol") (coding @Text projectHead (assignment reviewLabel (responseValue answer)))))); pure () }; ReplyUnavailable _ -> pure () })
