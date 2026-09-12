let reviewLabel = "review" :: BranchLabel
let laterWave = "later-wave" :: ForkGroupLabel
:{
otherWorkers <- unfold (batch campaign laterWave) $
  (,) <$> child (withEffort Low (coding @Report domainLabel projectHead domainPlan))
      <*> child (researching @Review reviewLabel projectHead reviewPlan)
:}
