let reviewLabel = [label|review|]
let laterWave = "later-wave" :: ForkGroupLabel
otherWorkers <- unfold (batch campaign laterWave) $
  (,) <$> child (withEffort Low (coding @Report projectHead (assignment domainLabel domainPlan)))
      <*> child (researching @Review projectHead (assignment reviewLabel reviewPlan))
