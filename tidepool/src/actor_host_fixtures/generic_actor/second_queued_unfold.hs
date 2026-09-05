let Right laterWave = forkGroupLabel "later-wave"
:{
otherWorkers <- unfold (batch campaign laterWave) $
  (,) <$> child (withEffort Low (coding @Report domainLabel projectHead domainPlan))
      <*> child (researching @Review reviewLabel projectHead reviewPlan)
:}
