let campaign = case campaignLabel "research-policy" of { Right value -> value; Left _ -> error "fixture label" }
let group = case forkGroupLabel "coordinator" of { Right value -> value; Left _ -> error "fixture label" }
let leafLabel = case branchLabel "researcher" of { Right value -> value; Left _ -> error "fixture label" }
worker <- unfold (batch campaign group) (child (researching @Text leafLabel projectHead ()))
