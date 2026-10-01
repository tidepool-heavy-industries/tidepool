let campaign = "my-project" :: CampaignLabel
let wave = "first-wave" :: ForkGroupLabel
let domainLabel = [label|domain|]
let consumerLabel = [label|consumer-tests|]
workers <- unfoldDeferred (batch campaign wave) $
  (,) <$> child (withLifetime ActorOwned (coding @Report projectHead (assignment domainLabel domainPlan)))
      <*> child (withLifetime ActorOwned (withEffort Medium (coding @Report projectHead (assignment consumerLabel consumerPlan))))
let sharedAfterUnfold = ("ready" :: Text)
