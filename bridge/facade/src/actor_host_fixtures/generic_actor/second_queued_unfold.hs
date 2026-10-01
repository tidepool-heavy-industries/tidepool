let reviewLabel = [label|review|]
let laterWave = "later-wave" :: ForkGroupLabel
otherWorkers <- unfoldDeferred (batch campaign laterWave) $
  (,) <$> child (withLifetime ActorOwned (withEffort Low (coding @Report projectHead (assignment domainLabel domainPlan))))
      <*> child (withLifetime ActorOwned (researching @Review projectHead (assignment reviewLabel reviewPlan)))
